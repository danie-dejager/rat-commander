//! Shared directory-size cache.
//!
//! [`SizeTree`] is a path-keyed arena of directory nodes, each carrying a
//! running subtree total and the largest files beneath it. It is filled
//! incrementally by the background crawler in [`crawl`], and read by both the
//! disk explorer and the 3D space view — so a directory is walked once per
//! session, not once per keypress.
//!
//! **Why this is shared state rather than an `AppEvent` payload.** The rest of
//! the app follows a strict rule: background workers never touch state, they
//! send [`AppEvent`](crate::app::event::AppEvent)s and the render loop applies
//! them. The crawler is the one place that rule does not fit, because it has to
//! *read* the tree to know which subtrees it has already completed and can skip.
//! Passing snapshots would mean the crawler keeping a second full copy of the
//! same data. So the tree lives behind a mutex instead. What the rule actually
//! protects — UI state, and the terminal — is untouched by the crawler; this is
//! a data cache, and the app already shares `Arc<dyn Vfs>` backends across
//! tasks. Locks are held only for bookkeeping, never across I/O.

pub mod crawl;
pub mod timeline;

use crate::disk::{FileEntry, TOP_FILES};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Build a finished [`SizeTree`] from a flat list of `(path relative to root,
/// size)` pairs — the shape `git ls-tree` hands back.
///
/// The crawler is not the only thing that can fill a size tree, and nothing
/// about the type says it is. This is the other filler: where the crawler
/// discovers a directory at a time and revises totals as it goes, a revision's
/// listing arrives complete, so the tree is built once and handed over finished.
///
/// `epoch` is stamped into `dirs_seen`, which is what the 3D view and the disk
/// explorer compare against their own `synced_at` to notice new data. A counter
/// of directories would collide between two revisions that happen to hold the
/// same number of them; a caller-chosen epoch cannot.
pub fn from_paths<'a>(
    root: &Path,
    entries: impl Iterator<Item = (&'a str, u64)>,
    epoch: u64,
) -> SizeTree {
    let mut tree = SizeTree::new();
    // The root exists even for an empty revision, so the view has something to
    // stand on rather than reporting the directory missing.
    tree.ensure(root);
    for (rel, size) in entries {
        let full = root.join(rel);
        let Some(dir) = full.parent() else { continue };
        let id = tree.ensure(dir);
        tree.add_file(id, &full, size);
    }
    tree.complete_below(root);
    tree.dirs_seen = epoch;
    tree
}

/// Index of a node in [`SizeTree::nodes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    fn ix(self) -> usize {
        self.0 as usize
    }
}

/// One directory in the cache.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub parent: Option<NodeId>,
    /// Materialized subdirectories. Directories past the crawler's depth cap
    /// are not materialized — their bytes roll up into the deepest ancestor
    /// that is, so this can be empty on a node with a large `total`.
    pub children: Vec<NodeId>,
    /// Bytes of files sitting directly in this directory.
    pub own: u64,
    /// Bytes of this directory's whole subtree. Only ever grows while a crawl
    /// is in flight, so boxes drawn from it never shrink mid-scan.
    pub total: u64,
    /// The whole subtree has been walked.
    pub complete: bool,
    /// This directory's own `read_dir` has been done.
    pub listed: bool,
    /// The largest files anywhere beneath this directory, largest first.
    pub top_files: Vec<FileEntry>,
    /// How many things this node is still waiting on: its own pending listing,
    /// plus one per queued child subtree. At zero the node is `complete` and
    /// its parent's count drops by one, cascading up.
    outstanding: u32,
}

/// A directory as handed to the renderers: an owned copy taken under the lock
/// so drawing never holds it.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `path`/`complete`/`has_children` are for the 3D scene
pub struct DirInfo {
    pub name: String,
    pub path: PathBuf,
    pub total: u64,
    pub complete: bool,
    pub top_files: Vec<FileEntry>,
    /// Whether this directory has materialized children worth descending into.
    pub has_children: bool,
}

/// The path-keyed arena. See the module comment for the ownership rationale.
#[derive(Debug, Default)]
pub struct SizeTree {
    nodes: Vec<Node>,
    index: HashMap<PathBuf, NodeId>,
    /// Directories whose listing has been done, for the "scanning… N dirs"
    /// readout that replaced the old full-screen progress bar.
    pub dirs_seen: u64,
}

impl SizeTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, path: &Path) -> Option<&Node> {
        self.index.get(path).map(|id| &self.nodes[id.ix()])
    }

    #[allow(dead_code)] // used by the 3D scene builder
    pub fn id_of(&self, path: &Path) -> Option<NodeId> {
        self.index.get(path).copied()
    }

    #[allow(dead_code)] // used by the 3D scene builder
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.ix()]
    }

    #[allow(dead_code)] // used by the 3D scene builder
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Find or create the node for `path`, creating any missing ancestors as
    /// placeholders. Navigating *above* the current root simply wires the old
    /// root up to a newly created parent, so the cache survives going up.
    pub fn ensure(&mut self, path: &Path) -> NodeId {
        if let Some(&id) = self.index.get(path) {
            return id;
        }
        let parent = path.parent().map(|p| self.ensure(p));
        let id = NodeId(self.nodes.len() as u32);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.nodes.push(Node {
            name,
            path: path.to_path_buf(),
            parent,
            children: Vec::new(),
            own: 0,
            total: 0,
            complete: false,
            listed: false,
            top_files: Vec::new(),
            outstanding: 0,
        });
        if let Some(p) = parent {
            self.nodes[p.ix()].children.push(id);
        }
        self.index.insert(path.to_path_buf(), id);
        id
    }

    /// Credit `size` bytes for the file at `full` to `id` and every ancestor,
    /// offering it to each one's largest-files list.
    pub fn add_file(&mut self, id: NodeId, full: &Path, size: u64) {
        self.nodes[id.ix()].own += size;
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = &mut self.nodes[c.ix()];
            n.total += size;
            // The size gate rejects the overwhelming majority of files without
            // touching the list or allocating a relative path for them.
            if accepts(&n.top_files, size)
                && let Ok(rel) = full.strip_prefix(&n.path)
            {
                insert_top(
                    &mut n.top_files,
                    FileEntry { rel: rel.to_string_lossy().into_owned(), size },
                );
            }
            cur = n.parent;
        }
    }

    /// Register one more outstanding unit of work on `id` (a queued subtree, or
    /// the node's own pending listing).
    pub fn add_outstanding(&mut self, id: NodeId) {
        self.nodes[id.ix()].outstanding += 1;
        // Newly-pending work un-completes the chain above it, which matters
        // when the user re-focuses a directory that was finished earlier.
        let mut cur = self.nodes[id.ix()].parent;
        while let Some(c) = cur {
            if !self.nodes[c.ix()].complete {
                break;
            }
            self.nodes[c.ix()].complete = false;
            self.nodes[c.ix()].outstanding += 1;
            cur = self.nodes[c.ix()].parent;
        }
    }

    /// Retire one unit of work on `id`, cascading completion upward.
    pub fn finish_outstanding(&mut self, id: NodeId) {
        let mut cur = Some(id);
        while let Some(c) = cur {
            let n = &mut self.nodes[c.ix()];
            n.outstanding = n.outstanding.saturating_sub(1);
            if n.outstanding > 0 || !n.listed || n.complete {
                return;
            }
            n.complete = true;
            let parent = n.parent;
            // Only cascade into a parent that has listed its own children,
            // because only then did it count this subtree. A placeholder
            // ancestor (created by `ensure` on the way down and never crawled)
            // never took that +1, so decrementing it here would corrupt its
            // count and let it claim a completion it never earned.
            cur = parent.filter(|&p| self.nodes[p.ix()].listed);
        }
    }

    pub fn mark_listed(&mut self, id: NodeId) {
        self.nodes[id.ix()].listed = true;
        self.dirs_seen += 1;
    }

    /// Declare every directory at or below `root` fully walked.
    ///
    /// For a tree built from a listing that was already complete — a git
    /// revision, say — there is no crawl to wait on, and a node left incomplete
    /// would make the 3D view claim it is still scanning forever.
    ///
    /// Ancestors *above* `root` are deliberately left unlisted: they were only
    /// created to hang the root off, nothing enumerated them, and the 3D view
    /// picks its anchor by walking up through listed nodes. Leaving them alone
    /// is what pins the scene to the root rather than drifting up the disk.
    pub fn complete_below(&mut self, root: &Path) {
        for n in self.nodes.iter_mut().filter(|n| n.path.starts_with(root)) {
            n.listed = true;
            n.complete = true;
            n.outstanding = 0;
        }
    }

    /// Whether `path` is known to be fully walked already — the check that
    /// turns re-entering a directory into a no-op instead of a rescan.
    pub fn is_complete(&self, path: &Path) -> bool {
        self.get(path).is_some_and(|n| n.complete)
    }

    /// Whether a crawl of `path` is already queued or in flight, so the
    /// crawler can re-focus a directory without queueing it twice.
    pub fn is_pending(&self, path: &Path) -> bool {
        self.get(path).is_some_and(|n| n.outstanding > 0)
    }

    /// The materialized subdirectories of `path`, largest first. This is what
    /// the treemap and the 3D view draw.
    pub fn children_of(&self, path: &Path) -> Vec<DirInfo> {
        let Some(&id) = self.index.get(path) else {
            return Vec::new();
        };
        let mut out: Vec<DirInfo> = self.nodes[id.ix()]
            .children
            .iter()
            .map(|&c| {
                let n = &self.nodes[c.ix()];
                DirInfo {
                    name: n.name.clone(),
                    path: n.path.clone(),
                    total: n.total,
                    complete: n.complete,
                    top_files: n.top_files.clone(),
                    has_children: !n.children.is_empty(),
                }
            })
            .collect();
        out.sort_by(|a, b| b.total.cmp(&a.total).then(a.name.cmp(&b.name)));
        out
    }

    /// The subtree total for `path`, and whether it is final.
    pub fn total_of(&self, path: &Path) -> (u64, bool) {
        self.get(path).map_or((0, false), |n| (n.total, n.complete))
    }
}

/// Whether `size` would earn a place in a largest-files list.
fn accepts(list: &[FileEntry], size: u64) -> bool {
    list.len() < TOP_FILES || list.last().is_some_and(|f| size > f.size)
}

/// Insert into a descending-by-size list, keeping it bounded.
fn insert_top(list: &mut Vec<FileEntry>, f: FileEntry) {
    let pos = list.partition_point(|e| e.size > f.size);
    list.insert(pos, f);
    list.truncate(TOP_FILES);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn ensure_creates_ancestor_chain_and_links_parents() {
        let mut t = SizeTree::new();
        let id = t.ensure(&p("/a/b/c"));
        assert_eq!(t.node(id).name, "c");
        // Every ancestor exists and is wired both ways.
        let b = t.id_of(&p("/a/b")).expect("/a/b");
        assert_eq!(t.node(id).parent, Some(b));
        assert!(t.node(b).children.contains(&id));
        assert!(t.id_of(&p("/a")).is_some());
        // Asking again is idempotent — no duplicate node.
        let again = t.ensure(&p("/a/b/c"));
        assert_eq!(again, id);
    }

    #[test]
    fn add_file_rolls_bytes_up_every_ancestor() {
        let mut t = SizeTree::new();
        let c = t.ensure(&p("/a/b/c"));
        t.add_file(c, &p("/a/b/c/big.bin"), 1000);
        assert_eq!(t.total_of(&p("/a/b/c")).0, 1000);
        assert_eq!(t.total_of(&p("/a/b")).0, 1000);
        assert_eq!(t.total_of(&p("/a")).0, 1000);
        // `own` stays local to the directory the file is actually in.
        assert_eq!(t.node(c).own, 1000);
        assert_eq!(t.get(&p("/a/b")).unwrap().own, 0);
    }

    #[test]
    fn top_files_are_bounded_and_relative_to_each_ancestor() {
        let mut t = SizeTree::new();
        let c = t.ensure(&p("/a/b/c"));
        for i in 0..(TOP_FILES + 20) {
            t.add_file(c, &p(&format!("/a/b/c/f{i}")), i as u64 + 1);
        }
        let c_files = &t.node(c).top_files;
        assert_eq!(c_files.len(), TOP_FILES, "list stays bounded");
        assert_eq!(c_files[0].size, (TOP_FILES + 20) as u64, "largest first");
        assert_eq!(c_files[0].rel, format!("f{}", TOP_FILES + 19));
        // The same file is listed relative to each ancestor's own directory.
        let a = t.get(&p("/a")).unwrap();
        assert_eq!(a.top_files[0].rel, format!("b/c/f{}", TOP_FILES + 19));
    }

    #[test]
    fn totals_only_grow_so_boxes_never_shrink_mid_scan() {
        let mut t = SizeTree::new();
        let c = t.ensure(&p("/a/b"));
        let mut last = 0;
        for i in 0..50 {
            t.add_file(c, &p(&format!("/a/b/f{i}")), 7);
            let now = t.total_of(&p("/a")).0;
            assert!(now >= last, "total went backwards: {last} -> {now}");
            last = now;
        }
    }

    #[test]
    fn completion_cascades_up_only_when_all_work_is_retired() {
        let mut t = SizeTree::new();
        let a = t.ensure(&p("/a"));
        let b = t.ensure(&p("/a/b"));
        // /a waits on its own listing plus the /a/b subtree; /a/b on its listing.
        t.add_outstanding(a);
        t.add_outstanding(a);
        t.add_outstanding(b);
        t.mark_listed(a);
        t.mark_listed(b);
        t.finish_outstanding(a); // /a's own listing done, but /a/b is pending
        assert!(!t.is_complete(&p("/a")));
        t.finish_outstanding(b); // /a/b done -> cascades into /a
        assert!(t.is_complete(&p("/a/b")));
        assert!(t.is_complete(&p("/a")), "parent completes once children do");
    }

    #[test]
    fn an_unlisted_node_never_counts_as_complete() {
        let mut t = SizeTree::new();
        let a = t.ensure(&p("/a"));
        t.add_outstanding(a);
        t.finish_outstanding(a); // retired, but the listing never happened
        assert!(!t.is_complete(&p("/a")));
    }

    #[test]
    fn children_are_sorted_largest_first() {
        let mut t = SizeTree::new();
        for (name, size) in [("small", 10u64), ("huge", 9000), ("mid", 500)] {
            let id = t.ensure(&p(&format!("/r/{name}")));
            t.add_file(id, &p(&format!("/r/{name}/f")), size);
        }
        let kids = t.children_of(&p("/r"));
        let names: Vec<&str> = kids.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["huge", "mid", "small"]);
        assert_eq!(kids[0].total, 9000);
    }

    #[test]
    fn children_of_an_unknown_path_is_empty_not_a_panic() {
        let t = SizeTree::new();
        assert!(t.children_of(&p("/nope")).is_empty());
        assert_eq!(t.total_of(&p("/nope")), (0, false));
    }

    #[test]
    fn re_focusing_a_finished_subtree_reopens_its_ancestors() {
        let mut t = SizeTree::new();
        let a = t.ensure(&p("/a"));
        let b = t.ensure(&p("/a/b"));
        t.add_outstanding(a);
        t.mark_listed(a);
        t.mark_listed(b);
        t.finish_outstanding(a);
        assert!(t.is_complete(&p("/a")));
        // New work under /a/b must mark /a unfinished again, or the crawler
        // would report a directory as fully sized while it is still growing.
        t.add_outstanding(b);
        assert!(!t.is_complete(&p("/a")));
    }
}

#[cfg(test)]
mod from_paths_tests {
    use super::*;

    const ROOT: &str = "/repo";

    fn build(entries: &[(&str, u64)], epoch: u64) -> SizeTree {
        from_paths(Path::new(ROOT), entries.iter().copied(), epoch)
    }

    #[test]
    fn sizes_roll_up_to_the_root() {
        let t = build(&[("a.txt", 10), ("src/b.rs", 20), ("src/deep/c.rs", 30)], 1);
        assert_eq!(t.total_of(Path::new(ROOT)).0, 60);
        assert_eq!(t.total_of(Path::new("/repo/src")).0, 50);
        assert_eq!(t.total_of(Path::new("/repo/src/deep")).0, 30);
        // Bytes sitting directly in a directory are its own, not its children's.
        assert_eq!(t.get(Path::new("/repo/src")).unwrap().own, 20);
    }

    /// The 3D view reports "Scanning…" for as long as the focus is incomplete.
    /// A revision's listing arrives whole, so every directory in it must read as
    /// finished or the view would say it is still working forever.
    #[test]
    fn every_directory_in_the_tree_reads_as_complete() {
        let t = build(&[("a.txt", 1), ("src/deep/c.rs", 2)], 1);
        for p in [ROOT, "/repo/src", "/repo/src/deep"] {
            assert!(t.is_complete(Path::new(p)), "{p} should be complete");
            assert!(t.total_of(Path::new(p)).1, "{p} should report a settled total");
        }
    }

    /// Ancestors above the root exist only to hang it off. Leaving them unlisted
    /// is what keeps the 3D view's anchor pinned to the root instead of drifting
    /// up the disk as soon as the scene is built.
    #[test]
    fn ancestors_above_the_root_are_left_unlisted() {
        let t = build(&[("a.txt", 1)], 1);
        let above = t.get(Path::new("/")).expect("the root is hung off its parents");
        assert!(!above.listed, "nothing enumerated the filesystem root");
        assert!(!above.complete);
        assert!(t.get(Path::new(ROOT)).unwrap().listed);
    }

    #[test]
    fn the_epoch_is_what_callers_compare_against() {
        // A directory count would collide between two revisions holding the same
        // number of them; a caller-chosen epoch cannot.
        let a = build(&[("x/a", 1)], 7);
        let b = build(&[("y/b", 9)], 8);
        assert_eq!(a.dirs_seen, 7);
        assert_eq!(b.dirs_seen, 8);
    }

    #[test]
    fn an_empty_revision_still_has_a_root_to_stand_on() {
        let t = build(&[], 1);
        assert_eq!(t.total_of(Path::new(ROOT)).0, 0);
        assert!(t.is_complete(Path::new(ROOT)), "an empty tree is a finished tree");
    }

    /// Going back in time makes directories smaller. The crawler's tree only
    /// ever grows, which is why a revision gets a *fresh* tree rather than an
    /// edit of the last one.
    #[test]
    fn a_later_tree_may_be_smaller_than_an_earlier_one() {
        let big = build(&[("a", 100), ("b", 100)], 1);
        let small = build(&[("a", 10)], 2);
        assert_eq!(big.total_of(Path::new(ROOT)).0, 200);
        assert_eq!(small.total_of(Path::new(ROOT)).0, 10);
        // And the file that went away is simply absent from the newer tree.
        assert!(small.get(Path::new("/repo/b")).is_none());
    }

    #[test]
    fn the_biggest_files_are_recorded_for_the_treemap_and_the_fsn_solids() {
        let t = build(&[("small.txt", 1), ("big.bin", 1_000), ("src/mid.rs", 100)], 1);
        let top = &t.get(Path::new(ROOT)).unwrap().top_files;
        assert_eq!(top.first().map(|f| f.rel.as_str()), Some("big.bin"));
        assert!(top.iter().any(|f| f.rel.ends_with("mid.rs")), "nested files count too");
    }
}
