//! The file-operation engine: streaming copy / move / delete across any VFS
//! backends, with progress reporting and cooperative cancellation.

use super::cancel::CancelToken;
use super::progress::{
    ConflictInfo, CopyAction, DeniedInfo, OverwriteDecision, OverwriteRule, PrivDecision,
    ProgressUpdate, TaskId, TaskOutcome, TaskReply,
};
use super::sync::SyncStep;
use super::{OpKind, OpRequest};
use crate::util::async_bridge::AppSender;
use crate::util::{Error, Result};
use crate::vfs::{BoxWrite, Vfs, VfsEntry, VfsKind, VfsPath, WriteMeta};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

const CHUNK: usize = 64 * 1024;
const EMIT_INTERVAL: Duration = Duration::from_millis(33);

/// Run an operation to completion, returning the outcome. Always emits a final
/// progress snapshot via the channel before returning.
pub async fn run(
    id: TaskId,
    req: OpRequest,
    tx: AppSender,
    cancel: CancelToken,
    reply_rx: mpsc::Receiver<TaskReply>,
) -> TaskOutcome {
    let verb = match req.kind {
        OpKind::Copy => "Copying",
        OpKind::Move => "Moving",
        OpKind::Delete => "Deleting",
        OpKind::Trash => "Trashing",
        OpKind::Sync => "Synchronizing",
    };
    let mut engine = Engine {
        id,
        verb,
        tx,
        cancel,
        reply_rx,
        // Start with an "overwrite all" policy when confirmation is disabled, so
        // conflicts resolve silently instead of prompting.
        policy: req.overwrite_all.then_some(OverwriteRule::All),
        skip_empty: false,
        priv_policy: None,
        files_total: 0,
        files_done: 0,
        total_total: 0,
        total_done: 0,
        file_total: 0,
        file_done: 0,
        current_name: String::new(),
        last_emit: Instant::now(),
    };

    match engine.execute(&req).await {
        Ok(()) => TaskOutcome::Done,
        Err(Error::Cancelled) => TaskOutcome::Cancelled,
        Err(e) => TaskOutcome::Failed(e.to_string()),
    }
}

struct Engine {
    id: TaskId,
    verb: &'static str,
    tx: AppSender,
    cancel: CancelToken,
    /// Receives the user's answer to an overwrite prompt.
    reply_rx: mpsc::Receiver<TaskReply>,
    /// Global overwrite rule once the user picks "...all files"; `None` = ask.
    policy: Option<OverwriteRule>,
    /// Never overwrite a destination with a zero-length source.
    skip_empty: bool,
    /// Standing answer to permission denials once the user picks an "...all"
    /// option; `None` = ask each time.
    priv_policy: Option<PrivDecision>,
    files_total: u64,
    files_done: u64,
    total_total: u64,
    total_done: u64,
    file_total: u64,
    file_done: u64,
    current_name: String,
    last_emit: Instant,
}

impl Engine {
    async fn execute(&mut self, req: &OpRequest) -> Result<()> {
        // A sync plan already knows its sizes, so it needs no pre-scan walk —
        // the totals come straight off the steps.
        if req.kind == OpKind::Sync {
            return self.execute_sync(req).await;
        }
        // Pre-scan to compute totals for the progress bars.
        for src in &req.sources {
            self.scan(&req.src_fs, src).await?;
        }
        self.emit(true);

        match req.kind {
            // Handled above; kept exhaustive so a new kind must be considered.
            OpKind::Sync => unreachable!("dispatched before the pre-scan"),
            OpKind::Delete => {
                for src in &req.sources {
                    self.check_cancel()?;
                    self.delete_tree(&req.src_fs, src.clone()).await?;
                }
            }
            OpKind::Trash => {
                for src in &req.sources {
                    self.check_cancel()?;
                    self.trash_tree(&req.src_fs, src.clone()).await?;
                }
            }
            OpKind::Copy | OpKind::Move => {
                let dst_fs = req
                    .dst_fs
                    .clone()
                    .ok_or_else(|| Error::other("destination backend missing"))?;
                let dst_dir = req
                    .dst_dir
                    .clone()
                    .ok_or_else(|| Error::other("destination directory missing"))?;
                let is_move = matches!(req.kind, OpKind::Move);
                let same_backend = Arc::ptr_eq(&req.src_fs, &dst_fs);

                for src in &req.sources {
                    self.check_cancel()?;
                    // A rename supplies the exact target name; otherwise the
                    // source keeps its own name inside the destination directory.
                    let dst = match &req.dst_name {
                        Some(name) => dst_dir.join(name),
                        None => dst_dir.join(src.file_name()),
                    };

                    // Refuse to copy/move something onto itself or into one of
                    // its own subdirectories (which would truncate the file or
                    // recurse forever). Skip the source entirely.
                    if same_backend && is_self_or_descendant(&dst, src) {
                        continue;
                    }

                    // Fast path: intra-backend move via rename. Skip it when the
                    // destination already exists so the copy path's overwrite
                    // prompt can run instead of silently clobbering/merging.
                    let dst_exists = dst_fs.stat(&dst).await.is_ok();
                    if is_move && same_backend && !dst_exists && dst_fs.capabilities().server_rename
                    {
                        // Count the subtree before it is renamed away so the
                        // progress counters stay consistent.
                        let mut files = 0u64;
                        let mut bytes = 0u64;
                        let _ = count_tree(&req.src_fs, src, &mut files, &mut bytes).await;
                        match req.src_fs.rename(src, &dst).await {
                            Ok(()) => {
                                self.files_done += files;
                                self.total_done += bytes;
                                self.emit(true);
                                continue;
                            }
                            Err(_) => { /* fall back to copy+delete (e.g. cross-device) */ }
                        }
                    }

                    let copied = self.copy_tree(&req.src_fs, src.clone(), &dst_fs, dst).await?;
                    // Only remove the source once it has actually been copied — a
                    // file skipped at an overwrite conflict must stay put (a
                    // partly-skipped directory keeps its whole source subtree).
                    if is_move && copied {
                        self.delete_tree(&req.src_fs, src.clone()).await?;
                    }
                }
            }
        }

        self.emit(true);
        Ok(())
    }

    /// Run a directory-sync plan. Each step names its own source and destination
    /// and the side it acts on, so a two-way plan copies in both directions
    /// within this one task — one progress bar, one Abort, one "To background".
    ///
    /// The steps arrive already ordered (clash-deletes, mkdirs, copies, then
    /// extraneous deletes — see [`sync::plan`](super::sync::plan)), so this just
    /// walks them in order and reuses the ordinary copy/delete paths, which brings
    /// the overwrite policy, cancellation and progress with them.
    async fn execute_sync(&mut self, req: &OpRequest) -> Result<()> {
        let dst_fs =
            req.dst_fs.clone().ok_or_else(|| Error::other("destination backend missing"))?;
        // Side 0 is the source panel's backend, side 1 the destination's.
        let fs_of = |side: usize| if side == 0 { req.src_fs.clone() } else { dst_fs.clone() };

        // Totals come from the plan: copies contribute their bytes, and a delete
        // contributes the files it will remove (`delete_tree` counts those as it
        // goes, so they must be in the total for the bar to reach 100%).
        for step in &req.steps {
            match step {
                SyncStep::Copy { size, .. } => {
                    self.files_total += 1;
                    self.total_total += size;
                }
                SyncStep::Delete { files, .. } => self.files_total += files,
                SyncStep::MkDir { .. } => {}
            }
        }
        self.emit(true);

        for step in &req.steps {
            self.check_cancel()?;
            match step {
                SyncStep::MkDir { side, path, .. } => {
                    // Best-effort: an existing directory is exactly what we want.
                    let _ = fs_of(*side).mkdir(path).await;
                }
                SyncStep::Copy { from, src, dst, rel, .. } => {
                    let (src_fs, dst_fs) = (fs_of(*from), fs_of(1 - *from));
                    self.current_name = rel.clone();
                    // The plan creates directories it knows about, but a parent
                    // can still be missing when the destination changed under us.
                    if let Some(parent) = dst.parent() {
                        let _ = dst_fs.mkdir(&parent).await;
                    }
                    let copied = self.copy_tree(&src_fs, src.clone(), &dst_fs, dst.clone()).await?;
                    // Give the copy its source's timestamp, so the next run sees
                    // the two as identical instead of copying it again forever
                    // (and, two-way, bouncing it back). Best-effort: a backend
                    // that can't set times just gets re-synced next time.
                    if copied
                        && let Ok(meta) = src_fs.stat(src).await
                        && let Some(mtime) = meta.mtime
                    {
                        let _ = dst_fs.set_mtime(dst, mtime).await;
                    }
                }
                SyncStep::Delete { side, path, .. } => {
                    self.delete_tree(&fs_of(*side), path.clone()).await?;
                }
            }
        }
        self.emit(true);
        Ok(())
    }

    /// Recursively accumulate file count and byte totals.
    fn scan<'a>(
        &'a mut self,
        fs: &'a Arc<dyn Vfs>,
        path: &'a VfsPath,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let entry = fs.stat(path).await?;
            match entry.kind {
                VfsKind::Dir => {
                    for child in fs.read_dir(path).await? {
                        let cp = path.join(&child.name);
                        self.scan(fs, &cp).await?;
                    }
                }
                _ => {
                    self.files_total += 1;
                    self.total_total += entry.size;
                }
            }
            Ok(())
        })
    }

    /// Copy `src` onto `dst` (recursively for a directory). Returns whether the
    /// whole subtree was actually written to the destination — `false` if any
    /// file was skipped at an overwrite conflict. A caller performing a *move*
    /// must only delete the source when this is `true`, so a skipped file is
    /// never removed without a copy having been made.
    fn copy_tree<'a>(
        &'a mut self,
        src_fs: &'a Arc<dyn Vfs>,
        src: VfsPath,
        dst_fs: &'a Arc<dyn Vfs>,
        dst: VfsPath,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            self.check_cancel()?;
            let entry = src_fs.stat(&src).await?;
            match entry.kind {
                VfsKind::Dir => {
                    // Create destination directory (ignore "already exists").
                    // A permission denial here is worth asking about, though:
                    // otherwise every file inside would ask separately.
                    if let Err(e) = dst_fs.mkdir(&dst).await
                        && e.is_permission_denied()
                        && dst_fs.stat(&dst).await.is_err()
                    {
                        let escalatable = Self::escalatable(&[&dst]);
                        if self.resolve_denied(&dst, escalatable).await? {
                            crate::priv_ops::mkdir(dst.as_path()).await?;
                        }
                    }
                    // The directory is fully copied only if every child was.
                    let mut all_copied = true;
                    for child in src_fs.read_dir(&src).await? {
                        let cs = src.join(&child.name);
                        let cd = dst.join(&child.name);
                        all_copied &= self.copy_tree(src_fs, cs, dst_fs, cd).await?;
                    }
                    Ok(all_copied)
                }
                VfsKind::Symlink => {
                    // Recreate the link if the destination backend supports it;
                    // only report success (safe to remove the source) if it took.
                    let recreated = match entry.symlink_target {
                        Some(target) => dst_fs.symlink(&target, &dst).await.is_ok(),
                        None => false,
                    };
                    self.files_done += 1;
                    self.emit(true);
                    Ok(recreated)
                }
                _ => {
                    // Resolve a conflict if the destination file already exists.
                    let action = match dst_fs.stat(&dst).await {
                        Ok(dst_entry) => {
                            self.resolve_conflict(&src, &entry, &dst, &dst_entry).await?
                        }
                        Err(_) => CopyAction::Overwrite, // no existing file
                    };
                    match action {
                        CopyAction::Skip => {
                            // Count it as handled so the totals stay consistent,
                            // but report that nothing was written (don't delete it).
                            self.files_done += 1;
                            self.total_done += entry.size;
                            self.emit(true);
                            Ok(false)
                        }
                        CopyAction::Overwrite => {
                            self.copy_file(
                                src_fs,
                                &src,
                                dst_fs,
                                &dst,
                                entry.size,
                                entry.mode,
                                entry.mtime,
                                false,
                            )
                            .await?;
                            Ok(true)
                        }
                        CopyAction::Append => {
                            self.copy_file(
                                src_fs,
                                &src,
                                dst_fs,
                                &dst,
                                entry.size,
                                entry.mode,
                                entry.mtime,
                                true,
                            )
                            .await?;
                            Ok(true)
                        }
                    }
                }
            }
        })
    }

    /// Ask the UI (or apply the standing policy) how to handle an existing
    /// destination file. Returns `Err(Cancelled)` if the user aborts.
    async fn resolve_conflict(
        &mut self,
        src: &VfsPath,
        new: &VfsEntry,
        dst: &VfsPath,
        old: &VfsEntry,
    ) -> Result<CopyAction> {
        let decide = |rule: OverwriteRule, skip_empty: bool| -> CopyAction {
            if skip_empty && new.size == 0 {
                CopyAction::Skip
            } else if rule.should_overwrite(new.size, new.mtime, old.size, old.mtime) {
                CopyAction::Overwrite
            } else {
                CopyAction::Skip
            }
        };

        if self.skip_empty && new.size == 0 {
            return Ok(CopyAction::Skip);
        }
        if let Some(rule) = self.policy {
            return Ok(decide(rule, self.skip_empty));
        }

        // Pause and ask the UI.
        let info = ConflictInfo {
            id: self.id,
            name: src.file_name(),
            new_path: src.display(),
            new_size: new.size,
            new_mtime: new.mtime,
            old_path: dst.display(),
            old_size: old.size,
            old_mtime: old.mtime,
        };
        if self.tx.send(crate::app::event::AppEvent::Conflict(info)).await.is_err() {
            return Err(Error::Cancelled);
        }
        match self.reply_rx.recv().await {
            Some(TaskReply::Overwrite(OverwriteDecision::OverwriteOnce)) => {
                Ok(CopyAction::Overwrite)
            }
            Some(TaskReply::Overwrite(OverwriteDecision::SkipOnce)) => Ok(CopyAction::Skip),
            Some(TaskReply::Overwrite(OverwriteDecision::AppendOnce)) => Ok(CopyAction::Append),
            Some(TaskReply::Overwrite(OverwriteDecision::Policy { rule, skip_empty })) => {
                self.policy = Some(rule);
                self.skip_empty |= skip_empty;
                Ok(decide(rule, self.skip_empty))
            }
            // An answer to a different question means the UI and the engine are
            // out of step; abort rather than guess.
            Some(TaskReply::Overwrite(OverwriteDecision::Abort))
            | Some(TaskReply::Privilege(_))
            | None => Err(Error::Cancelled),
        }
    }

    /// Ask the UI what to do about a permission-denied step, or apply the
    /// standing policy. Returns `true` when the caller should retry the step
    /// with elevated privileges, `false` to skip it.
    ///
    /// This reuses the same pause-and-ask channel as the overwrite prompt, so a
    /// denial stops the operation *at that file* rather than failing the whole
    /// run — which also gives the engine the per-file skip it never had.
    async fn resolve_denied(&mut self, path: &VfsPath, escalatable: bool) -> Result<bool> {
        if let Some(policy) = self.priv_policy {
            return match policy {
                PrivDecision::EscalateAll => Ok(escalatable),
                PrivDecision::SkipAll => Ok(false),
                // The one-shot answers are never stored as a policy.
                _ => Ok(false),
            };
        }

        let info = DeniedInfo { id: self.id, path: path.display(), verb: self.verb, escalatable };
        if self.tx.send(crate::app::event::AppEvent::PermissionDenied(info)).await.is_err() {
            return Err(Error::Cancelled);
        }
        match self.reply_rx.recv().await {
            Some(TaskReply::Privilege(PrivDecision::Escalate)) => Ok(escalatable),
            Some(TaskReply::Privilege(PrivDecision::Skip)) => Ok(false),
            Some(TaskReply::Privilege(PrivDecision::EscalateAll)) => {
                self.priv_policy = Some(PrivDecision::EscalateAll);
                Ok(escalatable)
            }
            Some(TaskReply::Privilege(PrivDecision::SkipAll)) => {
                self.priv_policy = Some(PrivDecision::SkipAll);
                Ok(false)
            }
            Some(TaskReply::Privilege(PrivDecision::Abort))
            | Some(TaskReply::Overwrite(_))
            | None => Err(Error::Cancelled),
        }
    }

    /// Deal with a failed delete step: pass non-permission errors straight
    /// through, and for a denial ask whether to retry it as root or skip it.
    async fn handle_denied(&mut self, e: &Error, path: &VfsPath, is_dir: bool) -> Result<()> {
        if !e.is_permission_denied() {
            return Err(Error::other(e.to_string()));
        }
        let escalatable = Self::escalatable(&[path]);
        if self.resolve_denied(path, escalatable).await? {
            if is_dir {
                crate::priv_ops::remove_dir(path.as_path()).await?;
            } else {
                crate::priv_ops::remove_file(path.as_path()).await?;
            }
        }
        Ok(())
    }

    /// Whether a failure on these paths is one `sudo` could actually fix.
    /// Only real local files qualify: no amount of privilege changes what an
    /// SFTP server or a `.zip` will let us do.
    fn escalatable(paths: &[&VfsPath]) -> bool {
        cfg!(unix) && paths.iter().all(|p| p.is_plain_local())
    }

    /// Copy one file, and if the filesystem refuses on permissions, ask the user
    /// whether to retry it as root, skip it, or abort.
    ///
    /// Skipping still counts the file towards the totals, so the progress bar
    /// stays honest and the operation runs to the end instead of dying on the
    /// first unreadable file the way it used to.
    #[allow(clippy::too_many_arguments)]
    async fn copy_file(
        &mut self,
        src_fs: &Arc<dyn Vfs>,
        src: &VfsPath,
        dst_fs: &Arc<dyn Vfs>,
        dst: &VfsPath,
        size: u64,
        mode: Option<u32>,
        mtime: Option<std::time::SystemTime>,
        append: bool,
    ) -> Result<()> {
        let attempt =
            self.copy_file_inner(src_fs, src, dst_fs, dst, size, mode, mtime, append).await;
        let Err(e) = attempt else {
            return attempt;
        };
        if !e.is_permission_denied() {
            return Err(e);
        }
        let escalatable = Self::escalatable(&[src, dst]);
        if self.resolve_denied(dst, escalatable).await? {
            crate::priv_ops::copy_file(src.as_path(), dst.as_path()).await?;
        }
        // Escalated or skipped, this file is dealt with either way.
        self.files_done += 1;
        self.total_done += size.saturating_sub(self.file_done);
        self.file_done = 0;
        self.emit(true);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn copy_file_inner(
        &mut self,
        src_fs: &Arc<dyn Vfs>,
        src: &VfsPath,
        dst_fs: &Arc<dyn Vfs>,
        dst: &VfsPath,
        size: u64,
        mode: Option<u32>,
        mtime: Option<std::time::SystemTime>,
        append: bool,
    ) -> Result<()> {
        self.current_name = src.file_name();
        self.file_total = size;
        self.file_done = 0;
        self.emit(true);

        let mut reader = src_fs.open_read(src).await?;
        let meta = WriteMeta { size_hint: Some(size), mode, mtime: None, append };
        let mut writer = match dst_fs.open_write(dst, meta).await {
            Ok(w) => w,
            Err(e) => return Err(e),
        };

        let mut buf = vec![0u8; CHUNK];
        loop {
            if self.cancel.is_cancelled() {
                discard_partial(writer, dst_fs, dst, append).await;
                return Err(Error::Cancelled);
            }
            // A read failure mid-copy (e.g. a dropped remote read) must not leave a
            // truncated destination behind, same as the cancel/write paths.
            let n = match reader.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    discard_partial(writer, dst_fs, dst, append).await;
                    return Err(e.into());
                }
            };
            if n == 0 {
                break;
            }
            if let Err(e) = writer.write_all(&buf[..n]).await {
                discard_partial(writer, dst_fs, dst, append).await;
                return Err(e.into());
            }
            self.file_done += n as u64;
            self.total_done += n as u64;
            self.emit(false);
        }
        // shutdown() flushes AND closes — required so remote/buffering writers
        // (SFTP File, FTP/SCP CollectWriter) actually finalize the upload. A
        // failure here means the upload didn't commit, so drop the partial too.
        if let Err(e) = writer.shutdown().await {
            discard_partial(writer, dst_fs, dst, append).await;
            return Err(e.into());
        }
        drop(writer);

        // Best-effort: give the copy its source's timestamp, like `cp -p`.
        // Appending is excluded: the destination genuinely changed just now.
        // Done *before* the chmod below, because a read-only source mode would
        // otherwise stop a backend that needs a write-open to set the time.
        if let (Some(t), false) = (mtime, append) {
            let _ = dst_fs.set_mtime(dst, t).await;
        }

        // Best-effort: preserve permissions.
        if let Some(m) = mode {
            let _ = dst_fs.set_permissions(dst, m).await;
        }

        self.files_done += 1;
        self.emit(true);
        Ok(())
    }

    fn delete_tree<'a>(
        &'a mut self,
        fs: &'a Arc<dyn Vfs>,
        path: VfsPath,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.check_cancel()?;
            let entry = fs.stat(&path).await?;
            match entry.kind {
                VfsKind::Dir => {
                    for child in fs.read_dir(&path).await? {
                        let cp = path.join(&child.name);
                        self.delete_tree(fs, cp).await?;
                    }
                    if let Err(e) = fs.remove_dir(&path).await {
                        // A directory whose children were skipped is simply not
                        // empty; report the refusal the same way either way.
                        self.handle_denied(&e, &path, true).await?;
                    }
                }
                _ => {
                    self.current_name = path.file_name();
                    if let Err(e) = fs.remove_file(&path).await {
                        self.handle_denied(&e, &path, false).await?;
                    }
                    self.files_done += 1;
                    self.emit(false);
                }
            }
            Ok(())
        })
    }

    /// Move one source into the freedesktop trash.
    ///
    /// Same filesystem is the normal case and is a plain rename: instant, and it
    /// preserves everything (permissions, timestamps, hard links, the lot).
    /// Across filesystems a rename is impossible, so it degrades to the ordinary
    /// copy + delete — which is exactly why this lives in the engine and not in
    /// the app layer: trashing a multi-gigabyte directory off a USB stick then
    /// still shows progress and can be cancelled.
    async fn trash_tree(&mut self, fs: &Arc<dyn Vfs>, path: VfsPath) -> Result<()> {
        self.check_cancel()?;
        self.current_name = path.file_name();
        self.emit(true);

        // Reserving writes the `.trashinfo` first, which claims the name.
        let plan = crate::trash::reserve(path.as_path())?;
        let dest = VfsPath::local(&plan.dest);

        if plan.same_device {
            // Count the subtree *before* the rename moves it out of reach, so
            // the progress totals stay honest for a one-step move.
            let mut files = 0u64;
            let mut bytes = 0u64;
            let _ = count_tree(fs, &path, &mut files, &mut bytes).await;
            // A failure here falls through to copy+delete: `same_device` is a
            // best guess and a rename can still fail across a bind mount.
            if fs.rename(&path, &dest).await.is_ok() {
                self.files_done += files.max(1);
                self.total_done += bytes;
                self.emit(true);
                return Ok(());
            }
        }

        let copied = self.copy_tree(fs, path.clone(), fs, dest).await;
        match copied {
            Ok(true) => {
                // Only unlink the original once it is safely in the trash.
                self.delete_tree(fs, path).await?;
                Ok(())
            }
            Ok(false) => {
                // Nothing was written (everything skipped): leave the source be.
                crate::trash::rollback(&plan);
                Ok(())
            }
            Err(e) => {
                crate::trash::rollback(&plan);
                Err(e)
            }
        }
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_cancelled() { Err(Error::Cancelled) } else { Ok(()) }
    }

    /// Emit a progress snapshot, throttled unless `force`.
    fn emit(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_emit) < EMIT_INTERVAL {
            return;
        }
        self.last_emit = now;
        let update = ProgressUpdate {
            id: self.id,
            verb: self.verb,
            current_name: self.current_name.clone(),
            file_done: self.file_done,
            file_total: self.file_total,
            total_done: self.total_done,
            total_total: self.total_total,
            files_done: self.files_done,
            files_total: self.files_total,
        };
        // Best-effort; if the channel is full we simply drop this frame.
        let _ = self.tx.try_send(crate::app::event::AppEvent::Progress(update));
    }
}

/// Drop a half-written destination after a cancelled or failed copy. The writer
/// is closed first (so a remote pipe releases), then the file is removed — but
/// **only when we created it**: in append mode the destination pre-existed and
/// holds the user's data, so removing it would destroy exactly what we were
/// adding to. Removal is best-effort; a leftover partial is preferable to
/// masking the original error.
async fn discard_partial(writer: BoxWrite, dst_fs: &Arc<dyn Vfs>, dst: &VfsPath, append: bool) {
    drop(writer);
    // A backend whose writer commits atomically on shutdown has written nothing
    // yet, so there is no partial destination to discard — and removing it would
    // delete the file that was already there and never got overwritten.
    if !append && !dst_fs.capabilities().atomic_write {
        let _ = dst_fs.remove_file(dst).await;
    }
}

/// Whether `dst` is `src` itself, or a path inside `src` (same backend). Used to
/// reject copying/moving a file or directory onto/into itself.
fn is_self_or_descendant(dst: &VfsPath, src: &VfsPath) -> bool {
    dst.scheme == src.scheme && dst.container == src.container && dst.path.starts_with(&src.path)
}

/// Count files/bytes of a (possibly already-moved) subtree. Used only to keep
/// the progress counters consistent after a fast-path rename. Errors are
/// swallowed because the source may no longer exist.
fn count_tree<'a>(
    fs: &'a Arc<dyn Vfs>,
    path: &'a VfsPath,
    files: &'a mut u64,
    bytes: &'a mut u64,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let entry = fs.stat(path).await?;
        if entry.kind == VfsKind::Dir {
            for child in fs.read_dir(path).await? {
                let cp = path.join(&child.name);
                count_tree(fs, &cp, files, bytes).await?;
            }
        } else {
            *files += 1;
            *bytes += entry.size;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OpKind, OpRequest};
    use crate::trash::test_home::TempHome;
    use crate::util::async_bridge;
    use crate::vfs::local::LocalFs;
    use std::path::PathBuf;

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rc_test_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn copies_a_directory_tree() {
        let root = unique_dir("copy");
        let src = root.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello world").unwrap();
        std::fs::write(src.join("sub/b.bin"), vec![7u8; 5000]).unwrap();
        let dst_dir = root.join("dest");

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        // The app would create dst_dir first; mirror that here.
        std::fs::create_dir_all(&dst_dir).unwrap();

        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        assert_eq!(std::fs::read(dst_dir.join("src/a.txt")).unwrap(), b"hello world");
        assert_eq!(std::fs::read(dst_dir.join("src/sub/b.bin")).unwrap().len(), 5000);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A full mirror round trip: walk both trees, plan, and execute the plan
    /// through the engine. Exercises every step kind against a real filesystem.
    #[tokio::test]
    async fn sync_mirrors_a_tree_and_prunes_extraneous_files() {
        use crate::ops::sync::{self, SyncMode};
        let root = unique_dir("sync");
        let a = root.join("a");
        let b = root.join("b");
        // Source: a changed file, a new nested file, and an empty directory.
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(a.join("empty")).unwrap();
        std::fs::write(a.join("same.txt"), b"same").unwrap();
        std::fs::write(a.join("changed.txt"), b"NEW CONTENT").unwrap();
        std::fs::write(a.join("sub/new.bin"), vec![3u8; 4096]).unwrap();
        // Backdate the sources well beyond the mtime tolerance. This is what makes
        // the idempotency check below meaningful: freshly-written files would be
        // "unchanged" simply because everything happened within a second, hiding a
        // mirror that re-copies its whole tree on every run.
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        for f in ["same.txt", "changed.txt", "sub/new.bin"] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(a.join(f))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        // Destination: one identical file, one stale, plus junk to be removed.
        std::fs::create_dir_all(b.join("junkdir")).unwrap();
        std::fs::write(b.join("same.txt"), b"same").unwrap();
        std::fs::write(b.join("changed.txt"), b"old").unwrap();
        std::fs::write(b.join("extra.txt"), b"remove me").unwrap();
        std::fs::write(b.join("junkdir/deep.txt"), b"also gone").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (pa, pb) = (VfsPath::local(&a), VfsPath::local(&b));
        let ta = sync::walk(&fs, &pa).await.unwrap();
        let tb = sync::walk(&fs, &pb).await.unwrap();
        let steps = sync::plan(&ta, &tb, [&pa, &pb], SyncMode::OneWay { delete_extraneous: true });
        assert!(!steps.is_empty(), "the trees differ, so there is work to do");

        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Sync,
            src_fs: fs.clone(),
            sources: Vec::new(),
            dst_fs: Some(fs.clone()),
            dst_dir: None,
            dst_name: None,
            overwrite_all: true, // a mirror overwrites by definition
            steps,
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(11, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        // The destination now matches the source exactly.
        assert_eq!(std::fs::read(b.join("changed.txt")).unwrap(), b"NEW CONTENT");
        assert_eq!(std::fs::read(b.join("sub/new.bin")).unwrap().len(), 4096);
        assert_eq!(std::fs::read(b.join("same.txt")).unwrap(), b"same");
        assert!(b.join("empty").is_dir(), "an empty source directory is mirrored");
        assert!(!b.join("extra.txt").exists(), "the extraneous file is gone");
        assert!(!b.join("junkdir").exists(), "the extraneous directory is gone");

        // The copies carry their source's timestamp, which is what lets the mirror
        // converge rather than re-copying everything next time.
        assert_eq!(
            std::fs::metadata(b.join("changed.txt")).unwrap().modified().unwrap(),
            old,
            "a copied file keeps the source's mtime"
        );
        // Re-planning the now-identical trees finds nothing left to do.
        let ta = sync::walk(&fs, &pa).await.unwrap();
        let tb = sync::walk(&fs, &pb).await.unwrap();
        let again = sync::plan(&ta, &tb, [&pa, &pb], SyncMode::OneWay { delete_extraneous: true });
        assert!(again.is_empty(), "a second run is a no-op: {again:?}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn two_way_sync_moves_each_file_to_the_side_that_lacks_it() {
        use crate::ops::sync::{self, SyncMode};
        let root = unique_dir("sync2");
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("from_a.txt"), b"AAA").unwrap();
        std::fs::write(b.join("from_b.txt"), b"BBB").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (pa, pb) = (VfsPath::local(&a), VfsPath::local(&b));
        let ta = sync::walk(&fs, &pa).await.unwrap();
        let tb = sync::walk(&fs, &pb).await.unwrap();
        let steps = sync::plan(&ta, &tb, [&pa, &pb], SyncMode::TwoWay);

        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Sync,
            src_fs: fs.clone(),
            sources: Vec::new(),
            dst_fs: Some(fs.clone()),
            dst_dir: None,
            dst_name: None,
            overwrite_all: true,
            steps,
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(12, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        // Both sides now hold both files — copied in opposite directions by the
        // one task, and nothing was deleted.
        assert_eq!(std::fs::read(b.join("from_a.txt")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(a.join("from_b.txt")).unwrap(), b"BBB");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Copy a file over an existing destination, answering the overwrite prompt
    /// with `decision`; returns the destination's resulting bytes.
    async fn copy_with_conflict(decision: OverwriteDecision) -> Vec<u8> {
        use crate::app::event::AppEvent;
        let root = unique_dir("ow");
        let src = root.join("f.txt");
        std::fs::write(&src, b"NEWDATA").unwrap();
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();
        std::fs::write(dst_dir.join("f.txt"), b"OLD").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, mut rx) = async_bridge::channel();
        let (reply_tx, reply_rx) = mpsc::channel(1);
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let handle = tokio::spawn(run(7, req, tx, CancelToken::new(), reply_rx));

        // Drain progress until the conflict prompt arrives, then answer it.
        while let Some(ev) = rx.recv().await {
            if let AppEvent::Conflict(info) = ev {
                assert_eq!(info.name, "f.txt");
                assert_eq!(info.old_size, 3);
                assert_eq!(info.new_size, 7);
                reply_tx.send(TaskReply::Overwrite(decision)).await.unwrap();
                break;
            }
        }
        let _ = handle.await.unwrap();
        let bytes = std::fs::read(dst_dir.join("f.txt")).unwrap();
        std::fs::remove_dir_all(&root).ok();
        bytes
    }

    #[tokio::test]
    async fn overwrite_decision_replaces_destination() {
        assert_eq!(copy_with_conflict(OverwriteDecision::OverwriteOnce).await, b"NEWDATA");
    }

    #[tokio::test]
    async fn skip_decision_keeps_destination() {
        assert_eq!(copy_with_conflict(OverwriteDecision::SkipOnce).await, b"OLD");
    }

    #[tokio::test]
    async fn append_decision_appends_to_destination() {
        assert_eq!(copy_with_conflict(OverwriteDecision::AppendOnce).await, b"OLDNEWDATA");
    }

    #[tokio::test]
    async fn refuses_copy_and_move_onto_itself() {
        for kind in [OpKind::Copy, OpKind::Move] {
            let root = unique_dir("self");
            let file = root.join("f.txt");
            std::fs::write(&file, b"DATA").unwrap();

            let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
            let (tx, _rx) = async_bridge::channel();
            let (_reply_tx, reply_rx) = mpsc::channel(1);
            // Destination directory is the file's own directory → dst == src.
            let req = OpRequest {
                kind,
                src_fs: fs.clone(),
                sources: vec![VfsPath::local(&file)],
                dst_fs: Some(fs.clone()),
                dst_dir: Some(VfsPath::local(&root)),
                dst_name: None,
                overwrite_all: false,
                steps: Vec::new(),
            };
            let outcome = run(9, req, tx, CancelToken::new(), reply_rx).await;
            assert!(matches!(outcome, TaskOutcome::Done), "{kind:?}: {outcome:?}");
            // The file must be untouched (not truncated, not deleted).
            assert_eq!(std::fs::read(&file).unwrap(), b"DATA", "{kind:?} left file intact");

            std::fs::remove_dir_all(&root).ok();
        }
    }

    #[tokio::test]
    async fn refuses_copy_dir_into_itself() {
        let root = unique_dir("selfdir");
        let dir = root.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), b"x").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        // Copy d into d → dst d/d would be a descendant of the source.
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&dir)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let outcome = run(10, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "{outcome:?}");
        assert!(!dir.join("d").exists(), "must not recurse into itself");

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn deletes_a_directory_tree() {
        let root = unique_dir("del");
        let victim = root.join("victim");
        std::fs::create_dir_all(victim.join("inner")).unwrap();
        std::fs::write(victim.join("inner/x"), b"x").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Delete,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&victim)],
            dst_fs: None,
            dst_dir: None,
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(2, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done));
        assert!(!victim.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// A plain copy carries the source's timestamp across, like `cp -p`, so a
    /// copied file keeps its date instead of being stamped "now".
    #[tokio::test]
    async fn copy_preserves_source_mtime() {
        let root = unique_dir("copy_mtime");
        let src = root.join("a.txt");
        std::fs::write(&src, b"dated").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        let got = std::fs::metadata(dst_dir.join("a.txt")).unwrap().modified().unwrap();
        assert_eq!(got, old, "the copy kept the source's mtime");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Trashing on the same filesystem is a rename into the trash's `files/`,
    /// with a matching `.trashinfo` recording where it came from.
    #[tokio::test]
    async fn trash_moves_the_file_and_writes_its_info_sidecar() {
        // Point the trash at a temp HOME so the real ~/.local/share/Trash is
        // never touched, and the source shares its filesystem (so it renames).
        let root = unique_dir("trash_op");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _guard = TempHome::set(&home);

        let src = root.join("doomed.txt");
        std::fs::write(&src, b"bye").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Trash,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: None,
            dst_dir: None,
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        assert!(!src.exists(), "the original is gone from the panel's directory");
        let trash = home.join(".local/share/Trash");
        let dest = trash.join("files/doomed.txt");
        assert!(dest.is_file(), "it is in the trash");
        assert_eq!(std::fs::read(&dest).unwrap(), b"bye", "contents intact");

        let info = std::fs::read_to_string(trash.join("info/doomed.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\n"), "{info}");
        assert!(info.contains(&format!("Path={}", src.display())), "records the origin: {info}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Escalation is only offered where it could actually work. `sudo` has no
    /// bearing on what an SFTP server or a `.zip` will allow, so offering it
    /// there would be a lie.
    #[test]
    fn only_plain_local_paths_are_escalatable() {
        let local = VfsPath::local("/etc/hosts");
        let remote =
            VfsPath { scheme: "sftp-0".into(), path: "/etc/hosts".into(), container: None };
        let in_archive = VfsPath {
            scheme: "archive".into(),
            path: "/inside.txt".into(),
            container: Some("/home/u/a.zip".into()),
        };

        assert_eq!(Engine::escalatable(&[&local]), cfg!(unix));
        assert!(!Engine::escalatable(&[&remote]));
        assert!(!Engine::escalatable(&[&in_archive]));
        // A mixed copy (local -> remote) is not escalatable either: the half
        // that failed may well be the remote one.
        assert!(!Engine::escalatable(&[&local, &remote]));
    }

    /// A copy that hits an unreadable file must pause and ask, not die — and
    /// answering "Skip" must let the rest of the operation finish. Before this,
    /// the first `EACCES` failed the whole run and left everything after it
    /// uncopied.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_denied_file_asks_and_skip_lets_the_rest_finish() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::Uid::effective().is_root() {
            eprintln!("skipping: root can read anything");
            return;
        }
        let root = unique_dir("denied_skip");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // Three files, the middle one unreadable.
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("b.txt"), b"b").unwrap();
        std::fs::write(src.join("c.txt"), b"c").unwrap();
        std::fs::set_permissions(src.join("b.txt"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, mut rx) = async_bridge::channel();
        let (reply_tx, reply_rx) = mpsc::channel(4);
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };

        // Answer every denial with Skip, as the UI would.
        let answerer = tokio::spawn(async move {
            let mut asked = 0;
            while let Some(ev) = rx.recv().await {
                if let crate::app::event::AppEvent::PermissionDenied(info) = ev {
                    asked += 1;
                    assert!(info.escalatable, "a local file is escalatable");
                    assert!(
                        info.path.contains("b.txt"),
                        "asked about the right file: {}",
                        info.path
                    );
                    let _ = reply_tx.send(TaskReply::Privilege(PrivDecision::Skip)).await;
                }
            }
            asked
        });

        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "the op completes: {outcome:?}");
        let asked = answerer.await.unwrap();
        assert_eq!(asked, 1, "asked exactly once, about the one denied file");

        // The readable files either side of it made it across.
        assert_eq!(std::fs::read(dst_dir.join("src/a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(dst_dir.join("src/c.txt")).unwrap(), b"c");
        assert!(!dst_dir.join("src/b.txt").exists(), "the skipped file was not created");

        let _ = std::fs::set_permissions(src.join("b.txt"), std::fs::Permissions::from_mode(0o644));
        std::fs::remove_dir_all(&root).ok();
    }

    /// "Skip all" answers once for a whole tree of denials.
    #[cfg(unix)]
    #[tokio::test]
    async fn skip_all_answers_once_for_every_later_denial() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::Uid::effective().is_root() {
            return;
        }
        let root = unique_dir("denied_skipall");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(src.join(n), b"x").unwrap();
            std::fs::set_permissions(src.join(n), std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, mut rx) = async_bridge::channel();
        let (reply_tx, reply_rx) = mpsc::channel(4);
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let answerer = tokio::spawn(async move {
            let mut asked = 0;
            while let Some(ev) = rx.recv().await {
                if matches!(ev, crate::app::event::AppEvent::PermissionDenied(_)) {
                    asked += 1;
                    let _ = reply_tx.send(TaskReply::Privilege(PrivDecision::SkipAll)).await;
                }
            }
            asked
        });

        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "{outcome:?}");
        assert_eq!(answerer.await.unwrap(), 1, "three denials, one question");

        for n in ["a.txt", "b.txt", "c.txt"] {
            let _ = std::fs::set_permissions(src.join(n), std::fs::Permissions::from_mode(0o644));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Answering "Abort" stops the operation rather than skipping on.
    #[cfg(unix)]
    #[tokio::test]
    async fn abort_at_a_denial_stops_the_operation() {
        use std::os::unix::fs::PermissionsExt;
        if nix::unistd::Uid::effective().is_root() {
            return;
        }
        let root = unique_dir("denied_abort");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::set_permissions(src.join("a.txt"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, mut rx) = async_bridge::channel();
        let (reply_tx, reply_rx) = mpsc::channel(4);
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if matches!(ev, crate::app::event::AppEvent::PermissionDenied(_)) {
                    let _ = reply_tx.send(TaskReply::Privilege(PrivDecision::Abort)).await;
                }
            }
        });

        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Cancelled), "abort cancels: {outcome:?}");

        let _ = std::fs::set_permissions(src.join("a.txt"), std::fs::Permissions::from_mode(0o644));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Interop: what we write must be readable by the tools people actually
    /// restore with. Runs `trash-list` (trash-cli) against a sandboxed
    /// `XDG_DATA_HOME` and checks it reports our entry and its original path.
    /// Skipped where trash-cli isn't installed, so CI stays green without it.
    #[tokio::test]
    async fn a_trashed_file_is_readable_by_trash_cli() {
        let Ok(tool) = which_trash_list() else {
            eprintln!("skipping: trash-list (trash-cli) not installed");
            return;
        };
        let root = unique_dir("trash_interop");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _guard = TempHome::set(&home);

        let src = root.join("interop probe.txt");
        std::fs::write(&src, b"x").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Trash,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: None,
            dst_dir: None,
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        let out = std::process::Command::new(tool)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .output()
            .expect("run trash-list");
        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(
            listing.contains("interop probe.txt"),
            "trash-cli should list what we trashed (note the space in the name); got: {listing}"
        );
        assert!(
            listing.contains(&src.display().to_string()),
            "and should decode the original path back: {listing}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `trash-list` on PATH, if it is installed.
    fn which_trash_list() -> std::result::Result<PathBuf, ()> {
        let path = std::env::var_os("PATH").ok_or(())?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("trash-list"))
            .find(|p| p.is_file())
            .ok_or(())
    }

    /// A whole directory goes in one piece, contents and all.
    #[tokio::test]
    async fn trash_takes_a_directory_tree_whole() {
        let root = unique_dir("trash_tree");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _guard = TempHome::set(&home);

        let src = root.join("project");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"b").unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Trash,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: None,
            dst_dir: None,
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        assert!(!src.exists());
        let files = home.join(".local/share/Trash/files");
        assert_eq!(std::fs::read(files.join("project/sub/b.txt")).unwrap(), b"b");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Trashing two files of the same name must not lose one: the second gets a
    /// numbered name and its own sidecar.
    #[tokio::test]
    async fn trashing_the_same_name_twice_keeps_both() {
        let root = unique_dir("trash_dup");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _guard = TempHome::set(&home);

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        for body in [b"first".as_slice(), b"second".as_slice()] {
            let src = root.join("same.txt");
            std::fs::write(&src, body).unwrap();
            let (tx, _rx) = async_bridge::channel();
            let req = OpRequest {
                kind: OpKind::Trash,
                src_fs: fs.clone(),
                sources: vec![VfsPath::local(&src)],
                dst_fs: None,
                dst_dir: None,
                dst_name: None,
                overwrite_all: false,
                steps: Vec::new(),
            };
            let (_reply_tx, reply_rx) = mpsc::channel(1);
            let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
            assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");
        }

        let files = home.join(".local/share/Trash/files");
        assert_eq!(std::fs::read(files.join("same.txt")).unwrap(), b"first");
        assert_eq!(std::fs::read(files.join("same (2).txt")).unwrap(), b"second");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Regression test for the ordering bug: the timestamp must be stamped
    /// *before* the destination is chmod'ed read-only, and via a call that does
    /// not need a write-open. Both the mode and the mtime have to survive.
    #[cfg(unix)]
    #[tokio::test]
    async fn copy_preserves_mtime_on_a_readonly_source() {
        use std::os::unix::fs::PermissionsExt;
        let root = unique_dir("copy_ro_mtime");
        let src = root.join("ro.txt");
        std::fs::write(&src, b"read only").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs: fs.clone(),
            sources: vec![VfsPath::local(&src)],
            dst_fs: Some(fs.clone()),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(matches!(outcome, TaskOutcome::Done), "outcome: {outcome:?}");

        let meta = std::fs::metadata(dst_dir.join("ro.txt")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o444, "mode carried over");
        assert_eq!(meta.modified().unwrap(), old, "mtime survived the read-only mode");
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod partial_cleanup_tests {
    use super::*;
    use crate::ops::{OpKind, OpRequest};
    use crate::util::async_bridge;
    use crate::vfs::local::LocalFs;
    use crate::vfs::testmock::MockVfs;

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rc_test_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A source read that drops mid-copy must not leave a truncated file at the
    /// destination — the read-error path now cleans up like cancel/write errors.
    #[tokio::test]
    async fn copy_removes_the_partial_destination_on_a_read_error() {
        let root = unique_dir("readerr");
        let dst_dir = root.join("dest");
        std::fs::create_dir_all(&dst_dir).unwrap();

        // A mock source claiming 100 bytes but failing after 10.
        let src_fs: Arc<dyn Vfs> =
            MockVfs { file_size: 100, read_fail_after: Some(10), ..Default::default() }.arc();
        let dst_fs: Arc<dyn Vfs> = Arc::new(LocalFs::new());
        let (tx, _rx) = async_bridge::channel();
        let req = OpRequest {
            kind: OpKind::Copy,
            src_fs,
            sources: vec![VfsPath {
                scheme: "mock".into(),
                path: "/src.bin".into(),
                container: None,
            }],
            dst_fs: Some(dst_fs),
            dst_dir: Some(VfsPath::local(&dst_dir)),
            dst_name: None,
            overwrite_all: false,
            steps: Vec::new(),
        };
        let (_reply_tx, reply_rx) = mpsc::channel(1);
        let outcome = run(1, req, tx, CancelToken::new(), reply_rx).await;
        assert!(
            matches!(outcome, TaskOutcome::Failed(_)),
            "the read error fails the op: {outcome:?}"
        );
        assert!(
            !dst_dir.join("src.bin").exists(),
            "the truncated destination file was removed, not left behind"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
