//! The background directory crawler that fills [`SizeTree`].
//!
//! One long-lived task per session. It is *steered*, not restarted: navigating
//! publishes a new focus path, and the crawler puts that subtree at the front of
//! its queue while keeping everything it has already learned. That is what turns
//! re-entering a directory from a full rescan into an instant redraw.
//!
//! Work order is breadth-first from the focus directory, which is deliberate:
//! it means every box on screen grows a little at a time, rather than one box
//! reaching its final size while its neighbours are still empty — so the
//! relative sizes a user is reading are meaningful from the first frames.

use super::{NodeId, SizeTree};
use crate::ops::CancelToken;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How deep from a crawl root directories still get their own node. Deeper
/// directories are still walked — their bytes roll up into the deepest
/// materialized ancestor — but no node is created for them, so memory scales
/// with the breadth of the tree rather than with its total directory count.
const DEPTH_CAP: u16 = 6;

/// How long one blocking work slice runs before yielding, so a focus change is
/// picked up promptly and the runtime's blocking pool is not monopolized.
const SLICE: Duration = Duration::from_millis(50);

/// One queued directory.
struct Item {
    path: PathBuf,
    /// The node this directory's files are credited to.
    owner: NodeId,
    /// Whether `owner` is this very directory, or an ancestor because we are
    /// past [`DEPTH_CAP`].
    own_node: bool,
    depth: u16,
}

/// Handle to the crawler, held on `AppState`.
pub struct Crawler {
    tree: Arc<Mutex<SizeTree>>,
    focus_tx: watch::Sender<PathBuf>,
    cancel: CancelToken,
    running: Arc<AtomicBool>,
}

impl Crawler {
    /// Start the crawler, focused on `focus`.
    pub fn spawn(focus: PathBuf) -> Crawler {
        let tree = Arc::new(Mutex::new(SizeTree::new()));
        let (focus_tx, focus_rx) = watch::channel(focus);
        let cancel = CancelToken::new();
        let running = Arc::new(AtomicBool::new(true));
        tokio::spawn(run(tree.clone(), focus_rx, cancel.clone(), running.clone()));
        Crawler { tree, focus_tx, cancel, running }
    }

    /// Point the crawler at `path`: its subtree jumps the queue. Everything
    /// already learned is kept, and a subtree that is already done is not
    /// walked again.
    pub fn focus(&self, path: &Path) {
        let _ = self.focus_tx.send(path.to_path_buf());
    }

    /// Read the cache. The guard is held only for the call, so renderers copy
    /// what they need out and drop it before drawing.
    pub fn with_tree<R>(&self, f: impl FnOnce(&SizeTree) -> R) -> R {
        f(&lock(&self.tree))
    }

    /// Whether a crawl is in flight — drives both the "scanning…" readout and
    /// the animation tick, so a settled cache costs no CPU at all.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

impl Drop for Crawler {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Lock the cache, recovering from a poisoned mutex rather than propagating a
/// panic into the render loop — a half-updated size cache is cosmetic.
fn lock(tree: &Mutex<SizeTree>) -> std::sync::MutexGuard<'_, SizeTree> {
    tree.lock().unwrap_or_else(|e| e.into_inner())
}

async fn run(
    tree: Arc<Mutex<SizeTree>>,
    mut focus_rx: watch::Receiver<PathBuf>,
    cancel: CancelToken,
    running: Arc<AtomicBool>,
) {
    let mut queue: VecDeque<Item> = VecDeque::new();
    seed(&tree, &mut queue, &focus_rx.borrow_and_update().clone());

    loop {
        if cancel.is_cancelled() {
            break;
        }
        if focus_rx.has_changed().unwrap_or(false) {
            let f = focus_rx.borrow_and_update().clone();
            seed(&tree, &mut queue, &f);
        }
        if queue.is_empty() {
            // Nothing left to do: park until the user goes somewhere new. This
            // is what lets an idle rat-commander sit at zero CPU.
            running.store(false, Ordering::Relaxed);
            tokio::select! {
                r = focus_rx.changed() => {
                    if r.is_err() { break; }
                    let f = focus_rx.borrow_and_update().clone();
                    seed(&tree, &mut queue, &f);
                }
                _ = cancel.cancelled() => break,
            }
            continue;
        }
        running.store(true, Ordering::Relaxed);
        let t = tree.clone();
        let taken = std::mem::take(&mut queue);
        queue = match tokio::task::spawn_blocking(move || slice(&t, taken)).await {
            Ok(q) => q,
            Err(_) => break, // the runtime is going away
        };
    }
    running.store(false, Ordering::Relaxed);
}

/// Put `focus` (and then its parent, so siblings get sized too) at the front of
/// the queue. Subtrees already done, or already queued, are left alone — which
/// is what keeps repeated navigation from stacking duplicate work.
fn seed(tree: &Mutex<SizeTree>, queue: &mut VecDeque<Item>, focus: &Path) {
    let mut t = lock(tree);
    // Pushed front-first in reverse priority, so `focus` ends up ahead of its
    // parent.
    for (i, path) in [focus.parent(), Some(focus)].into_iter().flatten().enumerate() {
        if is_excluded(path) || t.is_complete(path) || t.is_pending(path) {
            continue;
        }
        let id = t.ensure(path);
        t.add_outstanding(id);
        queue.push_front(Item {
            path: path.to_path_buf(),
            owner: id,
            own_node: true,
            // The parent is seeded at depth 0 too: it is a crawl root in its
            // own right, and its children (the focus's siblings) deserve nodes.
            depth: 0,
        });
        let _ = i;
    }
}

/// Process queued directories until the slice budget runs out.
fn slice(tree: &Mutex<SizeTree>, mut queue: VecDeque<Item>) -> VecDeque<Item> {
    let start = Instant::now();
    while let Some(item) = queue.pop_front() {
        visit(tree, item, &mut queue);
        if start.elapsed() >= SLICE {
            break;
        }
    }
    queue
}

/// Read one directory, credit its files, and queue its subdirectories.
fn visit(tree: &Mutex<SizeTree>, item: Item, queue: &mut VecDeque<Item>) {
    // All I/O happens outside the lock.
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&item.path) {
        for de in rd.flatten() {
            let Ok(ft) = de.file_type() else { continue };
            // Symlinks are never followed or counted, so no cycle can occur and
            // nothing is double-counted through a link.
            if ft.is_symlink() {
                continue;
            }
            let path = de.path();
            if ft.is_dir() {
                if !is_excluded(&path) {
                    subdirs.push(path);
                }
            } else if ft.is_file()
                && let Ok(meta) = de.metadata()
            {
                files.push((path, crate::disk::on_disk_len(&meta)));
            }
        }
    }

    let mut t = lock(tree);
    if item.own_node {
        t.mark_listed(item.owner);
    } else {
        // Past the depth cap there is no node of its own, but the directory
        // still counts toward the "scanning… N dirs" readout.
        t.dirs_seen += 1;
    }
    for (path, size) in files {
        t.add_file(item.owner, &path, size);
    }
    let depth = item.depth + 1;
    for sub in subdirs {
        if depth <= DEPTH_CAP {
            let child = t.ensure(&sub);
            if t.is_complete(&sub) || t.is_pending(&sub) {
                continue; // already known, or already queued — don't redo it
            }
            // One unit on the parent for this subtree, one on the child for
            // its own listing.
            t.add_outstanding(item.owner);
            t.add_outstanding(child);
            queue.push_back(Item { path: sub, owner: child, own_node: true, depth });
        } else {
            t.add_outstanding(item.owner);
            queue.push_back(Item { path: sub, owner: item.owner, own_node: false, depth });
        }
    }
    // Retire this directory's own unit last, so it cannot transiently reach
    // zero while its children are still being queued.
    t.finish_outstanding(item.owner);
}

/// Kernel/pseudo filesystems, which have no meaningful on-disk size and whose
/// contents can block or recurse forever. Without this, crawling from `/`
/// descends into `/proc` — which the old one-shot scanner did too, and which
/// matters far more now that crawling goes wide.
#[cfg(unix)]
fn is_excluded(path: &Path) -> bool {
    matches!(
        path.as_os_str().as_encoded_bytes(),
        b"/proc" | b"/sys" | b"/dev" | b"/run"
    )
}

#[cfg(not(unix))]
fn is_excluded(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a small tree under a fresh temp dir and crawl it synchronously by
    /// draining the queue, so the tests don't depend on task scheduling.
    fn crawl_all(root: &Path) -> SizeTree {
        let tree = Mutex::new(SizeTree::new());
        let mut q = VecDeque::new();
        {
            let mut t = lock(&tree);
            let id = t.ensure(root);
            t.add_outstanding(id);
            q.push_back(Item { path: root.to_path_buf(), owner: id, own_node: true, depth: 0 });
        }
        while let Some(item) = q.pop_front() {
            visit(&tree, item, &mut q);
        }
        tree.into_inner().unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rc-sizes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn crawl_sums_a_tree_and_marks_it_complete() {
        let root = tmp("sum");
        fs::create_dir_all(root.join("a/deep")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        fs::write(root.join("a/deep/x"), vec![0u8; 4096]).unwrap();
        fs::write(root.join("b/y"), vec![0u8; 8192]).unwrap();

        let t = crawl_all(&root);
        let (total, complete) = t.total_of(&root);
        assert!(complete, "a fully drained crawl marks the root complete");
        assert!(total >= 4096 + 8192, "total {total} covers both files");
        // Each subtree is attributed to the right box.
        let kids = t.children_of(&root);
        let names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["b", "a"], "largest first");
        assert!(t.is_complete(&root.join("a/deep")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinked_directories_are_not_followed_or_counted() {
        let root = tmp("link");
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/f"), vec![0u8; 4096]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        let t = crawl_all(&root);
        // The link contributes nothing and gets no node, so the bytes are
        // counted exactly once.
        assert!(t.get(&root.join("link")).is_none(), "no node for a symlink");
        let names: Vec<String> = t.children_of(&root).iter().map(|k| k.name.clone()).collect();
        assert_eq!(names, ["real"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn directories_past_the_depth_cap_roll_up_instead_of_getting_nodes() {
        let root = tmp("cap");
        let mut deep = root.clone();
        for i in 0..(DEPTH_CAP + 3) {
            deep = deep.join(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("f"), vec![0u8; 4096]).unwrap();

        let t = crawl_all(&root);
        // The bytes still reach the root...
        assert!(t.total_of(&root).0 >= 4096, "deep bytes still roll up");
        // ...but the deepest directories got no node of their own.
        assert!(t.get(&deep).is_none(), "past the cap, no node is materialized");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_already_complete_subtree_is_not_walked_again() {
        let root = tmp("reuse");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/f"), vec![0u8; 4096]).unwrap();

        let tree = Mutex::new(crawl_all(&root));
        let before = lock(&tree).dirs_seen;
        // Re-focusing a finished directory must queue nothing at all — this is
        // the rescan-on-every-keypress regression the cache exists to kill.
        let mut q = VecDeque::new();
        seed(&tree, &mut q, &root.join("a"));
        assert!(q.is_empty(), "a complete subtree re-queues nothing");
        assert_eq!(lock(&tree).dirs_seen, before, "and re-reads no directories");
        let _ = fs::remove_dir_all(&root);
    }

    /// The live task: focusing a directory fills the cache in the background
    /// and eventually settles, and re-focusing a finished tree does no work.
    #[tokio::test]
    async fn the_live_crawler_fills_the_cache_and_then_parks() {
        let root = tmp("live");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/f"), vec![0u8; 8192]).unwrap();

        let c = Crawler::spawn(root.clone());
        // Give it a moment to drain; the tree is tiny, so a few slices is plenty.
        let mut settled = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if !c.is_running() && c.with_tree(|t| t.is_complete(&root)) {
                settled = true;
                break;
            }
        }
        assert!(settled, "the crawler finished and parked");
        c.with_tree(|t| {
            assert!(t.total_of(&root).0 >= 8192, "bytes reached the root");
            assert_eq!(t.children_of(&root).len(), 1, "one child directory");
        });

        // Parked means parked: re-focusing a finished tree reads nothing more.
        let seen = c.with_tree(|t| t.dirs_seen);
        c.focus(&root.join("a"));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(c.with_tree(|t| t.dirs_seen), seen, "no directory was re-read");
        assert!(!c.is_running(), "and the crawler stayed parked");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pseudo_filesystems_are_excluded() {
        #[cfg(unix)]
        {
            assert!(is_excluded(Path::new("/proc")));
            assert!(is_excluded(Path::new("/sys")));
            assert!(is_excluded(Path::new("/dev")));
            assert!(!is_excluded(Path::new("/home")));
            // Only the mount points themselves are named; a user's own
            // directory that merely starts with those letters is fine.
            assert!(!is_excluded(Path::new("/process-notes")));
        }
    }
}
