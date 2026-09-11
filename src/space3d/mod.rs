//! 3D directory-tree view: a panel view format that draws a directory and its
//! neighbourhood as a tree of boxes connected by lines, each box sized by the
//! directory's total size on disk.
//!
//! Like the Details and Tree formats, it describes the **other** panel: as you
//! navigate over there, the focus moves and the camera flies to it. Sizes come
//! from the shared cache in [`crate::sizes`], so the tree grows outward while
//! the background crawler is still working.
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
const DIST_MIN: f32 = 1.0;
const DIST_MAX: f32 = 14.0;

/// Vertical drop from one tree level to the next.
const LEVEL_DY: f32 = 0.95;

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

/// How many levels below the current directory the tree goes.
const DEPTH_BELOW: u8 = 2;

/// How many children a box at `below` levels under the current directory gets.
///
/// The first level is the subject and gets the most room; deeper levels are
/// there to show shape, and a wide cap on them multiplies out into hundreds of
/// specks. Beyond the cap the smallest are left out; see [`children_to_draw`].
fn child_cap(below: u8) -> usize {
    match below {
        0 => 28,
        _ => 6,
    }
}

/// Half-extent limits for a box.
///
/// Directory sizes span many orders of magnitude, so they are mapped
/// logarithmically and then **clamped between these two**. Without the clamp a
/// single huge directory flattens everything else into invisible specks, and an
/// empty one disappears entirely — the point of the view is comparing shapes,
/// which needs every box to stay on screen and clickable.
const BOX_MIN: f32 = 0.05;
const BOX_MAX: f32 = 0.17;

// -- the fsn style ----------------------------------------------------------
//
// A separate set of constants rather than reused Cubes ones: a platform has to
// be wide enough to stand a grid of file solids on, which is several times what
// a Cubes box is, and the whole scene is spread over a ground plane instead of
// hanging in space.

/// Half-extent limits for a directory platform's footprint, before the room its
/// own file grid needs is taken into account.
const PLATFORM_MIN: f32 = 0.13;
const PLATFORM_MAX: f32 = 0.34;

/// How thick a platform slab is. Thin enough to read as a floor, thick enough
/// to catch the light on its edge rather than vanishing edge-on.
const PLATFORM_H: f32 = 0.042;

/// Gap between neighbouring subtrees, as a fraction of a platform's width.
const FSN_SIBLING_GAP: f32 = 0.55;

/// How far one level sits in front of the next, along the ground.
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

/// Height limits for a file solid, mapped from its size the same way a
/// directory box is — logarithmically, then clamped, so a small file still has
/// a solid worth seeing and a huge one does not become a skyscraper.
const FILE_H_MIN: f32 = 0.030;
const FILE_H_MAX: f32 = 0.145;

/// How many file solids a platform stands: more on the directory the view is
/// about than on its children, which are there to show shape.
///
/// A grid of fifty solids on every one of two hundred platforms would be an
/// unreadable carpet and a great deal to rasterize, so only the focus and its
/// direct children carry files at all.
fn file_cap(below: u8) -> usize {
    match below {
        0 => 16,
        1 => 8,
        _ => 0,
    }
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
    /// Where the layout wants this node; the drawn position chases it.
    pub target: V3,
    pub target_half: f32,
    /// Still being crawled.
    pub partial: bool,
    /// The directory the other panel is on.
    pub is_focus: bool,
    /// The directory the other panel's cursor is on.
    pub is_cursor: bool,
    /// Drawn only to say where this is — the directory above. Small and dimmed,
    /// and never expanded.
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
    /// Directory the laid-out tree hangs from.
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
            shown: HashMap::new(),
            ghosts: Vec::new(),
            aspect: 1.0,
            fit_radius: 1.0,
            fit_pts: Vec::new(),
            zoom: 1.0,
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
    /// The tree is rooted at the **current directory** and grows downward
    /// through its contents. What lies above is context, not the subject: the
    /// directory you are in gets one small, dimmed box above it as a signpost,
    /// and its siblings are not drawn at all. Showing that upper structure in
    /// full — a parent box bigger than everything else, ringed by a hundred
    /// siblings — buries the thing the view exists to show.
    ///
    /// Children are ordered by name, not by size: a name order is stable while
    /// the crawler is still revising sizes, so the tree grows in place instead
    /// of reshuffling under the cursor. Size is carried by the box.
    pub fn sync_from(&mut self, tree: &crate::sizes::SizeTree) {
        self.scanning = !tree.total_of(&self.focus).1;
        self.root = self.focus.clone();

        let mut nodes: Vec<SceneNode> = Vec::new();
        let mut kids: Vec<Vec<usize>> = Vec::new();
        // Levels below the focus; the context parent counts as the focus's own.
        let mut below: Vec<u8> = Vec::new();
        let sel = self.sel_path.clone();

        let push = |nodes: &mut Vec<SceneNode>,
                    kids: &mut Vec<Vec<usize>>,
                    below: &mut Vec<u8>,
                    n: SceneNode,
                    lvl: u8| {
            nodes.push(n);
            kids.push(Vec::new());
            below.push(lvl);
            nodes.len() - 1
        };

        // One box for the directory above, when the crawler knows it — a
        // signpost saying where this is, drawn small and dimmed so it cannot be
        // mistaken for the subject.
        // Only for a parent the crawler has actually enumerated: one it merely
        // knows *of* (interned on the way down to the focus) has no measured
        // size, and a signpost labelled "0 B" is worse than no signpost.
        let up = self
            .focus
            .parent()
            .filter(|p| tree.get(p).is_some_and(|n| n.listed))
            .map(Path::to_path_buf);
        if let Some(p) = &up {
            let (size, complete) = tree.total_of(p);
            push(
                &mut nodes,
                &mut kids,
                &mut below,
                self.node_for(display_name(p), p.clone(), size, complete, true),
                0,
            );
        }

        let (fsize, fcomplete) = tree.total_of(&self.focus);
        let fi = push(
            &mut nodes,
            &mut kids,
            &mut below,
            self.node_for(display_name(&self.focus), self.focus.clone(), fsize, fcomplete, false),
            0,
        );
        if up.is_some() {
            nodes[fi].parent = Some(0);
            kids[0].push(fi);
        }

        let mut i = fi;
        while i < nodes.len() {
            if nodes.len() >= MAX_NODES || below[i] >= DEPTH_BELOW {
                i += 1;
                continue;
            }
            let ppath = nodes[i].path.clone();
            let cap = child_cap(below[i]);
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
                let mut n = self.node_for(k.name, k.path, k.total, k.complete, false);
                n.parent = Some(i);
                let lvl = below[i] + 1;
                let ci = push(&mut nodes, &mut kids, &mut below, n, lvl);
                kids[i].push(ci);
            }
            i += 1;
        }

        scale_boxes(&mut nodes);
        if self.style == Space3dStyle::Fsn {
            // Only the focus and its children carry files; see `file_cap`.
            let mut budget = MAX_FILE_SOLIDS;
            for i in 0..nodes.len() {
                let cap = if nodes[i].context { 0 } else { file_cap(below[i]) };
                let cap = cap.min(budget);
                if cap == 0 {
                    continue;
                }
                nodes[i].files = own_files(tree, &nodes[i].path, cap);
                budget -= nodes[i].files.len();
            }
            scale_platforms(&mut nodes);
            place_fsn(&mut nodes, &kids);
        } else {
            place(&mut nodes, &kids);
        }

        let live: std::collections::HashSet<PathBuf> =
            nodes.iter().map(|n| n.path.clone()).collect();
        // A directory that has left the scene keeps its box for a few frames and
        // fades out where it stood, rather than blinking away.
        let departing = std::mem::replace(&mut self.nodes, nodes);
        for (i, old) in departing.into_iter().enumerate() {
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
            let _ = i;
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

    fn node_for(
        &self,
        name: String,
        path: PathBuf,
        size: u64,
        complete: bool,
        context: bool,
    ) -> SceneNode {
        SceneNode {
            is_focus: path == self.focus,
            is_cursor: self.cursor.as_deref() == Some(path.as_path()),
            name,
            size,
            parent: None,
            target: v3(0.0, 0.0, 0.0),
            target_half: BOX_MIN,
            partial: !complete,
            context,
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

    /// Aim the camera to frame the focus and everything immediately around it.
    ///
    /// Fitted to the actual bounding sphere of that neighbourhood rather than
    /// guessed from the child count, so the tree fills the panel whether the
    /// directory has two subdirectories or two hundred.
    fn aim_camera(&mut self) {
        let Some(fi) = self.nodes.iter().position(|n| n.is_focus) else {
            return;
        };
        // What the view is *about*: the current directory and what is inside
        // it, plus the signpost above so it stays in shot. Grandchildren are
        // left out of the fit — they are detail, and framing all of them would
        // push the camera back until the level that matters was a speck.
        let parent = self.nodes[fi].parent;
        let framed: Vec<&SceneNode> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| *i == fi || Some(*i) == parent || n.parent == Some(fi))
            .map(|(_, n)| n)
            .collect();
        if framed.is_empty() {
            return;
        }
        let fsn = self.style == Space3dStyle::Fsn;
        // What each framed node actually occupies. In the fsn style that is a
        // slab standing on the ground with its files on top, not a cube around
        // its centre.
        let extent = |n: &SceneNode| -> (V3, V3) {
            if fsn {
                let h = n.target_plat;
                let top = PLATFORM_H + if n.files.is_empty() { 0.0 } else { FILE_H_MAX };
                (v3(n.target.x - h, 0.0, n.target.z - h), v3(n.target.x + h, top, n.target.z + h))
            } else {
                let h = n.target_half;
                (
                    v3(n.target.x - h, n.target.y - h, n.target.z - h),
                    v3(n.target.x + h, n.target.y + h, n.target.z + h),
                )
            }
        };
        let boxes: Vec<(V3, V3)> = framed.iter().map(|n| extent(n)).collect();
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
        self.goal.target = if fsn { v3(centre.x, hi.y.max(0.12), centre.z) } else { centre };
        self.fit_radius = hi.sub(lo).scale(0.5).len().max(0.35);
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
                    if lo <= DIST_MIN * 0.5 {
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
        self.goal.dist = (d * self.zoom).clamp(DIST_MIN, DIST_MAX);
        self.settled = false;
    }

    /// The largest fraction of the raster the framed boxes would cover at
    /// distance `d`: 1.0 means they exactly touch an edge, more than that means
    /// they would be cropped.
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
                let (x, y, _) = vec3::project(vec3::to_view(&basis, corner), fw, fh, focal)?;
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
        !self.settled || !self.ghosts.is_empty()
    }

    /// Advance the camera, node positions and box sizes toward their targets.
    ///
    /// Exponential smoothing on a wall-clock delta, so it behaves the same at
    /// 10 fps and at 30 — which matters because this app's frame rate is
    /// event-driven and varies widely.
    pub fn advance(&mut self, now: Instant) {
        let dt = (now - self.last).as_secs_f32().clamp(0.0, 0.1);
        self.last = now;
        if dt <= 0.0 {
            return;
        }
        let t = 1.0 - (-dt / TAU).exp();

        self.cam.target = self.cam.target.lerp(self.goal.target, t);
        self.cam.dist += (self.goal.dist - self.cam.dist) * t;
        self.cam.pitch += (self.goal.pitch - self.cam.pitch) * t;
        // Always orbit the short way round.
        self.cam.yaw += vec3::wrap_angle(self.goal.yaw - self.cam.yaw) * t;

        let mut moving = self.cam.target.sub(self.goal.target).len() > 1e-3
            || (self.cam.dist - self.goal.dist).abs() > 1e-3
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
            let settled_here = cur.pos.sub(n.target).len() <= 1e-3
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
                    let lift = PLATFORM_H * (h / n.target_plat.max(1e-4)).clamp(0.0, 1.0);
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
                        v3(g.pos.x + g.half, PLATFORM_H, g.pos.z + g.half),
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
            let lift = PLATFORM_H * grow;
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
                    max: v3(cx + half, lift + file_height(f.size) * grow, cz + half),
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
                    // surface so the line is not swallowed by it.
                    let y = PLATFORM_H * 0.6;
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

/// Lay the tree out: work out how much room each subtree needs, then hand it
/// that much.
///
/// Two passes, because a parent cannot space its children until it knows how
/// wide each of *their* subtrees is. Without that, a child with fifty
/// grandchildren of its own would spread them straight through its siblings.
///
/// Within a level the **widest** child — normally the focus, since that is the
/// one whose contents are expanded — takes the centre, and its siblings ring
/// around it outside its reach. Spacing every sibling as though it were as wide
/// as that one would scatter a dozen small directories across an enormous disc
/// and push the camera back until nothing was legible.
fn place(nodes: &mut [SceneNode], kids: &[Vec<usize>]) {
    if nodes.is_empty() {
        return;
    }
    let n = nodes.len();
    // Pass 1, bottom-up: `extent[i]` is the radius of a disc around node `i`
    // holding its whole subtree. Children always come after their parent in
    // `nodes`, so a reverse walk sees every child before its parent.
    let mut extent = vec![BOX_MAX; n];
    let mut r_in = vec![0.0f32; n];
    let mut r_out = vec![0.0f32; n];
    let mut centre_child = vec![0usize; n];
    for i in (0..n).rev() {
        let k = &kids[i];
        if k.is_empty() {
            extent[i] = nodes[i].target_half;
            continue;
        }
        // The directory the view is about always takes the centre, directly
        // below its parent, so it can be found at a glance. Only when it is not
        // one of these children does the widest subtree get the middle — which
        // is the arrangement that actually needs the room.
        let big = k.iter().copied().find(|&c| nodes[c].is_focus).unwrap_or_else(|| {
            k.iter().copied().max_by(|&a, &b| extent[a].total_cmp(&extent[b])).expect("non-empty")
        });
        centre_child[i] = big;
        let rest = k.len() - 1;
        let widest_other =
            k.iter().copied().filter(|&c| c != big).map(|c| extent[c]).fold(BOX_MIN, f32::max);
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
        let mut dy = LEVEL_DY.max(r_out[i] * 0.7);
        if nodes[i].context {
            // The signpost hangs close to the directory it points at: a full
            // level's gap would put a long bare line through the middle of the
            // view and push everything that matters down the frame.
            dy *= 0.5;
        }
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

/// Map sizes onto box half-extents, logarithmically and clamped.
///
/// Directory sizes routinely span six orders of magnitude. A linear mapping
/// makes everything except the largest box vanish; an unclamped log one still
/// lets an empty directory collapse to nothing. Clamping to `BOX_MIN..BOX_MAX`
/// keeps every box readable and clickable while preserving the ordering.
fn scale_boxes(nodes: &mut [SceneNode]) {
    // Normalised over the *contents* alone — neither the directory above nor the
    // one we are in takes part.
    //
    // Both are containers of what is being compared, so including either pegs
    // the top of the scale to a total that by definition exceeds everything
    // inside it, and squashes the whole of the contents onto the floor. A
    // directory of six roughly equal subdirectories would render as six boxes at
    // the minimum size, which says nothing at all.
    let subject: Vec<f32> =
        nodes.iter().filter(|n| !n.context && !n.is_focus).map(|n| (1 + n.size) as f32).collect();
    let lo = subject.iter().copied().fold(f32::MAX, f32::min).max(1.0).log2();
    let hi = subject.iter().copied().fold(1.0f32, f32::max).log2();
    let span = (hi - lo).max(1e-3);
    for n in nodes.iter_mut() {
        if n.context {
            // A signpost, not a container: fixed and small whatever it holds.
            n.target_half = BOX_MIN * 0.9;
            continue;
        }
        if n.is_focus {
            // The directory everything else sits inside: drawn at full size,
            // not measured against its own contents.
            n.target_half = BOX_MAX;
            continue;
        }
        // A flat range (every directory the same size) sits mid-scale rather
        // than all at the floor.
        let t = if hi - lo < 1e-3 { 0.5 } else { (((1 + n.size) as f32).log2() - lo) / span };
        n.target_half = BOX_MIN + (BOX_MAX - BOX_MIN) * t.clamp(0.0, 1.0);
    }
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

/// Map directory sizes onto platform footprints, and make sure each platform is
/// at least big enough for the files standing on it.
///
/// Same logarithmic, clamped mapping as [`scale_boxes`] and for the same
/// reason; the difference is that a platform has a second, harder requirement —
/// it must physically hold its grid — so the two are maxed together.
fn scale_platforms(nodes: &mut [SceneNode]) {
    let subject: Vec<f32> =
        nodes.iter().filter(|n| !n.context).map(|n| (1 + n.size) as f32).collect();
    let lo = subject.iter().copied().fold(f32::MAX, f32::min).max(1.0).log2();
    let hi = subject.iter().copied().fold(1.0f32, f32::max).log2();
    let span = (hi - lo).max(1e-3);
    for n in nodes.iter_mut() {
        let by_size = if n.context {
            // A signpost, not a container — fixed and small, as in Cubes.
            PLATFORM_MIN * 0.8
        } else {
            let t = if hi - lo < 1e-3 { 0.5 } else { (((1 + n.size) as f32).log2() - lo) / span };
            PLATFORM_MIN + (PLATFORM_MAX - PLATFORM_MIN) * t.clamp(0.0, 1.0)
        };
        n.target_plat = by_size;
        // The status line and the Cubes-style fit still read `target_half`, and
        // the platform is what is actually on screen, so keep them in step.
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
/// The **context** parent is the exception: it is a signpost, not part of the
/// subject, so it sits one short row *behind* the focus, nearer the camera,
/// rather than in front of it with everything else.
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
        let gap = FSN_SIBLING_GAP * PLATFORM_MAX;
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
        let gap = FSN_SIBLING_GAP * PLATFORM_MAX;
        let total: f32 =
            k.iter().map(|&c| span[c]).sum::<f32>() + gap * k.len().saturating_sub(1) as f32;
        // A short hop for the signpost above; it should read as attached to the
        // focus, not as another level of contents.
        let dz = if nodes[i].context { FSN_ROW_GAP * 0.6 } else { FSN_ROW_GAP };
        let mut x = p.x - total * 0.5;
        for c in k {
            nodes[c].target = v3(x + span[c] * 0.5, 0.0, p.z + dz);
            x += span[c] + gap;
        }
    }
}

/// The children of `path` to draw, ordered by name.
///
/// A home directory can hold a hundred subdirectories, and drawing them all
/// turns the view into a wall of boxes with nothing legible on it — so only the
/// largest `cap` are kept. The directory the view is *about*, the one
/// under the other panel's cursor, and whatever is selected are kept regardless
/// of where they rank: dropping the one box the user is looking for would be the
/// worst possible trade.
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
            let must = k.path == focus
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

#[cfg(test)]
mod tests;
