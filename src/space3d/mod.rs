//! 3D directory-tree view: a panel view format that draws a directory and its
//! neighbourhood as a tree of boxes connected by lines, each box sized by the
//! directory's total size on disk.
//!
//! Like the Details and Tree formats, it describes the **other** panel: as you
//! navigate over there, the focus moves and the camera flies to it. Sizes come
//! from the shared cache in [`crate::sizes`], so the tree grows outward while
//! the background crawler is still working.
//!
//! The tree itself does not move as you navigate. It is laid out in a **world
//! of its own**, anchored a couple of levels above the current directory and
//! shrinking by a fixed factor at every level down, so each directory has a
//! place of its own in a fixed, self-similar structure: what is above is drawn
//! large, what is below small. Walking into a subdirectory does not re-form
//! that structure — it flies the **camera** to the subdirectory's own place in
//! it and closes in, which is why the zoom tightens the deeper you go.
//!
//! The anchor has to move eventually, or a long descent would run the whole
//! scene into the floating-point floor. When it does, the frame is re-based by
//! the similarity transform between the old layout and the new one — the camera
//! and every animating box carried through it — so the re-anchor is invisible:
//! the pixels on the frame after are the pixels on the frame before. See
//! [`Space3d::rebase`].
//!
//! Nothing here snaps. Node positions and box sizes are animated toward their
//! layout targets, and a node that has only just been discovered starts at its
//! parent's position — so a scan in progress reads as a tree growing rather than
//! as boxes flickering into existence.
//!
//! There are two looks, chosen by [`Space3dStyle`]:
//!
//! * **Cubes** — the original: shaded cubes hanging in the panel background,
//!   children spread on rings below their parent.
//! * **Spare no expense** — SGI IRIX's *fsn*: a camera looking across a ground
//!   plane under a sky gradient, directories as pale platforms standing on it
//!   joined by lines running over the ground, and the files inside them as
//!   smaller solids whose shape and colour say what kind of file they are.
//!
//! Both come from the same renderer, the same size cache, the same animation
//! and the same navigation — only the layout, the geometry and the background
//! differ. Everything style-dependent branches in `place`/`boxes`/`links` here
//! and in [`raster3d::render_scene`]; nothing else in the app knows which look
//! is on.

pub mod raster3d;
pub mod render;
pub mod vec3;

pub use crate::config::Space3dStyle;
use raster3d::{SceneBox, Shape};
use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;
use std::path::{Path, PathBuf};
use std::time::Instant;
use vec3::{V3, v3};

/// Where the camera is, expressed as an orbit around a target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CamPose {
    pub target: V3,
    pub dist: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl CamPose {
    pub fn eye(&self) -> V3 {
        let (sp, cp) = (self.pitch.sin(), self.pitch.cos());
        let (sy, cy) = (self.yaw.sin(), self.yaw.cos());
        self.target.add(v3(cp * cy, sp, cp * sy).scale(self.dist))
    }
}

/// Seconds for an animation to cover ~63 % of the distance left.
const TAU: f32 = 0.20;
const PITCH_MIN: f32 = 0.08;
const PITCH_MAX: f32 = 1.45;

/// Camera-distance limits, **as multiples of the current level's scale** rather
/// than as absolute world units.
///
/// Every level is drawn [`LEVEL_SCALE`] times the size of the one above it, so
/// a fixed world-space limit would mean something different at every depth: a
/// floor of one unit is a sensible closest approach at the top of the tree and
/// far outside the solar system five levels down. Multiplied by the scale of
/// the level the camera is on, the same numbers mean the same thing everywhere.
///
/// The floor is a little more than a full-size box's half-extent
/// ([`BOX_MAX`]), which is as close as the camera can come before it is inside
/// the directory it is looking at. It is a backstop, not the working limit:
/// how far the user may zoom is set as a multiple of the fitted distance in
/// [`Space3d::zoom()`], and this only catches what that cannot.
const DIST_MIN: f32 = 0.25;
const DIST_MAX: f32 = 14.0;

/// Vertical drop from one tree level to the next, at scale 1.
const LEVEL_DY: f32 = 0.95;

/// How much smaller each level is drawn than the one above it.
///
/// This is what makes the structure hold still while the camera moves through
/// it: a node's whole subtree is laid out relative to the node itself and
/// scaled by this factor per level, so the arrangement below any directory is
/// the *same* arrangement whether you are looking at it from two levels up or
/// standing on it. It is also what lets the anchor be re-based without a visible
/// jump — see [`Space3d::rebase`].
///
/// Chosen so that a level is unmistakably smaller than its parent while three
/// or four of them are still on screen together: at 0.7 a grandchild is about
/// half the size of its grandparent.
const LEVEL_SCALE: f32 = 0.70;

/// Most nodes the tree will lay out.
///
/// Past this it stops going deeper. A directory with thousands of entries three
/// levels down would otherwise bury the shape the view exists to show — and cost
/// a great deal to rasterize every frame.
const MAX_NODES: usize = 220;

/// How much of the panel the framed content should span. Short of the full
/// width so a box on the rim keeps a little air around it.
const FILL_TARGET: f32 = 0.92;

/// Most fading-out boxes kept at once, so walking quickly through directories
/// cannot pile them up without limit.
const MAX_GHOSTS: usize = 120;

/// Seconds for a box to fade in or out.
const FADE_TAU: f32 = 0.13;

/// Shortest gap between two rebuilds of the scene's image.
///
/// Rebuilding is the expensive half of this view, and the cost is not the
/// rasterizer — that is a few hundred microseconds — but what happens to the
/// picture afterwards. On a graphics terminal the raster is re-encoded and the
/// *whole image* is re-transmitted: for a half-screen panel that is several
/// megabytes of escape data per rebuild, and no terminal swallows those at the
/// rate a held-down arrow key produces them.
///
/// Which is exactly what a held-down arrow key in the other panel used to
/// produce, because this view highlights that panel's cursor and so its picture
/// genuinely changes on every keypress. Pinning rebuilds to the animation's own
/// frame rate keeps the highlight following along while leaving the frames in
/// between free to draw the panel that is actually being scrolled.
const MIN_REPAINT: std::time::Duration = std::time::Duration::from_millis(33);

/// How many frames a held-back paint keeps asking for.
///
/// One is all it takes when the view is being drawn — the next tick collects it
/// — so this is a ceiling for the case where nothing ever does: a panel behind a
/// dialog, on a hidden side, or too small to draw into. Counted in frames rather
/// than measured in time so the debt cannot outlive the ticker that is meant to
/// pay it off.
const REPAINT_GRACE: u8 = 4;

/// How many levels below the current directory the tree goes.
const DEPTH_BELOW: u8 = 2;

/// How many levels **above** the current directory the tree is anchored.
///
/// This is the context that no longer moves when you navigate: the directory
/// you are in, where it sits among its siblings, and where that sits in turn.
/// Only ancestors the crawler has actually enumerated count — one it merely
/// knows *of* has no measured size, and a box labelled "0 B" is worse than no
/// box — so near a filesystem root, or in the first seconds of a session, the
/// tree is anchored shallower than this.
const DEPTH_ABOVE: u8 = 2;

/// Children drawn under a directory on the **spine** — the chain from the
/// anchor down to the current directory.
///
/// The same cap all the way along, deliberately. Walking into a subdirectory
/// turns the current directory into an ancestor, and if ancestors were allowed
/// fewer children than the current directory, that step would quietly delete
/// the very boxes the user was just looking at — their siblings. One cap means
/// the spine's branching is drawn identically before and after the move.
const SPINE_CAP: usize = 18;

/// Children drawn under a directory **inside** the current one.
///
/// Much smaller than [`SPINE_CAP`], because this multiplies: the current
/// directory's eighteen children each carrying a fan of their own is already
/// most of the scene's box budget. Beyond the cap the smallest are left out;
/// see [`children_to_draw`].
const BRANCH_CAP: usize = 5;

/// Half-extent limits for a box, **before its level's scale is applied**.
///
/// Directory sizes span many orders of magnitude, so they are mapped
/// logarithmically and then **clamped between these two**. Without the clamp a
/// single huge directory flattens everything else into invisible specks, and an
/// empty one disappears entirely — the point of the view is comparing shapes,
/// which needs every box to stay on screen and clickable.
///
/// The mapping is against a directory's **siblings**, not against the whole
/// scene: a scene now spans several levels whose sizes differ by construction,
/// and normalising across all of them would peg the top of the scale to an
/// ancestor that by definition contains everything under it. Comparing like
/// with like also means a box's size never changes because something somewhere
/// else in the tree came into view.
const BOX_MIN: f32 = 0.05;
const BOX_MAX: f32 = 0.17;

// -- the fsn style ----------------------------------------------------------
//
// A separate set of constants rather than reused Cubes ones: a platform has to
// be wide enough to stand a grid of file solids on, which is several times what
// a Cubes box is, and the whole scene is spread over a ground plane instead of
// hanging in space.

/// Half-extent limits for a directory platform's footprint, before the room its
/// own file grid needs — or its level's scale — is taken into account.
const PLATFORM_MIN: f32 = 0.13;
const PLATFORM_MAX: f32 = 0.34;

/// How thick a platform slab is, at scale 1. Thin enough to read as a floor,
/// thick enough to catch the light on its edge rather than vanishing edge-on.
const PLATFORM_H: f32 = 0.042;

/// Gap between neighbouring subtrees, as a fraction of a platform's width.
const FSN_SIBLING_GAP: f32 = 0.55;

/// How far one level sits in front of the next, along the ground, at scale 1.
const FSN_ROW_GAP: f32 = 1.15;

/// How much of a platform's width its file grid spans. Short of the full
/// width, so the solids stand *on* the platform with a margin of floor showing
/// round them rather than teetering off the edge.
const GRID_FILL: f32 = 0.78;

/// Widest a grid cell may get, as a fraction of the platform's half-extent, so
/// a directory holding one or two files does not stand monoliths on it. Tied to
/// the platform rather than absolute: a fixed ceiling leaves a big directory
/// showing a handful of specks adrift on a wide slab.
const FILE_CELL_MAX: f32 = 0.55;

/// Height limits for a file solid at scale 1, mapped from its size the same way
/// a directory box is — logarithmically, then clamped, so a small file still
/// has a solid worth seeing and a huge one does not become a skyscraper.
const FILE_H_MIN: f32 = 0.030;
const FILE_H_MAX: f32 = 0.145;

/// How many file solids a platform stands, by where the directory sits.
///
/// A grid of fifty solids on every one of two hundred platforms would be an
/// unreadable carpet and a great deal to rasterize, so only the spine — the
/// chain from the anchor down to the current directory — and the current
/// directory's own children carry files at all.
///
/// The spine gets them, rather than only the current directory, so that walking
/// into a subdirectory does not strip the files off the platform you just left:
/// it is the nearest thing in shot, and watching its contents evaporate as the
/// camera passes over it is exactly the flicker this view is built to avoid.
fn file_cap(rel: Relation) -> usize {
    match rel {
        Relation::Spine => 14,
        Relation::Inside(1) => 8,
        _ => 0,
    }
}

/// Where a node sits relative to the current directory.
///
/// The one thing in this module that asks where the user is standing, and it
/// only ever decides how far a directory is opened up and how much detail it
/// carries. No placement rule reads it: which level a node is on, which parent
/// it hangs off and where it sits among its siblings come from the tree alone.
/// See [`Space3d::sync_from`] for the one way that detail still reaches the
/// layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The current directory, or an ancestor of it: the chain the camera has
    /// travelled down.
    Spine,
    /// Inside the current directory, this many levels down.
    Inside(u8),
    /// Off the spine and above the current directory — a sibling, an aunt. Drawn
    /// so the structure around you is visible, but never opened up.
    Aside,
}

/// Ceiling on file solids across the whole scene, whatever the per-platform
/// caps would allow.
const MAX_FILE_SOLIDS: usize = 256;

/// One directory in the tree.
#[derive(Debug, Clone)]
pub struct SceneNode {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    /// Index of this node's parent in the node list.
    pub parent: Option<usize>,
    /// Levels below the anchor this node sits at. The anchor itself is 0.
    pub depth: u8,
    /// How large this level is drawn: [`LEVEL_SCALE`] to the power of `depth`.
    ///
    /// Everything about a node scales with this — its box, the drop or the row
    /// gap to its children, how far apart those children are spread — which is
    /// what makes the subtree under a directory the same shape wherever it is
    /// seen from, and what lets the anchor be re-based by a single similarity
    /// transform.
    pub scale: f32,
    /// Where the layout wants this node; the drawn position chases it.
    pub target: V3,
    pub target_half: f32,
    /// Still being crawled.
    pub partial: bool,
    /// The directory the other panel is on.
    pub is_focus: bool,
    /// The directory the other panel's cursor is on.
    pub is_cursor: bool,
    /// Drawn to say where this is rather than as the subject: an ancestor of
    /// the current directory, or something hanging off one. Dimmed, so the
    /// structure around you is legible without competing with what is inside
    /// the directory the view is actually about.
    pub context: bool,
    /// The files sitting **directly** in this directory, largest first, for the
    /// solids the fsn style stands on its platform. Empty in the Cubes style,
    /// which draws directories alone, and empty on any platform past
    /// [`file_cap`]'s reach.
    pub files: Vec<FileSolid>,
    /// Half-extent of the platform's footprint in the fsn style; the drawn
    /// value chases it like `target_half` does.
    pub target_plat: f32,
}

/// One file standing on an fsn platform.
#[derive(Debug, Clone)]
pub struct FileSolid {
    pub name: String,
    pub size: u64,
}

/// A node's animated state, keyed by path so it survives a re-layout.
#[derive(Debug, Clone, Copy)]
struct Shown {
    pos: V3,
    half: f32,
    /// 0 on the frame it appears, rising to 1.
    fade: f32,
}

/// A box that has left the tree and is on its way out.
///
/// Kept and drawn for a few frames after the directory it belonged to stops
/// being part of the scene, so a change of directory dissolves instead of
/// cutting. Ghosts take no part in navigation — they are not in `nodes`.
struct Ghost {
    node: SceneNode,
    pos: V3,
    half: f32,
    fade: f32,
}

/// Panel-view state for the 3D tree.
pub struct Space3d {
    /// The directory the *other* panel is on — what the view describes.
    pub focus: PathBuf,
    /// The directory that panel's *cursor* is sitting on, when it is on one.
    /// Highlighted in the scene, so you can see where you are about to go
    /// before you go there.
    pub cursor: Option<PathBuf>,
    /// Directory the laid-out tree hangs from: an ancestor of the focus, up to
    /// [`DEPTH_ABOVE`] levels above it. The world is anchored here, and stays
    /// anchored here while the focus moves within reach of it — see
    /// [`Space3d::rebase`] for what happens when it has to move.
    root: PathBuf,
    pub nodes: Vec<SceneNode>,
    pub selected: usize,
    /// Selection tracked by path, because the node list is rebuilt as the crawl
    /// discovers directories and a bare index would drift onto another one.
    sel_path: Option<PathBuf>,
    pub cam: CamPose,
    goal: CamPose,
    settled: bool,
    last: Instant,
    /// When the scene's image was last rebuilt, and how many more frames a
    /// rebuild held back by [`MIN_REPAINT`] should keep asking for — see
    /// [`Space3d::claim_repaint`].
    painted: Instant,
    repaint_owed: u8,
    shown: HashMap<PathBuf, Shown>,
    ghosts: Vec<Ghost>,
    /// Width / height of the raster the scene is drawn into. The camera fit
    /// depends on it, so a resized panel re-frames rather than cropping.
    aspect: f32,
    /// Radius of what the camera is framing, from the last layout.
    fit_radius: f32,
    /// The boxes being framed, as `(min, max)` corners. Kept so the fit can be
    /// solved against their real projected size rather than a bounding sphere,
    /// which on a flat wide scene leaves a tall panel half empty.
    ///
    /// Real boxes rather than centre-and-radius, because an fsn platform is a
    /// wide flat slab: treating it as a cube would claim it is as tall as it is
    /// broad and push the camera back until the scene was a smudge.
    fit_pts: Vec<(V3, V3)>,
    /// The user's own zoom, multiplied onto the fitted distance so it survives
    /// a resize and a change of directory.
    zoom: f32,
    /// The scale of the level the camera is on, from the last aim. The distance
    /// limits are multiples of this rather than absolute, so "as close as you
    /// may get" means the same thing at every depth.
    focus_scale: f32,
    /// Projected pixel bounds per node from the last render, for arrow
    /// navigation and mouse hit-testing.
    pub bounds: Vec<raster3d::Bounds>,
    pub bounds_px: (u32, u32),
    pub synced_at: u64,
    /// The crawler has not finished sizing the focus directory yet.
    pub scanning: bool,
    /// Which look to draw. Pushed in from the config every loop iteration, so
    /// changing the setting re-forms the scene under the user rather than
    /// waiting for the panel to be rebuilt.
    pub style: Space3dStyle,
}

impl Space3d {
    pub fn new(focus: PathBuf) -> Space3d {
        let goal = default_pose(Space3dStyle::Cubes);
        Space3d {
            root: focus.clone(),
            focus,
            cursor: None,
            nodes: Vec::new(),
            selected: 0,
            sel_path: None,
            cam: goal,
            goal,
            settled: false,
            last: Instant::now(),
            // A view that has never been painted must not have its first paint
            // held back, so it starts a full interval in the past. `checked_sub`
            // because `Instant` is monotonic from boot and subtracting past
            // zero panics.
            painted: Instant::now().checked_sub(MIN_REPAINT).unwrap_or_else(Instant::now),
            repaint_owed: 0,
            shown: HashMap::new(),
            ghosts: Vec::new(),
            aspect: 1.0,
            fit_radius: 1.0,
            fit_pts: Vec::new(),
            zoom: 1.0,
            focus_scale: 1.0,
            bounds: Vec::new(),
            bounds_px: (0, 0),
            synced_at: u64::MAX,
            scanning: true,
            style: Space3dStyle::Cubes,
        }
    }

    /// Switch which look is drawn.
    ///
    /// The two styles lay their nodes out completely differently, so this
    /// re-runs the layout from the cache and re-frames the camera. Cheap and
    /// idempotent when the style has not actually changed, which matters
    /// because the config pushes it in on every loop iteration.
    pub fn set_style(&mut self, style: Space3dStyle) {
        if self.style == style {
            return;
        }
        self.style = style;
        // Nodes keep their identity across the switch, so the boxes slide and
        // resize into their new arrangement rather than blinking out and back.
        self.synced_at = u64::MAX;
        let (yaw, pitch) = {
            let p = default_pose(style);
            (p.yaw, p.pitch)
        };
        self.goal.yaw = yaw;
        self.goal.pitch = pitch;
        self.settled = false;
    }

    /// Point the view at a directory (the other panel moved). Keeps every node
    /// that is still in the tree, so this is a camera flight rather than a cut.
    pub fn set_focus(&mut self, path: &Path) {
        if self.focus == path {
            return;
        }
        self.focus = path.to_path_buf();
        self.sel_path = None;
        self.synced_at = u64::MAX;
        self.settled = false;
    }

    /// Track the other panel's cursor. Cheap and called every loop iteration,
    /// so it does nothing at all when the cursor has not moved.
    pub fn set_cursor(&mut self, dir: Option<&Path>) {
        if self.cursor.as_deref() == dir {
            return;
        }
        self.cursor = dir.map(Path::to_path_buf);
        // Only the highlight moves — not the camera. Flying the view around on
        // every arrow keypress in the other panel would be unusable.
        self.synced_at = u64::MAX;
    }

    /// The angles the camera is heading toward, as `(yaw, pitch)` in radians.
    #[cfg(test)]
    pub fn goal_angles(&self) -> (f32, f32) {
        (self.goal.yaw, self.goal.pitch)
    }

    pub fn selected_node(&self) -> Option<&SceneNode> {
        self.nodes.get(self.selected)
    }

    /// What fraction of its parent the selected directory accounts for.
    ///
    /// Measured against the **parent**, so it means something wherever the
    /// cursor is — including on a directory with no subdirectories of its own,
    /// which measured against its own contents would always read 0 %.
    pub fn selected_share(&self) -> Option<f64> {
        let n = self.nodes.get(self.selected)?;
        let parent = self.nodes.get(n.parent?)?;
        (parent.size > 0).then(|| n.size as f64 / parent.size as f64 * 100.0)
    }

    // -- layout -------------------------------------------------------------

    /// Rebuild the tree from the shared size cache.
    ///
    /// The tree hangs from an **anchor** a couple of levels above the current
    /// directory rather than from the current directory itself, and each level
    /// below the anchor is drawn [`LEVEL_SCALE`] times the size of the one
    /// above it. No placement rule here asks where the focus is — which level
    /// a directory is on, which parent it hangs off, where it sits among its
    /// siblings and how big its box is all fall out of the tree alone. Walking
    /// about moves the camera, not the scenery.
    ///
    /// What the focus *does* decide is how much of the tree is opened up and
    /// how much detail each part of it carries; see [`Relation`]. That has one
    /// knock-on effect worth being straight about: an opened-up branch needs
    /// more room than a closed one, so the ring or row holding it widens a
    /// little as you step onto it and closes again as you step off. It is a
    /// breath, not a rearrangement — nothing changes level, changes parent or
    /// changes places with anything else — and it happens while the camera is
    /// already flying, which is when it is least visible. The alternative is a
    /// layout that reserves the same room for every directory whether or not
    /// anything is drawn inside it, and that spreads the scene so far that
    /// none of it is legible.
    ///
    /// Children are ordered by name, not by size: a name order is stable while
    /// the crawler is still revising sizes, so the tree grows in place instead
    /// of reshuffling under the cursor. Size is carried by the box.
    pub fn sync_from(&mut self, tree: &crate::sizes::SizeTree) {
        self.scanning = !tree.total_of(&self.focus).1;
        let mut root = anchor_for(tree, &self.focus);
        let (mut nodes, mut kids) = self.grow(tree, &root);
        if !nodes.iter().any(|n| n.is_focus) {
            // The way down from the anchor does not reach the current
            // directory — it was made after its parent was last listed, say, so
            // the crawler does not know it is there yet. The view is about that
            // directory before it is about anything else, so the context is
            // what gives way: hang the world from the directory itself until
            // the next pass over the cache finds it where it belongs.
            root = self.focus.clone();
            (nodes, kids) = self.grow(tree, &root);
        }

        scale_boxes(&mut nodes, &kids);
        if self.style == Space3dStyle::Fsn {
            // Only the spine and the focus's own children carry files; see
            // `file_cap`.
            let mut budget = MAX_FILE_SOLIDS;
            for n in nodes.iter_mut() {
                let cap = file_cap(self.relation(&n.path)).min(budget);
                if cap == 0 {
                    continue;
                }
                n.files = own_files(tree, &n.path, cap);
                budget -= n.files.len();
            }
            scale_platforms(&mut nodes, &kids);
            place_fsn(&mut nodes, &kids);
        } else {
            place(&mut nodes, &kids);
        }

        // A moved anchor is a change of coordinates, not a change of scene: put
        // the camera and everything mid-animation into the new frame so the
        // frame after looks exactly like the frame before.
        if root != self.root {
            self.rebase(&nodes);
        }
        self.root = root;

        let live: std::collections::HashSet<PathBuf> =
            nodes.iter().map(|n| n.path.clone()).collect();
        // A directory that has left the scene keeps its box for a few frames and
        // fades out where it stood, rather than blinking away.
        let departing = std::mem::replace(&mut self.nodes, nodes);
        for old in departing {
            if live.contains(&old.path) {
                continue;
            }
            let st = self.shown.get(&old.path).copied();
            self.ghosts.push(Ghost {
                pos: st.map_or(old.target, |s| s.pos),
                half: st.map_or(old.target_half, |s| s.half),
                fade: st.map_or(1.0, |s| s.fade),
                node: old,
            });
        }
        // One that has come back cancels its own ghost, so it is not drawn twice.
        self.ghosts.retain(|g| !live.contains(&g.node.path));
        // Rapid navigation must not pile them up without limit.
        if self.ghosts.len() > MAX_GHOSTS {
            let cut = self.ghosts.len() - MAX_GHOSTS;
            self.ghosts.drain(..cut);
        }
        self.shown.retain(|k, _| live.contains(k.as_path()));

        // Seed newcomers where their parent is, at nothing, fully transparent —
        // done here rather than on the first `advance` because a frame can be
        // drawn before the animation is ever stepped, and that frame would
        // otherwise show them at full size.
        for i in 0..self.nodes.len() {
            if self.shown.contains_key(&self.nodes[i].path) {
                continue;
            }
            let start = match self.nodes[i].parent {
                Some(p) => {
                    self.shown.get(&self.nodes[p].path).map_or(self.nodes[p].target, |s| s.pos)
                }
                None => self.nodes[i].target,
            };
            self.shown
                .insert(self.nodes[i].path.clone(), Shown { pos: start, half: 0.0, fade: 0.0 });
        }

        self.restore_selection();
        self.aim_camera();
        self.settled = false;
    }

    /// Grow the tree downward from `root`, opening each directory as far as
    /// [`expand_cap`] allows and stopping at [`MAX_NODES`].
    ///
    /// Breadth-first, so what runs into the node ceiling is the deepest and
    /// smallest level rather than an arbitrary branch.
    fn grow(
        &self,
        tree: &crate::sizes::SizeTree,
        root: &Path,
    ) -> (Vec<SceneNode>, Vec<Vec<usize>>) {
        let sel = self.sel_path.clone();
        let (rsize, rcomplete) = tree.total_of(root);
        let mut nodes =
            vec![self.node_for(display_name(root), root.to_path_buf(), rsize, rcomplete, 0)];
        let mut kids: Vec<Vec<usize>> = vec![Vec::new()];

        let mut i = 0;
        while i < nodes.len() && nodes.len() < MAX_NODES {
            let cap = expand_cap(self.relation(&nodes[i].path));
            if cap == 0 {
                i += 1;
                continue;
            }
            let ppath = nodes[i].path.clone();
            let depth = nodes[i].depth + 1;
            for k in children_to_draw(
                tree,
                &ppath,
                cap,
                &self.focus,
                sel.as_deref(),
                self.cursor.as_deref(),
            ) {
                if nodes.len() >= MAX_NODES {
                    break;
                }
                let mut n = self.node_for(k.name, k.path, k.total, k.complete, depth);
                n.parent = Some(i);
                nodes.push(n);
                kids.push(Vec::new());
                let ci = nodes.len() - 1;
                kids[i].push(ci);
            }
            i += 1;
        }
        (nodes, kids)
    }

    /// Where a directory sits relative to the one the view is about.
    fn relation(&self, path: &Path) -> Relation {
        // An ancestor of the focus — or the focus itself, which `starts_with`
        // also covers.
        if self.focus.starts_with(path) {
            return Relation::Spine;
        }
        match path.strip_prefix(&self.focus) {
            Ok(rest) => Relation::Inside(rest.components().count().min(u8::MAX as usize) as u8),
            Err(_) => Relation::Aside,
        }
    }

    /// Move the world onto a new anchor without anything appearing to move.
    ///
    /// Every offset in the layout is proportional to the scale of the node it
    /// hangs off, so the arrangement under a directory is the *same*
    /// arrangement at every anchor — only translated and uniformly scaled.
    /// Which means a change of anchor is a similarity transform, and applying
    /// that same transform to the camera and to everything mid-animation
    /// leaves the projected image pixel-for-pixel unchanged. Without it, the
    /// re-anchor that keeps a long descent off the floating-point floor would
    /// be a jump cut in the middle of a camera flight.
    ///
    /// The transform is read off whichever directory is in **both** layouts and
    /// nearest what the camera is looking at. Any of them would do in an
    /// unchanged tree; picking the one under the eye means that when the tree
    /// has *also* moved a little — the crawler having found something new
    /// between one anchor and the next — the leftover error lands out at the
    /// edges of the frame rather than on the box being watched.
    fn rebase(&mut self, fresh: &[SceneNode]) {
        let pin = {
            let by_path: HashMap<&Path, &SceneNode> =
                fresh.iter().map(|n| (n.path.as_path(), n)).collect();
            let at = self.cam.target;
            self.nodes
                .iter()
                .filter_map(|old| by_path.get(old.path.as_path()).map(|new| (old, *new)))
                .filter(|(old, _)| old.scale > 0.0)
                .min_by(|a, b| a.0.target.sub(at).len().total_cmp(&b.0.target.sub(at).len()))
                .map(|(old, new)| (old.target, new.target, new.scale / old.scale))
        };
        // Nothing in common — a jump to an unrelated part of the filesystem.
        // There is no shared frame to preserve, so the scene simply re-forms.
        let Some((from, to, k)) = pin else {
            return;
        };
        if !k.is_finite() || k <= 0.0 {
            return;
        }
        let map = |p: V3| to.add(p.sub(from).scale(k));
        for st in self.shown.values_mut() {
            st.pos = map(st.pos);
            st.half *= k;
        }
        for g in self.ghosts.iter_mut() {
            g.pos = map(g.pos);
            g.half *= k;
            // Its own copy of the node, kept only for drawing — and the slab
            // thickness read off it has to move into the new frame as well.
            g.node.scale *= k;
        }
        self.cam.target = map(self.cam.target);
        self.cam.dist *= k;
        self.goal.target = map(self.goal.target);
        self.goal.dist *= k;
    }

    fn node_for(
        &self,
        name: String,
        path: PathBuf,
        size: u64,
        complete: bool,
        depth: u8,
    ) -> SceneNode {
        SceneNode {
            is_focus: path == self.focus,
            is_cursor: self.cursor.as_deref() == Some(path.as_path()),
            // Anything that is not the current directory or inside it is drawn
            // to say where you are, not to be read as the subject.
            context: !path.starts_with(&self.focus),
            name,
            size,
            parent: None,
            depth,
            scale: LEVEL_SCALE.powi(depth as i32),
            target: v3(0.0, 0.0, 0.0),
            target_half: BOX_MIN,
            partial: !complete,
            files: Vec::new(),
            target_plat: PLATFORM_MIN,
            path,
        }
    }

    fn restore_selection(&mut self) {
        let want = self.sel_path.clone().unwrap_or_else(|| self.focus.clone());
        self.selected = self.nodes.iter().position(|n| n.path == want).unwrap_or(0);
        self.sel_path = self.nodes.get(self.selected).map(|n| n.path.clone());
    }

    fn remember_selection(&mut self) {
        self.sel_path = self.nodes.get(self.selected).map(|n| n.path.clone());
    }

    /// Aim the camera at the current directory and what is inside it.
    ///
    /// Only those: not the ancestors above, which the fixed layout now draws
    /// at full size a level or two up. Framing them too would mean the camera
    /// pulling back far enough to hold a box several times the size of the one
    /// the view is about, every time — and the further down the tree you went,
    /// the worse it would get.
    ///
    /// Fitted to what the boxes actually occupy rather than guessed from the
    /// child count, so the directory fills the panel whether it has two
    /// subdirectories or two hundred. Since every level is drawn smaller than
    /// the one above it, that fit alone is what tightens the zoom as you
    /// descend: no separate rule decides how close the camera gets.
    fn aim_camera(&mut self) {
        let Some(fi) = self.nodes.iter().position(|n| n.is_focus) else {
            return;
        };
        let framed: Vec<&SceneNode> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| *i == fi || n.parent == Some(fi))
            .map(|(_, n)| n)
            .collect();
        if framed.is_empty() {
            return;
        }
        // Everything from here on is measured in multiples of the focus's own
        // level scale, so the framing means the same thing at any depth.
        self.focus_scale = self.nodes[fi].scale.max(f32::MIN_POSITIVE);
        let fsn = self.style == Space3dStyle::Fsn;
        // What each framed node actually occupies. In the fsn style that is a
        // slab standing on the ground with its files on top, not a cube around
        // its centre.
        let extent = |n: &SceneNode| -> (V3, V3) {
            if fsn {
                let h = n.target_plat;
                let top =
                    (PLATFORM_H + if n.files.is_empty() { 0.0 } else { FILE_H_MAX }) * n.scale;
                (v3(n.target.x - h, 0.0, n.target.z - h), v3(n.target.x + h, top, n.target.z + h))
            } else {
                let h = n.target_half;
                (
                    v3(n.target.x - h, n.target.y - h, n.target.z - h),
                    v3(n.target.x + h, n.target.y + h, n.target.z + h),
                )
            }
        };
        let mut boxes: Vec<(V3, V3)> = framed.iter().map(|n| extent(n)).collect();
        if fsn {
            // Stand back far enough to *look at* the focus's platform rather
            // than stand on it.
            //
            // A slab seen along the ground has its near edge racing toward the
            // eye as the camera closes in, so a fit solved against the platform
            // and its children alone settles with that near edge jammed against
            // the bottom of the frame and the children a smudge on the horizon.
            // Reserving the room the row *behind* would occupy — without
            // framing that row, which is an ancestor and drawn large — puts the
            // camera where the scene reads as a corridor receding in front of
            // it, which is the whole picture this style is for.
            let f = &self.nodes[fi];
            let standoff = FSN_ROW_GAP * gap_scale(f.scale / LEVEL_SCALE);
            let behind = v3(f.target.x, 0.0, f.target.z - standoff);
            boxes.push((behind, behind));
        }
        let mut lo = boxes[0].0;
        let mut hi = boxes[0].1;
        for (a, b) in &boxes {
            lo = v3(lo.x.min(a.x), lo.y.min(a.y), lo.z.min(a.z));
            hi = v3(hi.x.max(b.x), hi.y.max(b.y), hi.z.max(b.z));
        }
        let centre = lo.add(hi).scale(0.5);
        // Aim a little above the ground in the fsn style, so the camera looks
        // *across* the scene rather than down at the plane it stands on — which
        // is what keeps the horizon in frame.
        self.goal.target =
            if fsn { v3(centre.x, hi.y.max(0.12 * self.focus_scale), centre.z) } else { centre };
        self.fit_radius = hi.sub(lo).scale(0.5).len().max(0.35 * self.focus_scale);
        self.fit_pts = boxes;
        self.refit();
    }

    /// Tell the view what shape it is being drawn into.
    ///
    /// The vertical field of view is fixed, so the horizontal one follows the
    /// raster's aspect — which means a panel that is resized narrower would crop
    /// the scene, and one resized wider would leave it stranded in the middle.
    /// Re-fitting on a change keeps it filling whatever it is given.
    pub fn set_viewport(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let aspect = w as f32 / h as f32;
        if (aspect - self.aspect).abs() < 1e-3 {
            return;
        }
        self.aspect = aspect;
        self.refit();
    }

    /// Recompute the camera distance from what is being framed, the shape of
    /// the panel, and the user's own zoom.
    ///
    /// A bounding-sphere fit is only the starting guess. The scene is a wide
    /// flat arrangement, not a ball, so on a tall panel a sphere fit leaves most
    /// of the height empty and on a wide one it wastes the width. Solving
    /// against the *projected* extent instead makes it fill whatever shape it is
    /// given, in both directions, without ever cropping.
    ///
    /// Solved by bracketing and bisection rather than by scaling the distance by
    /// the overshoot. That shortcut assumes the projected size goes as `1/d`,
    /// which only holds when the scene is small compared with how far away it
    /// is. The fsn style breaks the assumption outright — a wide arrangement
    /// lying on the ground, seen along it from a shallow angle, has its near
    /// edge racing toward the eye plane as the camera closes in — and the
    /// iteration then oscillates between too near and too far instead of
    /// settling. `projected_fill` is monotone in `d`, so bisection always
    /// converges.
    fn refit(&mut self) {
        let guess = fit_dist(self.fit_radius, self.aspect);
        let fill = |d: f32| self.projected_fill(d).filter(|f| f.is_finite() && *f > 1e-4);
        let mut d = guess;
        if let Some(f0) = fill(guess) {
            // Bracket the target: `fill` falls as the camera pulls back, so walk
            // whichever way this guess is wrong until the answer is straddled.
            let (mut lo, mut hi) = (guess, guess);
            if f0 > FILL_TARGET {
                // Too big on screen — the camera has to go further out.
                for _ in 0..24 {
                    hi *= 1.35;
                    match fill(hi) {
                        Some(f) if f > FILL_TARGET => lo = hi,
                        _ => break,
                    }
                }
            } else {
                for _ in 0..24 {
                    lo /= 1.35;
                    if lo <= DIST_MIN * self.focus_scale * 0.5 {
                        break;
                    }
                    match fill(lo) {
                        Some(f) if f <= FILL_TARGET => hi = lo,
                        _ => break,
                    }
                }
            }
            // 30 halvings take any bracket down to well under a pixel.
            for _ in 0..30 {
                let mid = (lo + hi) * 0.5;
                if (hi - lo) < lo * 1e-3 {
                    break;
                }
                match fill(mid) {
                    Some(f) if f > FILL_TARGET => lo = mid,
                    Some(_) => hi = mid,
                    None => break,
                }
            }
            d = hi;
        }
        if !d.is_finite() || d <= 0.0 {
            d = guess;
        }
        self.goal.dist =
            (d * self.zoom).clamp(DIST_MIN * self.focus_scale, DIST_MAX * self.focus_scale);
        self.settled = false;
    }

    /// The largest fraction of the raster the framed boxes would cover at
    /// distance `d`: 1.0 means they exactly touch an edge, more than that means
    /// they would be cropped.
    ///
    /// A corner that falls **behind the eye** reads as `f32::MAX` rather than
    /// as a failure. It is the honest answer — a scene the camera is standing
    /// inside does not fit in any frame — and it is what keeps the bisection
    /// in [`Space3d::refit`] working: closing in on a wide arrangement seen
    /// along its length, which is exactly what the fsn style does at depth,
    /// swallows the near corners first, and treating that as "no answer" left
    /// the camera wherever the search happened to have got to.
    fn projected_fill(&self, d: f32) -> Option<f32> {
        if self.fit_pts.is_empty() || !d.is_finite() || d <= 0.0 {
            return None;
        }
        // A nominal raster of the right shape: only the ratio matters here.
        let (fw, fh) = (1000.0 * self.aspect.max(0.05), 1000.0f32);
        let focal = vec3::focal_for(fh, raster3d::FOV_Y);
        let pose = CamPose { dist: d, ..self.goal };
        let basis = vec3::look_at(pose.eye(), pose.target, v3(0.0, 1.0, 0.0));
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for (lo, hi) in &self.fit_pts {
            for corner in [
                v3(lo.x, lo.y, lo.z),
                v3(hi.x, lo.y, lo.z),
                v3(lo.x, lo.y, hi.z),
                v3(hi.x, lo.y, hi.z),
                v3(lo.x, hi.y, lo.z),
                v3(hi.x, hi.y, lo.z),
                v3(lo.x, hi.y, hi.z),
                v3(hi.x, hi.y, hi.z),
            ] {
                let Some((x, y, _)) = vec3::project(vec3::to_view(&basis, corner), fw, fh, focal)
                else {
                    return Some(f32::MAX);
                };
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        // Measured about the centre, so the result says how much would have to
        // shrink to fit, not merely how big the content is.
        let half_w = (x1 - fw * 0.5).abs().max((fw * 0.5 - x0).abs());
        let half_h = (y1 - fh * 0.5).abs().max((fh * 0.5 - y0).abs());
        Some((half_w / (fw * 0.5)).max(half_h / (fh * 0.5)))
    }

    // -- animation ----------------------------------------------------------

    pub fn needs_frames(&self) -> bool {
        !self.settled || !self.ghosts.is_empty() || self.repaint_owed > 0
    }

    /// Ask permission to rebuild the scene's image this frame.
    ///
    /// `true` grants it and starts a fresh [`MIN_REPAINT`] interval; `false`
    /// means draw whatever image is already there. A refusal is remembered for
    /// [`REPAINT_GRACE`] frames, so [`needs_frames`] keeps asking for the frame
    /// that delivers it — without that, a scene that went quiet in the same
    /// moment it was refused would sit on a stale picture for good.
    ///
    /// Only call this when the image would actually come out different;
    /// an unchanged one is already free.
    ///
    /// [`needs_frames`]: Space3d::needs_frames
    pub fn claim_repaint(&mut self, now: Instant) -> bool {
        if now.duration_since(self.painted) < MIN_REPAINT {
            self.repaint_owed = REPAINT_GRACE;
            return false;
        }
        self.mark_painted(now);
        true
    }

    /// Record that the scene's image has just been built, starting a fresh
    /// interval. For the first paint of all, which nothing precedes and which
    /// must therefore not be rationed.
    pub fn mark_painted(&mut self, now: Instant) {
        self.painted = now;
        self.repaint_owed = 0;
    }

    /// Settle a held-back paint that has since happened by other means.
    ///
    /// The cell-art fallback calls this. It is not rationed — it rebuilds every
    /// frame, because there is no image to ship the terminal, only cells the
    /// frame diff collapses anyway — so it owes no interval of its own and takes
    /// no clock. What it must not do is leave the view asking for frames to
    /// deliver a paint that is already on screen.
    pub fn clear_repaint_debt(&mut self) {
        self.repaint_owed = 0;
    }

    /// Advance the camera, node positions and box sizes toward their targets.
    ///
    /// Exponential smoothing on a wall-clock delta, so it behaves the same at
    /// 10 fps and at 30 — which matters because this app's frame rate is
    /// event-driven and varies widely.
    pub fn advance(&mut self, now: Instant) {
        // The frame a held-back paint was asking for has come round; whether the
        // draw that follows collects it or not, it is one frame less owed.
        self.repaint_owed = self.repaint_owed.saturating_sub(1);
        let dt = (now - self.last).as_secs_f32().clamp(0.0, 0.1);
        self.last = now;
        if dt <= 0.0 {
            return;
        }
        let t = 1.0 - (-dt / TAU).exp();

        self.cam.target = self.cam.target.lerp(self.goal.target, t);
        // Distance is smoothed **geometrically**, not linearly: a descent now
        // asks the camera to close from one level's framing to the next, which
        // is a constant *ratio* whatever the depth. A linear lerp over that
        // covers most of the ground in the first frames and then crawls — it
        // reads as a lurch — where a constant ratio per unit time reads as a
        // steady dive, and behaves identically at every scale.
        if self.cam.dist > 0.0 && self.goal.dist > 0.0 {
            self.cam.dist *= (self.goal.dist / self.cam.dist).powf(t);
        } else {
            self.cam.dist = self.goal.dist;
        }
        self.cam.pitch += (self.goal.pitch - self.cam.pitch) * t;
        // Always orbit the short way round.
        self.cam.yaw += vec3::wrap_angle(self.goal.yaw - self.cam.yaw) * t;

        // Distances and positions are measured against the camera's own
        // distance rather than against a fixed epsilon: at five levels down the
        // whole scene is a few hundredths of a unit across, and an absolute
        // tolerance would call a flight finished before it had visibly begun.
        let near = (self.goal.dist * 1e-3).max(f32::MIN_POSITIVE);
        let mut moving = self.cam.target.sub(self.goal.target).len() > near
            || (self.cam.dist - self.goal.dist).abs() > near
            || (self.cam.pitch - self.goal.pitch).abs() > 5e-4
            || vec3::wrap_angle(self.goal.yaw - self.cam.yaw).abs() > 5e-4;

        let f = 1.0 - (-dt / FADE_TAU).exp();

        for n in &self.nodes {
            let cur = self.shown.entry(n.path.clone()).or_insert(Shown {
                pos: n.target,
                half: 0.0,
                fade: 0.0,
            });
            cur.pos = cur.pos.lerp(n.target, t);
            cur.half += (n.target_half - cur.half) * t;
            cur.fade += (1.0 - cur.fade) * f;
            // Relative to the box itself, for the same reason the camera's
            // tolerance is relative: a deep box is a fraction of the size of a
            // shallow one, and a fixed epsilon would freeze it mid-flight.
            let close = (n.target_half * 1e-2).max(f32::MIN_POSITIVE);
            let settled_here = cur.pos.sub(n.target).len() <= close
                && (cur.half - n.target_half).abs() <= n.target_half * 1e-3
                && cur.fade >= 0.999;
            if settled_here {
                cur.pos = n.target;
                cur.half = n.target_half;
                cur.fade = 1.0;
            } else {
                moving = true;
            }
        }

        // Boxes on their way out shrink where they stand and dissolve; once
        // they are invisible they are dropped for good.
        for g in self.ghosts.iter_mut() {
            g.fade -= g.fade * f;
            g.half -= g.half * f;
        }
        let before = self.ghosts.len();
        self.ghosts.retain(|g| g.fade > 0.02);
        if !self.ghosts.is_empty() || self.ghosts.len() != before {
            moving = true;
        }

        if !moving {
            self.cam = self.goal;
            self.settled = true;
        }
    }

    fn drawn(&self, i: usize) -> (V3, f32) {
        let n = &self.nodes[i];
        match self.shown.get(&n.path) {
            Some(s) => (s.pos, s.half),
            None => (n.target, n.target_half),
        }
    }

    /// The boxes to draw, at their current animated positions.
    ///
    /// Order is load-bearing: every real node first, then the fading-out
    /// ghosts, then — in the fsn style — the file solids. `bounds` is built in
    /// step with this list and navigation only ever looks at the first
    /// `nodes.len()` of it, so everything after the nodes is scenery that
    /// cannot be selected or clicked.
    pub fn boxes(&self, pal: &ScenePalette) -> Vec<SceneBox> {
        let fsn = self.style == Space3dStyle::Fsn;
        let mut out: Vec<SceneBox> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let (p, h) = self.drawn(i);
                let sel = i == self.selected;
                // The selection is marked by the bright outline, not by
                // repainting the box: a flat accent-coloured slab would lose the
                // face shading that makes it read as solid.
                // The directory the view is about has to be findable at a
                // glance in a field of otherwise similar boxes, so it takes the
                // accent colour outright rather than a tint of it.
                let base = if n.is_focus {
                    crate::ui::graphics::raster::over(hue_for(&n.name), pal.accent, 0.8)
                } else {
                    hue_for(&n.name)
                };
                let (min, max, color) = if fsn {
                    // A platform: as wide as its footprint, barely off the
                    // ground. It is pale, near-white like fsn's pedestals, but
                    // carrying a breath of the directory's own hue so a wall of
                    // them is not undifferentiated.
                    let pale = crate::ui::graphics::raster::over(base, pal.platform, 0.82);
                    let c = if n.is_focus {
                        crate::ui::graphics::raster::over(pale, pal.accent, 0.35)
                    } else {
                        pale
                    };
                    // `h` animates from zero, so a platform grows out of the
                    // ground rather than being there from the first frame.
                    let grow = (h / n.target_plat.max(1e-4)).clamp(0.0, 1.0);
                    let lift = PLATFORM_H * n.scale * grow;
                    (v3(p.x - h, 0.0, p.z - h), v3(p.x + h, lift, p.z + h), c)
                } else {
                    (v3(p.x - h, p.y - h, p.z - h), v3(p.x + h, p.y + h, p.z + h), base)
                };
                SceneBox {
                    name: n.name.clone(),
                    size_label: crate::util::bytes::human_size(n.size),
                    min,
                    max,
                    color,
                    selected: sel,
                    focus: n.is_focus,
                    cursor: n.is_cursor,
                    partial: n.partial,
                    dim: n.context,
                    fade: self.shown.get(&n.path).map_or(1.0, |s| s.fade),
                    shape: Shape::Block,
                }
            })
            // Fading-out boxes ride along after every real node — `bounds` is
            // indexed in step with this, and navigation only looks at the first
            // `nodes.len()` of them.
            .chain(self.ghosts.iter().map(|g| {
                let (min, max) = if fsn {
                    (
                        v3(g.pos.x - g.half, 0.0, g.pos.z - g.half),
                        v3(g.pos.x + g.half, PLATFORM_H * g.node.scale, g.pos.z + g.half),
                    )
                } else {
                    (
                        v3(g.pos.x - g.half, g.pos.y - g.half, g.pos.z - g.half),
                        v3(g.pos.x + g.half, g.pos.y + g.half, g.pos.z + g.half),
                    )
                };
                SceneBox {
                    name: g.node.name.clone(),
                    size_label: crate::util::bytes::human_size(g.node.size),
                    min,
                    max,
                    color: hue_for(&g.node.name),
                    selected: false,
                    focus: false,
                    cursor: false,
                    partial: g.node.partial,
                    dim: g.node.context,
                    fade: g.fade,
                    shape: Shape::Block,
                }
            }))
            .collect();
        if fsn {
            out.extend(self.file_solids(pal));
        }
        out
    }

    /// The file solids standing on the platforms, in the fsn style.
    ///
    /// Left nameless on purpose: a grid of labels at this size is an illegible
    /// smudge, and `label_slots` skips an empty name for free. What a solid is
    /// is said by its shape and its colour instead.
    fn file_solids(&self, pal: &ScenePalette) -> Vec<SceneBox> {
        let mut out = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.files.is_empty() {
                continue;
            }
            let (p, h) = self.drawn(i);
            // Grown in from nothing along with the platform it stands on.
            let grow = (h / n.target_plat.max(1e-4)).clamp(0.0, 1.0);
            let fade = self.shown.get(&n.path).map_or(1.0, |s| s.fade);
            if grow < 0.02 {
                continue;
            }
            let cols = grid_cols(n.files.len());
            let rows = n.files.len().div_ceil(cols.max(1));
            let cell = grid_step(n.target_plat, cols);
            let step = cell * grow;
            // Well short of the cell, so a row behind is still visible between
            // the shoulders of the row in front.
            let half = cell * 0.32 * grow;
            let lift = PLATFORM_H * n.scale * grow;
            for (k, f) in n.files.iter().enumerate() {
                let (c, r) = (k % cols, k / cols);
                let cx = p.x + (c as f32 - (cols as f32 - 1.0) * 0.5) * step;
                let cz = p.z + (r as f32 - (rows as f32 - 1.0) * 0.5) * step;
                let ext = std::path::Path::new(&f.name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let (shape, color) = file_look(ext, pal);
                out.push(SceneBox {
                    name: String::new(),
                    size_label: String::new(),
                    min: v3(cx - half, lift, cz - half),
                    max: v3(cx + half, lift + file_height(f.size) * n.scale * grow, cz + half),
                    color,
                    selected: false,
                    focus: false,
                    cursor: false,
                    partial: false,
                    dim: false,
                    fade,
                    shape,
                });
            }
        }
        out
    }

    /// Parent→child connector segments, at their current animated positions.
    pub fn links(&self) -> Vec<(V3, V3)> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let pi = n.parent?;
                let (cp, ch) = self.drawn(i);
                let (pp, ph) = self.drawn(pi);
                if self.style == Space3dStyle::Fsn {
                    // Across the ground, from the front edge of the parent's
                    // platform to the back edge of the child's, a hair above the
                    // surface so the line is not swallowed by it. Keyed to the
                    // *child*, the thinner of the two slabs, so the line skims
                    // the ground at both ends rather than floating over one.
                    let y = PLATFORM_H * n.scale * 0.6;
                    return Some((v3(pp.x, y, pp.z + ph), v3(cp.x, y, cp.z - ch)));
                }
                // Anchor on the facing faces rather than the centres, so the line
                // emerges from the boxes instead of vanishing inside them.
                Some((v3(pp.x, pp.y - ph, pp.z), v3(cp.x, cp.y + ch, cp.z)))
            })
            .collect()
    }

    // -- navigation ---------------------------------------------------------

    /// Move the selection to the nearest box in a screen direction, using the
    /// projected bounds from the last frame — so the arrows follow what is
    /// actually on screen, at whatever angle the camera happens to be.
    pub fn step(&mut self, dx: f32, dy: f32) {
        let Some(from) = self.centre_of(self.selected) else {
            return;
        };
        let mut best: Option<(f32, usize)> = None;
        for i in 0..self.nodes.len() {
            if i == self.selected {
                continue;
            }
            let Some(c) = self.centre_of(i) else { continue };
            let (vx, vy) = (c.0 - from.0, c.1 - from.1);
            let along = vx * dx + vy * dy;
            let across = (vx * dy - vy * dx).abs();
            if along <= 0.0 || along < across * 0.6 {
                continue;
            }
            let cost = along + across * 1.6;
            if best.is_none_or(|(b, _)| cost < b) {
                best = Some((cost, i));
            }
        }
        if let Some((_, i)) = best {
            self.selected = i;
            self.remember_selection();
        }
    }

    fn centre_of(&self, i: usize) -> Option<(f32, f32)> {
        self.bounds
            .get(i)
            .copied()
            .flatten()
            .map(|(x0, y0, x1, y1)| ((x0 + x1) * 0.5, (y0 + y1) * 0.5))
    }

    pub fn orbit(&mut self, dyaw: f32, dpitch: f32) {
        self.goal.yaw += dyaw;
        self.goal.pitch = (self.goal.pitch + dpitch).clamp(PITCH_MIN, PITCH_MAX);
        // Turning the scene changes how much room it needs on screen, so the
        // distance is solved again — otherwise orbiting crops it.
        self.refit();
    }

    pub fn zoom(&mut self, factor: f32) {
        // Held as a multiplier on the fitted distance rather than as a distance,
        // so zooming in and then resizing the panel (or changing directory)
        // keeps you zoomed in instead of snapping back to the default framing.
        self.zoom = (self.zoom * factor).clamp(0.25, 4.0);
        self.refit();
    }

    /// Back to the default angle, framing the focus again.
    pub fn reset_view(&mut self) {
        let pose = default_pose(self.style);
        self.goal.yaw = pose.yaw;
        self.goal.pitch = pose.pitch;
        self.zoom = 1.0;
        self.aim_camera();
    }

    /// Select the box whose projected silhouette contains a raster pixel. The
    /// smallest match wins, which is what the eye expects where boxes overlap.
    pub fn pick(&mut self, px: f32, py: f32) -> bool {
        let mut best: Option<(f32, usize)> = None;
        // Only the real nodes: `bounds` also covers the fading-out boxes on the
        // tail of the box list, which are not selectable.
        for (i, b) in self.bounds.iter().enumerate().take(self.nodes.len()) {
            let Some((x0, y0, x1, y1)) = *b else { continue };
            if px < x0 || px > x1 || py < y0 || py > y1 {
                continue;
            }
            let area = (x1 - x0) * (y1 - y0);
            if best.is_none_or(|(a, _)| area < a) {
                best = Some((area, i));
            }
        }
        match best {
            Some((_, i)) => {
                self.selected = i;
                self.remember_selection();
                true
            }
            None => false,
        }
    }
}

/// The camera angles a style opens at, and returns to on `Home`.
///
/// The fsn style looks *across* its world rather than down onto it: a shallow
/// pitch is what puts the horizon in frame and makes the ground read as ground.
/// `PITCH_MIN` is above zero, so the eye can never drop below the ground plane
/// and see the platforms from underneath.
fn default_pose(style: Space3dStyle) -> CamPose {
    match style {
        Space3dStyle::Cubes => {
            CamPose { target: v3(0.0, -LEVEL_DY, 0.0), dist: 3.6, yaw: -2.0, pitch: 0.62 }
        }
        // Looking along +Z, the direction the tree grows in.
        Space3dStyle::Fsn => {
            CamPose { target: v3(0.0, 0.0, 0.0), dist: 3.6, yaw: -FRAC_PI_2, pitch: 0.34 }
        }
    }
}

/// Camera distance that fits a sphere of `radius` into a raster of the given
/// aspect, with a little room to spare.
///
/// Both axes are checked and the larger distance wins: the vertical field of
/// view is fixed, so on a tall narrow panel the *horizontal* field is the tight
/// one, and fitting only the vertical would crop the sides.
fn fit_dist(radius: f32, aspect: f32) -> f32 {
    const MARGIN: f32 = 1.12;
    let half_v = raster3d::FOV_Y * 0.5;
    let half_h = (half_v.tan() * aspect.max(0.05)).atan();
    let d_v = radius / half_v.sin().max(1e-3);
    let d_h = radius / half_h.sin().max(1e-3);
    d_v.max(d_h) * MARGIN
}

/// Where the world is pinned: the current directory's ancestor [`DEPTH_ABOVE`]
/// levels up.
///
/// Only ancestors the crawler has actually **enumerated** count. One it merely
/// knows of — interned on the way down to the focus — has no measured size, and
/// anchoring the whole scene to a box labelled "0 B" would make every size in
/// it a lie. Near a filesystem root there may be nothing above at all, and the
/// world is then pinned to the current directory itself.
fn anchor_for(tree: &crate::sizes::SizeTree, focus: &Path) -> PathBuf {
    let mut root = focus.to_path_buf();
    for _ in 0..DEPTH_ABOVE {
        let Some(up) = root.parent().map(Path::to_path_buf) else {
            break;
        };
        if !tree.get(&up).is_some_and(|n| n.listed) {
            break;
        }
        root = up;
    }
    root
}

/// How many children a directory is drawn with, or zero for one that is drawn
/// but not opened up.
///
/// The spine is opened all the way along and the current directory's contents
/// for [`DEPTH_BELOW`] levels; everything else is a leaf. Off-spine branches
/// *above* the current directory are the ones this keeps shut: a scene that
/// expanded the aunts as well as the ancestors would be mostly other people's
/// business, and would cost more boxes than the subject itself.
fn expand_cap(rel: Relation) -> usize {
    match rel {
        Relation::Spine => SPINE_CAP,
        Relation::Inside(d) if d < DEPTH_BELOW => BRANCH_CAP,
        _ => 0,
    }
}

/// The scale a gap between one level and the next is measured at: the mean of
/// the two.
///
/// A gap belongs to neither level on its own — it has to clear the parent's box
/// at one end and the child's at the other — so taking either side's scale
/// alone gets it wrong in one direction or the other. The parent's leaves a row
/// of small boxes stranded across a gap sized for large ones; the child's runs
/// two rows into each other, because the row in front is drawn at the larger
/// scale and eats most of the clearance.
fn gap_scale(scale: f32) -> f32 {
    scale * (1.0 + LEVEL_SCALE) * 0.5
}

/// Lay the tree out: work out how much room each subtree needs, then hand it
/// that much.
///
/// Two passes, because a parent cannot space its children until it knows how
/// wide each of *their* subtrees is. Without that, a child with fifty
/// grandchildren of its own would spread them straight through its siblings.
///
/// Within a level the **largest** child takes the centre, and its siblings ring
/// around it outside its reach. Spacing every sibling as though it were as wide
/// as that one would scatter a dozen small directories across an enormous disc
/// and push the camera back until nothing was legible.
///
/// Largest by what it holds, which is a fact about the tree. The two rules this
/// replaces both asked about the moment rather than about the tree — the
/// current directory took the centre, or failing that the widest subtree did —
/// and either way stepping from one sibling to the next swapped two children
/// over and re-formed everything hanging below them. Size is strongly
/// correlated with width, and it does not change because the user moved.
///
/// Every offset a node hands its children — the drop, the ring radii — is
/// proportional to that node's own [`SceneNode::scale`], which is what makes
/// each level smaller than the last and the whole layout self-similar.
fn place(nodes: &mut [SceneNode], kids: &[Vec<usize>]) {
    if nodes.is_empty() {
        return;
    }
    let n = nodes.len();
    // Pass 1, bottom-up: `extent[i]` is the radius of a disc around node `i`
    // holding its whole subtree. Children always come after their parent in
    // `nodes`, so a reverse walk sees every child before its parent.
    let mut extent = vec![0.0f32; n];
    let mut r_in = vec![0.0f32; n];
    let mut r_out = vec![0.0f32; n];
    let mut centre_child = vec![0usize; n];
    for i in (0..n).rev() {
        let k = &kids[i];
        if k.is_empty() {
            extent[i] = nodes[i].target_half;
            continue;
        }
        // Ties go to the earlier child, which is name order, so the choice is
        // decided the same way twice however the crawler got here.
        let big = k
            .iter()
            .copied()
            .reduce(|a, b| if nodes[b].size > nodes[a].size { b } else { a })
            .expect("non-empty");
        centre_child[i] = big;
        let rest = k.len() - 1;
        // The floor is a minimum box at the children's own scale, not at the
        // root's: at four levels down a `BOX_MIN` floor would be wider than the
        // entire subtree it is supposed to be bounding.
        let floor = BOX_MIN * nodes[i].scale * LEVEL_SCALE;
        let widest_other =
            k.iter().copied().filter(|&c| c != big).map(|c| extent[c]).fold(floor, f32::max);
        if rest == 0 {
            extent[i] = extent[big].max(nodes[i].target_half);
            continue;
        }
        // Clear of the centre child, then wide enough that the ring's *area*
        // holds the rest — area, not circumference, so a directory with fifty
        // subdirectories stays compact instead of growing a huge hoop.
        r_in[i] = extent[big].max(nodes[big].target_half) + widest_other * 1.25;
        let per = 3.6 * widest_other * widest_other;
        r_out[i] = (r_in[i] * r_in[i] + rest as f32 * per / std::f32::consts::PI).sqrt();
        extent[i] = (r_out[i] + widest_other).max(nodes[i].target_half);
    }
    // Pass 2, top-down: parents are placed before their children.
    nodes[0].target = v3(0.0, 0.0, 0.0);
    for i in 0..n {
        let p = nodes[i].target;
        let k = kids[i].clone();
        if k.is_empty() {
            continue;
        }
        // A broad level has to drop further, or the tree flattens into a pancake
        // and its links run almost horizontally — at which point the parent and
        // child relationship stops being readable at all.
        let dy = (LEVEL_DY * gap_scale(nodes[i].scale)).max(r_out[i] * 0.7);
        let big = centre_child[i];
        nodes[big].target = v3(p.x, p.y - dy, p.z);
        let rest = k.len() - 1;
        for (j, c) in k.into_iter().filter(|&c| c != big).enumerate() {
            let (dx, dz) = ring_point(j, rest, r_in[i], r_out[i]);
            nodes[c].target = v3(p.x + dx, p.y - dy, p.z + dz);
        }
    }
}

/// Placement of item `j` of `m` on the annulus between `r_in` and `r_out`, by
/// phyllotaxis — the sunflower-seed arrangement. Successive items land at the
/// golden angle and at an area-uniform radius, so they stay evenly spread at any
/// count rather than forming spokes or piling up on the inner edge.
fn ring_point(j: usize, m: usize, r_in: f32, r_out: f32) -> (f32, f32) {
    if m == 0 {
        return (0.0, 0.0);
    }
    // 2π / φ².
    const GOLDEN: f32 = 2.399_963_2;
    let f = (j as f32 + 0.5) / m as f32;
    let r = (r_in * r_in + (r_out * r_out - r_in * r_in) * f).sqrt();
    let a = j as f32 * GOLDEN;
    (a.cos() * r, a.sin() * r)
}

/// Map sizes onto box half-extents, logarithmically and clamped, then shrink
/// each level by its own scale.
///
/// Directory sizes routinely span six orders of magnitude. A linear mapping
/// makes everything except the largest box vanish; an unclamped log one still
/// lets an empty directory collapse to nothing. Clamping to `BOX_MIN..BOX_MAX`
/// keeps every box readable and clickable while preserving the ordering.
///
/// Normalised **within each set of siblings**, not across the scene. The scene
/// now spans several levels of the tree at once, and those levels differ in
/// size by construction — an ancestor contains everything below it — so one
/// scale over the lot would peg its top to a container and squash its own
/// contents onto the floor. Comparing children with children says the thing
/// worth saying ("this is the big one in here"), and has the property the rest
/// of this module is built around: a box's size depends on its own corner of
/// the tree, so it cannot change because something elsewhere came into view.
fn scale_boxes(nodes: &mut [SceneNode], kids: &[Vec<usize>]) {
    // The anchor has no siblings to be measured against, and is the one box
    // guaranteed to contain every other, so it simply takes the ceiling.
    if let Some(root) = nodes.first_mut() {
        root.target_half = BOX_MAX * root.scale;
    }
    for group in kids {
        for (c, unit) in sibling_units(nodes, group, BOX_MIN, BOX_MAX) {
            nodes[c].target_half = unit * nodes[c].scale;
        }
    }
}

/// Map a set of siblings' sizes onto `lo..hi`, logarithmically and clamped.
///
/// Returned rather than applied, so the two styles can put the result on
/// whatever they measure a directory by — a cube's half-extent, or the
/// footprint of a platform.
fn sibling_units(nodes: &[SceneNode], kids: &[usize], lo: f32, hi: f32) -> Vec<(usize, f32)> {
    let log = |i: usize| ((1 + nodes[i].size) as f32).log2();
    let smallest = kids.iter().map(|&c| log(c)).fold(f32::MAX, f32::min);
    let largest = kids.iter().map(|&c| log(c)).fold(f32::MIN, f32::max);
    let span = largest - smallest;
    kids.iter()
        .map(|&c| {
            // A flat range (every directory the same size) sits mid-scale
            // rather than all at the floor.
            let t = if span < 1e-3 { 0.5 } else { (log(c) - smallest) / span };
            (c, lo + (hi - lo) * t.clamp(0.0, 1.0))
        })
        .collect()
}

/// The files sitting **directly** in `path`, largest first, at most `cap`.
///
/// The crawler keeps each directory's largest files by *subtree*, so an entry's
/// `rel` is a path relative to that directory — the ones with no separator in
/// them are the files actually in it, and the rest belong to something below.
/// That list is capped ([`crate::disk::TOP_FILES`]), so a directory holding
/// thousands of small files shows only its largest few; the platform is a
/// portrait of what is big in a directory, not an inventory of it.
fn own_files(tree: &crate::sizes::SizeTree, path: &Path, cap: usize) -> Vec<FileSolid> {
    let Some(node) = tree.get(path) else {
        return Vec::new();
    };
    node.top_files
        .iter()
        .filter(|f| !f.rel.contains(std::path::MAIN_SEPARATOR) && !f.rel.contains('/'))
        .take(cap)
        .map(|f| FileSolid { name: f.rel.clone(), size: f.size })
        .collect()
}

/// Columns the file grid on a platform is laid out in — as square as it can be,
/// so the grid stays roughly square whatever it is holding.
fn grid_cols(n: usize) -> usize {
    if n == 0 { 0 } else { (n as f64).sqrt().ceil() as usize }
}

/// Spacing of a platform's file grid, given its footprint and column count.
///
/// Derived from the platform rather than fixed, so the grid *covers* it — a
/// directory's platform reads as a floor with its contents standing on it, the
/// way fsn's pedestals do, instead of a wide empty slab with a clump of solids
/// marooned in the middle. The `max(cols, 2)` keeps a lone file from being
/// scaled up to fill the whole platform on its own.
fn grid_step(plat_half: f32, cols: usize) -> f32 {
    (plat_half * 2.0 * GRID_FILL / cols.max(2) as f32).min(plat_half * FILE_CELL_MAX)
}

/// Map directory sizes onto platform footprints, level scale included.
///
/// Same logarithmic, clamped, per-sibling mapping as [`scale_boxes`] and for
/// the same reasons.
fn scale_platforms(nodes: &mut [SceneNode], kids: &[Vec<usize>]) {
    if let Some(root) = nodes.first_mut() {
        root.target_plat = PLATFORM_MAX * root.scale;
    }
    for group in kids {
        for (c, unit) in sibling_units(nodes, group, PLATFORM_MIN, PLATFORM_MAX) {
            nodes[c].target_plat = unit * nodes[c].scale;
        }
    }
    // The status line and the Cubes-style fit still read `target_half`, and the
    // platform is what is actually on screen, so keep them in step.
    for n in nodes.iter_mut() {
        n.target_half = n.target_plat;
    }
}

/// Lay the tree out on the ground, fsn-style: rows of platforms marching away
/// from the camera, each parent centred over the span of its children.
///
/// This is a tidy tree (Reingold–Tilford) drawn flat: "one level down" becomes
/// "one row further away", and the room a subtree needs is measured along X.
/// Two passes for the same reason [`place`] needs two — a parent cannot centre
/// itself over its children until it knows how wide they are together.
///
/// Rows do not march away at a fixed spacing: each one is [`LEVEL_SCALE`] times
/// the last, so the ground itself recedes and the camera flying forward into a
/// subdirectory is flying into a smaller, tighter arrangement of the same shape.
fn place_fsn(nodes: &mut [SceneNode], kids: &[Vec<usize>]) {
    if nodes.is_empty() {
        return;
    }
    let n = nodes.len();
    // Pass 1, bottom-up: the width each subtree needs. Children always come
    // after their parent, so a reverse walk sees every child first.
    let mut span = vec![0.0f32; n];
    for i in (0..n).rev() {
        let own = nodes[i].target_plat * 2.0;
        let gap = sibling_gap(nodes[i].scale);
        let kids_w: f32 = kids[i].iter().map(|&c| span[c]).sum::<f32>()
            + gap * kids[i].len().saturating_sub(1) as f32;
        span[i] = own.max(kids_w);
    }
    // Pass 2, top-down: parents are placed before their children.
    nodes[0].target = v3(0.0, 0.0, 0.0);
    for i in 0..n {
        let p = nodes[i].target;
        let k = kids[i].clone();
        if k.is_empty() {
            continue;
        }
        let gap = sibling_gap(nodes[i].scale);
        let total: f32 =
            k.iter().map(|&c| span[c]).sum::<f32>() + gap * k.len().saturating_sub(1) as f32;
        let dz = FSN_ROW_GAP * gap_scale(nodes[i].scale);
        let mut x = p.x - total * 0.5;
        for c in k {
            nodes[c].target = v3(x + span[c] * 0.5, 0.0, p.z + dz);
            x += span[c] + gap;
        }
    }
}

/// Gap between the subtrees of the children of a node at `scale`. Keyed to the
/// children's own scale, so the spacing shrinks with the platforms it separates
/// instead of blowing a deep row apart.
fn sibling_gap(scale: f32) -> f32 {
    FSN_SIBLING_GAP * PLATFORM_MAX * scale * LEVEL_SCALE
}

/// The children of `path` to draw, ordered by name.
///
/// A home directory can hold a hundred subdirectories, and drawing them all
/// turns the view into a wall of boxes with nothing legible on it — so only the
/// largest `cap` are kept. Whatever leads to the directory the view is *about*,
/// the one under the other panel's cursor, and whatever is selected are kept
/// regardless of where they rank: dropping the one box the user is looking for
/// would be the worst possible trade, and dropping a link in the chain down to
/// it would leave the view describing a directory it does not show.
fn children_to_draw(
    tree: &crate::sizes::SizeTree,
    path: &Path,
    cap: usize,
    focus: &Path,
    sel: Option<&Path>,
    cursor: Option<&Path>,
) -> Vec<crate::sizes::DirInfo> {
    // `children_of` hands them back largest-first, which is the order to trim.
    let mut kids = tree.children_of(path);
    if kids.len() > cap {
        let mut keep: Vec<crate::sizes::DirInfo> = Vec::with_capacity(cap + 2);
        for k in kids.drain(..) {
            // `starts_with` rather than equality: this child may be the focus
            // itself or merely the way down to it.
            let must = focus.starts_with(&k.path)
                || Some(k.path.as_path()) == sel
                || Some(k.path.as_path()) == cursor;
            if keep.len() < cap || must {
                keep.push(k);
            }
        }
        kids = keep;
    }
    // Name order for drawing: stable while the crawler is still revising sizes.
    kids.sort_by(|a, b| a.name.cmp(&b.name));
    kids
}

/// A directory's label: its own name, or the whole path for a filesystem root
/// (whose `file_name()` is empty).
fn display_name(p: &Path) -> String {
    match p.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => p.to_string_lossy().into_owned(),
    }
}

/// A stable colour per directory name.
///
/// Keyed by *name*, not by index: the node list is rebuilt as the crawler
/// discovers directories, and an index-keyed hue would recolour the whole tree
/// every time it does.
/// The colours a scene is drawn with, taken from the active theme.
///
/// The fsn style is *theme-tinted*, not a fixed reproduction of fsn's own
/// palette: its geometry is fsn's, but a panel that ignored the loaded theme
/// would look like a different program. The file-type accents are the very ones
/// the listing paints names with, so a red archive in the panel is a red drum
/// on the platform.
#[derive(Debug, Clone, Copy)]
pub struct ScenePalette {
    pub accent: crate::ui::graphics::raster::Rgb,
    /// Near-white, for platform slabs.
    pub platform: crate::ui::graphics::raster::Rgb,
    pub file: crate::ui::graphics::raster::Rgb,
    pub archive: crate::ui::graphics::raster::Rgb,
    pub doc: crate::ui::graphics::raster::Rgb,
    pub image: crate::ui::graphics::raster::Rgb,
    pub media: crate::ui::graphics::raster::Rgb,
    pub exec: crate::ui::graphics::raster::Rgb,
}

impl ScenePalette {
    pub fn from_theme(theme: &crate::ui::theme::Theme) -> ScenePalette {
        use crate::ui::graphics::raster::rgb;
        ScenePalette {
            accent: rgb(theme.panel_border_active),
            // Lifted well toward white: fsn's pedestals are pale, and a
            // platform also has to read as *under* the solids standing on it.
            platform: crate::ui::graphics::raster::over(rgb(theme.panel_fg), (255, 255, 255), 0.72),
            file: rgb(theme.file_fg),
            archive: rgb(theme.archive_fg),
            doc: rgb(theme.doc_fg),
            image: rgb(theme.image_fg),
            media: rgb(theme.media_fg),
            exec: rgb(theme.exec_fg),
        }
    }
}

/// The solid and the colour a file with this extension is drawn as.
///
/// Shape and colour say the same thing twice on purpose. Colour alone is a
/// handful of pixels on a panel-sized raster and nothing at all in the ASCII
/// fallback, where the scene is reduced to brightness — so the silhouette has
/// to carry the meaning on its own.
fn file_look(ext: &str, pal: &ScenePalette) -> (Shape, crate::ui::graphics::raster::Rgb) {
    use crate::util::filetype::{FileCategory, categorize};
    match categorize(ext) {
        Some(FileCategory::Archive) => (Shape::Drum, pal.archive),
        Some(FileCategory::Document) => (Shape::Sheet, pal.doc),
        Some(FileCategory::Image) => (Shape::Frustum, pal.image),
        Some(FileCategory::Media) => (Shape::Wedge, pal.media),
        None if crate::util::filetype::is_executable_ext(ext) => (Shape::Pyramid, pal.exec),
        None => (Shape::Block, pal.file),
    }
}

/// How tall a file's solid stands.
///
/// Logarithmic and clamped, exactly as directory boxes are sized and for the
/// same reason: file sizes in one directory routinely span six orders of
/// magnitude, and a linear mapping would leave everything but the largest as a
/// film on the platform.
fn file_height(size: u64) -> f32 {
    // 1 KiB reads as the floor, 1 GiB as the ceiling.
    const LO: f32 = 10.0;
    const HI: f32 = 30.0;
    let t = ((size.saturating_add(1) as f32).log2() - LO) / (HI - LO);
    FILE_H_MIN + (FILE_H_MAX - FILE_H_MIN) * t.clamp(0.0, 1.0)
}

fn hue_for(name: &str) -> crate::ui::graphics::raster::Rgb {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    crate::ui::graphics::raster::hsv((h.finish() % 360) as f64, 0.45, 0.82)
}

/// Whether `path` can be crawled at all: the crawler walks the real filesystem,
/// so archive, FTP and SFTP panels have no sizes to show.
pub fn is_crawlable(p: &crate::vfs::VfsPath) -> bool {
    p.scheme == "file"
}

/// Whether a panel can be put under the 3D time machine: it has to be looking at
/// a real work tree, since the revisions and their sizes both come from `git`
/// run there. A panel already inside a revision is browsing history, not a
/// repository to scrub.
pub fn is_scrubbable(p: &crate::vfs::VfsPath) -> bool {
    p.is_plain_local()
}

#[cfg(test)]
mod tests;
