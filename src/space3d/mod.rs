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

pub mod raster3d;
pub mod render;
pub mod vec3;

use raster3d::SceneBox;
use std::collections::HashMap;
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
    /// The boxes being framed, as `(centre, half-extent)`. Kept so the fit can
    /// be solved against their real projected size rather than a bounding
    /// sphere, which on a flat wide scene leaves a tall panel half empty.
    fit_pts: Vec<(V3, f32)>,
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
}

impl Space3d {
    pub fn new(focus: PathBuf) -> Space3d {
        let goal = CamPose { target: v3(0.0, -LEVEL_DY, 0.0), dist: 3.6, yaw: -2.0, pitch: 0.62 };
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
        }
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
            for k in children_to_draw(tree, &ppath, cap, &self.focus, sel.as_deref(), self.cursor.as_deref())
            {
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
        place(&mut nodes, &kids);

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
                Some(p) => self
                    .shown
                    .get(&self.nodes[p].path)
                    .map_or(self.nodes[p].target, |s| s.pos),
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
        let mut centre = v3(0.0, 0.0, 0.0);
        for n in &framed {
            centre = centre.add(n.target);
        }
        centre = centre.scale(1.0 / framed.len() as f32);
        let radius = framed
            .iter()
            .map(|n| n.target.sub(centre).len() + n.target_half)
            .fold(0.35f32, f32::max);
        self.goal.target = centre;
        self.fit_radius = radius;
        self.fit_pts = framed.iter().map(|n| (n.target, n.target_half)).collect();
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
    fn refit(&mut self) {
        let mut d = fit_dist(self.fit_radius, self.aspect);
        // Projected size goes as 1/distance, so scaling by the overshoot
        // converges in a couple of rounds.
        for _ in 0..4 {
            let Some(fill) = self.projected_fill(d) else { break };
            // Spelt out rather than as a negated comparison, so the NaN case
            // it is guarding against is obvious.
            if !fill.is_finite() || fill <= 1e-4 {
                break;
            }
            let next = d * fill / FILL_TARGET;
            if !next.is_finite() || next <= 0.0 {
                break;
            }
            if (next - d).abs() < d * 1e-3 {
                d = next;
                break;
            }
            d = next;
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
        for (c, h) in &self.fit_pts {
            for corner in [
                v3(c.x - h, c.y - h, c.z - h),
                v3(c.x + h, c.y - h, c.z - h),
                v3(c.x - h, c.y - h, c.z + h),
                v3(c.x + h, c.y - h, c.z + h),
                v3(c.x - h, c.y + h, c.z - h),
                v3(c.x + h, c.y + h, c.z - h),
                v3(c.x - h, c.y + h, c.z + h),
                v3(c.x + h, c.y + h, c.z + h),
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
            let cur = self
                .shown
                .entry(n.path.clone())
                .or_insert(Shown { pos: n.target, half: 0.0, fade: 0.0 });
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
    pub fn boxes(&self, accent: crate::ui::graphics::raster::Rgb) -> Vec<SceneBox> {
        self.nodes
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
                    crate::ui::graphics::raster::over(hue_for(&n.name), accent, 0.8)
                } else {
                    hue_for(&n.name)
                };
                SceneBox {
                    name: n.name.clone(),
                    size_label: crate::util::bytes::human_size(n.size),
                    min: v3(p.x - h, p.y - h, p.z - h),
                    max: v3(p.x + h, p.y + h, p.z + h),
                    color: base,
                    selected: sel,
                    focus: n.is_focus,
                    cursor: n.is_cursor,
                    partial: n.partial,
                    dim: n.context,
                    fade: self.shown.get(&n.path).map_or(1.0, |s| s.fade),
                }
            })
            // Fading-out boxes ride along at the end of the list, after every
            // real node — `bounds` is indexed in step with this, and navigation
            // only ever looks at the first `nodes.len()` of them.
            .chain(self.ghosts.iter().map(|g| SceneBox {
                name: g.node.name.clone(),
                size_label: crate::util::bytes::human_size(g.node.size),
                min: v3(g.pos.x - g.half, g.pos.y - g.half, g.pos.z - g.half),
                max: v3(g.pos.x + g.half, g.pos.y + g.half, g.pos.z + g.half),
                color: hue_for(&g.node.name),
                selected: false,
                focus: false,
                cursor: false,
                partial: g.node.partial,
                dim: g.node.context,
                fade: g.fade,
            }))
            .collect()
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
        self.goal.yaw = -2.0;
        self.goal.pitch = 0.62;
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
        let big = k
            .iter()
            .copied()
            .find(|&c| nodes[c].is_focus)
            .unwrap_or_else(|| {
                k.iter()
                    .copied()
                    .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
                    .expect("non-empty")
            });
        centre_child[i] = big;
        let rest = k.len() - 1;
        let widest_other = k
            .iter()
            .copied()
            .filter(|&c| c != big)
            .map(|c| extent[c])
            .fold(BOX_MIN, f32::max);
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
    let subject: Vec<f32> = nodes
        .iter()
        .filter(|n| !n.context && !n.is_focus)
        .map(|n| (1 + n.size) as f32)
        .collect();
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
        let t = if hi - lo < 1e-3 {
            0.5
        } else {
            (((1 + n.size) as f32).log2() - lo) / span
        };
        n.target_half = BOX_MIN + (BOX_MAX - BOX_MIN) * t.clamp(0.0, 1.0);
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
