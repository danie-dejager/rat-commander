//! The file-operation subsystem: request types, the engine, progress, cancel.

pub mod cancel;
pub mod engine;
pub mod progress;
pub mod sync;

pub use cancel::CancelToken;
pub use progress::TaskId;

use crate::app::event::AppEvent;
use crate::ops::progress::TaskReply;
use crate::util::async_bridge::AppSender;
use crate::vfs::{Vfs, VfsPath};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Which operation to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Delete,
    /// Move to the freedesktop trash instead of unlinking. Local paths only —
    /// see [`crate::trash`].
    Trash,
    /// Execute a directory-sync plan ([`OpRequest::steps`]) rather than a
    /// sources → destination transfer.
    Sync,
}

/// A pending "copy/move these local files into an archive" request.
///
/// Adding to an archive rebuilds the whole container, so it is a single bulk
/// step rather than a file-by-file transfer — which means the overwrite
/// question has to be asked once, up front, before the rebuild starts. The
/// request is parked in this struct while that happens.
#[derive(Debug, Clone)]
pub struct ArchiveAdd {
    pub kind: OpKind,
    pub sources: Vec<VfsPath>,
    /// The directory *inside* the archive the sources land in.
    pub dest: VfsPath,
}

/// A fully-resolved operation request handed to the engine.
pub struct OpRequest {
    pub kind: OpKind,
    pub src_fs: Arc<dyn Vfs>,
    pub sources: Vec<VfsPath>,
    pub dst_fs: Option<Arc<dyn Vfs>>,
    pub dst_dir: Option<VfsPath>,
    /// For a single-source rename/move-to-name, the exact final name to give the
    /// source inside `dst_dir` (instead of keeping its own name). `None` means the
    /// source is dropped into `dst_dir` under its existing name.
    pub dst_name: Option<String>,
    /// Overwrite existing destinations without prompting (confirm-overwrite off).
    pub overwrite_all: bool,
    /// For [`OpKind::Sync`]: the planned steps. Each names its own exact source
    /// and destination and which side it runs on (`0` = `src_fs`, `1` = `dst_fs`),
    /// so a plan can copy in both directions within the one task. `sources`,
    /// `dst_dir` and `dst_name` are unused in that mode.
    pub steps: Vec<sync::SyncStep>,
}

/// A handle to a running task, stored by the app so the progress dialog's
/// Abort button can trip its cancel token, and so the overwrite dialog can send
/// the user's decision back to the (paused) engine.
pub struct TaskHandle {
    #[allow(dead_code)] // task identity, set by every spawner; not read back yet
    pub id: TaskId,
    pub cancel: CancelToken,
    pub reply: mpsc::Sender<TaskReply>,
}

/// Spawn the operation on the tokio runtime. The task reports progress and a
/// terminal [`AppEvent::TaskDone`] over `tx`.
pub fn spawn_op(id: TaskId, req: OpRequest, tx: AppSender) -> TaskHandle {
    let cancel = CancelToken::new();
    let task_cancel = cancel.clone();
    let done_tx = tx.clone();
    // Capacity 1: at most one outstanding conflict prompt per task.
    let (reply_tx, reply_rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let outcome = engine::run(id, req, tx, task_cancel, reply_rx).await;
        let _ = done_tx.send(AppEvent::TaskDone { id, outcome }).await;
    });
    TaskHandle { id, cancel, reply: reply_tx }
}
