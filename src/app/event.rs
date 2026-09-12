//! Events delivered to the render loop from background tasks.
//!
//! Terminal input is handled separately (read directly in the loop); this
//! channel carries only asynchronous results so the loop never blocks on I/O.

use crate::net::Scan;
use crate::ops::progress::{ConflictInfo, DeniedInfo, ProgressUpdate, TaskId, TaskOutcome};
use crate::util::checksum::ChecksumReport;
use crate::vfs::VfsPath;

/// Why a file was fetched to a local temp (so the handler opens the right view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchKind {
    View,
    Edit,
}

/// One find-file match: the file, its size, and the line of the first content
/// hit. `line` is `None` when the search matched on name alone — which is always
/// the case on a remote or in-archive panel, where content search isn't run.
#[derive(Debug, Clone)]
pub struct FindHit {
    pub path: VfsPath,
    pub size: u64,
    pub line: Option<u64>,
}

/// Which guided Git dialog to open once the repository's branches/remotes have
/// been read in the background (see [`AppEvent::GitInfo`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitInfoForm {
    Checkout,
    Push,
    Fetch,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The persistent console subshell produced output; a coalesced signal to
    /// wake the render loop so the backdrop repaints. Carries no data — the
    /// output is already in the shared emulator.
    ConsoleOutput,
    /// The filesystem watcher has collected events in the app's inbox (see
    /// `app::state::watch`). One wake-up per batch, however many events.
    FsActivity,
    /// A thumbnail for panel `side`'s grid was loaded (`None`: it has none after
    /// all — not decodable, or too big).
    Thumbnail {
        side: usize,
        key: crate::thumbs::ThumbKey,
        thumb: Option<std::sync::Arc<crate::thumbs::Thumb>>,
    },
    /// A throttled progress snapshot from the ops engine.
    Progress(ProgressUpdate),
    /// A copy/move hit an existing destination; the engine is paused awaiting the
    /// user's overwrite decision (sent back via the task's reply channel).
    Conflict(ConflictInfo),
    /// A step failed on filesystem permissions and the engine is paused waiting
    /// to be told whether to escalate, skip, or give up.
    PermissionDenied(DeniedInfo),
    /// A background task finished (success, cancel, or failure).
    TaskDone { id: TaskId, outcome: TaskOutcome },
    /// A pending archive add has finished scanning the destination archive for
    /// members the copy would replace. `Ok(names)` lists them (empty = none);
    /// `Err` is why the archive could not be read.
    ArchiveAddChecked {
        conflicts: Result<Vec<String>, String>,
        request: Box<crate::ops::ArchiveAdd>,
    },
    /// A privileged disk-manager command (mount/unmount/format) run in the
    /// background finished; carries its result and the success message to show.
    PrivilegedDone { ok_msg: String, result: Result<(), String> },
    /// An image-flash task finished (success, cancel, or failure).
    FlashDone { id: TaskId, outcome: TaskOutcome },
    /// A device-imaging ("create image") task finished.
    ImageDone { id: TaskId, outcome: TaskOutcome },
    /// A file-checksum task finished. `Ok(report)` on success (the report also
    /// carries any comparison verdict); `Err(Some(msg))` on I/O failure;
    /// `Err(None)` when the user aborted (the progress dialog just closes).
    ChecksumDone { id: TaskId, result: Result<ChecksumReport, Option<String>> },
    /// A find-file task finished (or was aborted); carries the matches collected
    /// so far so partial results can still be panelized. Paths may be local or
    /// remote, depending on the searched backend.
    FindDone { id: TaskId, results: Vec<FindHit> },
    /// A find-duplicates task finished (or was cancelled). Carries the file names
    /// to mark in the left and right panels (identical per the chosen criteria);
    /// partial on cancel.
    DuplicatesFound { id: TaskId, left: Vec<String>, right: Vec<String> },
    /// A "Details" panel's background size scan reported progress (`done` marks
    /// the final update). `viewer` is the panel displaying the details; a stale
    /// `generation` is ignored.
    DetailsTally { viewer: usize, generation: u64, total: u64, files: u64, dirs: u64, done: bool },
    /// A network-explorer `ss` scan finished; `generation` lets the view drop a
    /// result from a scan it has already superseded.
    NetworkScanned { generation: u64, result: Result<Scan, String> },
    /// A reverse-DNS lookup for a peer IP finished (`host` = `None` = no PTR).
    ReverseDnsResolved { ip: String, host: Option<String> },
    /// A background Git-status scan for panel `side` finished; a stale
    /// `generation` is ignored. `status` is `None` when the directory is not a
    /// git work tree (or git is unavailable).
    GitStatusScanned { side: usize, generation: u64, status: Option<Box<crate::git::GitStatus>> },
    /// The viewer's background `git blame` finished. Only a viewer still waiting
    /// on this `generation` takes it; the error is a message for a dialog.
    BlameLoaded { generation: u64, result: Result<Box<crate::git::blame::Blame>, String> },
    /// One revision's file sizes arrived for the 3D time machine. A stale
    /// `generation` — the user scrubbed onward while this was in flight — is
    /// still cached, since it cost a `git` call, but does not become the scene.
    TimelineTree { oid: String, generation: u64, result: Result<Vec<(String, u64)>, String> },
    /// A background Details-view preview load finished for panel `viewer`; a stale
    /// `generation` is ignored.
    DetailsPreview { viewer: usize, generation: u64, preview: Box<crate::details::Preview> },
    /// A Details view's git activity calendar was counted, for the item `key`
    /// names; ignored when that view has moved on. `None` when it turned out
    /// not to be in a work tree after all.
    DetailsActivity {
        viewer: usize,
        key: String,
        activity: Option<std::sync::Arc<crate::git::activity::Activity>>,
    },
    /// A "Send file over LAN" selection finished being zipped to a temp archive;
    /// `Ok(path)` gives the archive to serve, `Err(msg)` reports a failure. `name`
    /// is the friendly download name to advertise. Only used for the multi-file /
    /// directory case (a lone file skips zipping).
    SendPrepared { name: String, result: Result<std::path::PathBuf, String> },
    /// A device fully downloaded the shared file from the LAN send server; the
    /// open Send dialog bumps its download counter.
    FileSent,
    /// Receive over LAN: an upload in flight has `received` of its `total` bytes.
    ReceiveProgress { name: String, received: u64, total: u64 },
    /// Receive over LAN: a file arrived whole and was saved as `name`.
    FileReceived { name: String, bytes: u64 },
    /// Receive over LAN: an upload failed and nothing was kept.
    ReceiveFailed { name: String, error: String },
    /// A directory-sync plan finished being computed (both trees walked and
    /// diffed). Nothing has been changed yet — the plan is shown for approval.
    SyncPlanned { result: Result<Box<crate::ops::sync::SyncPlan>, String> },
    /// A Git command finished; `title` names it (e.g. `"push"`). The handler shows
    /// the output (or closes quietly when a successful command said nothing) and
    /// refreshes the panels' VCS state.
    GitDone { title: String, out: crate::git::ops::GitOutput },
    /// The branches/remotes behind a guided Git dialog were read; open `form`
    /// populated with them.
    GitInfo { form: GitInfoForm, info: Box<crate::git::ops::RepoInfo> },
    /// A view/edit fetch streamed a (remote/archive) file to a local temp file;
    /// the handler opens it (paged viewer, or editor targeting `orig_path`).
    FileFetched {
        id: TaskId,
        kind: FetchKind,
        name: String,
        orig_path: VfsPath,
        temp: std::path::PathBuf,
    },
}
