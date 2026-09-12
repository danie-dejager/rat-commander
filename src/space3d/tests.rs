use super::*;
use crate::sizes::SizeTree;
use std::path::Path;
use std::time::Duration;

/// A cache holding `/r` with the given children, each carrying one file, and
/// optional grandchildren under `/r/<parent>`.
fn cache(kids: &[(&str, u64)], grandkids: &[(&str, &str, u64)]) -> SizeTree {
    let mut t = SizeTree::new();
    let r = t.ensure(Path::new("/r"));
    t.mark_listed(r);
    for (name, size) in kids {
        let p = PathBuf::from("/r").join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
        t.add_file(id, &p.join("f"), *size);
    }
    for (parent, name, size) in grandkids {
        let p = PathBuf::from("/r").join(parent).join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
        t.add_file(id, &p.join("f"), *size);
    }
    t
}

/// A palette for the box-list tests, which care about geometry and flags
/// rather than colour.
fn pal() -> ScenePalette {
    ScenePalette {
        accent: (255, 255, 255),
        platform: (240, 240, 240),
        file: (200, 200, 200),
        archive: (255, 85, 255),
        doc: (170, 85, 0),
        image: (85, 255, 255),
        media: (85, 255, 85),
        exec: (85, 255, 85),
    }
}

fn view_on(focus: &str, t: &SizeTree) -> Space3d {
    let mut sp = Space3d::new(PathBuf::from(focus));
    sp.sync_from(t);
    sp
}

/// A monotonic clock for the animation tests.
///
/// The view's own `last` timestamp runs ahead as frames are pushed through it,
/// so a test that reached for `Instant::now()` after settling would hand it a
/// time in the past, `dt` would be zero, and nothing would move.
struct Clock(Instant);

impl Clock {
    fn new() -> Clock {
        Clock(Instant::now())
    }
    /// Advance by `ms` and step the view once.
    fn step(&mut self, sp: &mut Space3d, ms: u64) {
        self.0 += std::time::Duration::from_millis(ms);
        sp.advance(self.0);
    }
    /// Run until nothing is moving.
    fn settle(&mut self, sp: &mut Space3d) {
        for _ in 0..4000 {
            self.step(sp, 33);
            if !sp.needs_frames() {
                return;
            }
        }
        panic!("the view never settled");
    }
}

/// Settle a view with a throwaway clock, for tests that do not animate further.
fn settle(sp: &mut Space3d) {
    Clock::new().settle(sp);
}

fn node<'a>(sp: &'a Space3d, name: &str) -> &'a SceneNode {
    sp.nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"))
}

// -- tree structure ---------------------------------------------------------

#[test]
fn the_current_directory_is_drawn_in_the_tree_around_it() {
    let t = cache(&[("a", 900), ("b", 500), ("c", 100)], &[("a", "deep", 300)]);
    let sp = view_on("/r/a", &t);
    let names: Vec<&str> = sp.nodes.iter().map(|n| n.name.as_str()).collect();
    // What is *inside* the current directory is the subject…
    assert!(names.contains(&"a"), "the current directory");
    assert!(names.contains(&"deep"), "and what is inside it");
    // …but it is drawn where it belongs, among the things it belongs among:
    // the directory above and the siblings it shares it with. That is the
    // structure the camera moves through, so it has to be there to move through.
    assert!(names.contains(&"r"), "the directory above, which the world hangs from");
    assert!(names.contains(&"b") && names.contains(&"c"), "no siblings in {names:?}");
    assert_eq!(sp.nodes.iter().filter(|n| n.is_focus).count(), 1);
    assert!(node(&sp, "a").is_focus);
}

#[test]
fn the_world_is_anchored_above_the_current_directory_as_far_as_the_crawl_reaches() {
    // Two levels up when they have been walked…
    let mut t = cache(&[("a", 900)], &[("a", "deep", 300)]);
    let below = PathBuf::from("/r/a/deep/further");
    let id = t.ensure(&below);
    t.mark_listed(id);
    t.add_file(id, &below.join("f"), 10);
    let sp = view_on("/r/a/deep", &t);
    assert_eq!(sp.nodes[0].name, "r", "anchored two levels above the current directory");
    assert_eq!(sp.nodes[0].depth, 0);

    // …and no further than the crawler has actually enumerated. "/" was never
    // walked, so there is nothing above "/r" to hang the world from.
    let sp = view_on("/r/a", &t);
    assert_eq!(sp.nodes[0].name, "r", "one level up is all there is");
}

#[test]
fn everything_outside_the_current_directory_is_drawn_as_context() {
    let t = cache(&[("a", 900_000), ("elsewhere", 5_000_000)], &[("a", "deep", 300)]);
    let sp = view_on("/r/a", &t);
    let up = node(&sp, "r");
    assert!(up.context, "the directory above");
    assert!(node(&sp, "elsewhere").context, "and what else is up there with it");
    assert_eq!(up.parent, None, "the anchor sits at the top");
    assert!(!node(&sp, "a").context, "the current directory is the subject");
    assert!(!node(&sp, "deep").context, "and so is what is inside it");
    // Context is drawn *larger*, not smaller: which way is up is exactly what
    // the size of a level says now that the tree no longer re-roots itself.
    assert!(
        up.target_half > node(&sp, "a").target_half,
        "the level above is the bigger one, {} vs {}",
        up.target_half,
        node(&sp, "a").target_half
    );
    // Being larger must not make it read as the subject, so it is faded back.
    assert!(sp.boxes(&pal())[0].dim, "the anchor is drawn faded back");
}

#[test]
fn each_level_down_is_drawn_smaller_than_the_one_above_it() {
    let t = cache(&[("a", 900)], &[("a", "deep", 300)]);
    let sp = view_on("/r/a", &t);
    let (r, a, deep) = (node(&sp, "r"), node(&sp, "a"), node(&sp, "deep"));
    assert_eq!((r.depth, a.depth, deep.depth), (0, 1, 2));
    assert!(r.scale > a.scale && a.scale > deep.scale, "the level scale falls with depth");
    assert!((a.scale - r.scale * LEVEL_SCALE).abs() < 1e-6, "by a fixed factor each time");
    // And the boxes follow it, whatever the directories happen to hold.
    assert!(r.target_half > a.target_half && a.target_half > deep.target_half);
}

#[test]
fn at_a_filesystem_root_there_is_nothing_above_to_point_at() {
    let t = cache(&[("a", 100)], &[]);
    // `/` was never enumerated, so there is no measured parent to sign-post.
    let sp = view_on("/r", &t);
    assert!(!sp.nodes.iter().any(|n| n.context), "no signpost");
    assert!(sp.nodes[0].is_focus, "the current directory is the root of the tree");
}

#[test]
fn every_node_but_the_root_has_a_parent_and_sits_below_it() {
    let t = cache(&[("a", 900), ("b", 500)], &[("a", "deep", 300)]);
    let sp = view_on("/r/a", &t);
    assert!(sp.nodes[0].parent.is_none(), "the root has no parent");
    for (i, n) in sp.nodes.iter().enumerate().skip(1) {
        let p = n.parent.expect("a non-root node has a parent");
        assert!(p < i, "parents come before their children");
        assert!(
            n.target.y < sp.nodes[p].target.y,
            "{} hangs below its parent {}",
            n.name,
            sp.nodes[p].name
        );
    }
    // The grandchild really is a child of the focus, not of the root.
    assert_eq!(sp.nodes[node_index(&sp, "deep")].parent, Some(node_index(&sp, "a")));
}

fn node_index(sp: &Space3d, name: &str) -> usize {
    sp.nodes.iter().position(|n| n.name == name).unwrap()
}

#[test]
fn children_are_ordered_by_name_so_the_tree_does_not_reshuffle_as_sizes_land() {
    // Name order is the whole point: the crawler revises sizes constantly, and a
    // size-ordered tree would rearrange itself under the cursor while it does.
    let small = cache(&[("aaa", 1), ("bbb", 1), ("ccc", 1)], &[]);
    let sp1 = view_on("/r", &small);
    let order1: Vec<String> = sp1.nodes.iter().map(|n| n.name.clone()).collect();
    // Now the sizes are wildly different — the order must not change.
    let big = cache(&[("aaa", 1), ("bbb", 9_000_000), ("ccc", 400)], &[]);
    let sp2 = view_on("/r", &big);
    let order2: Vec<String> = sp2.nodes.iter().map(|n| n.name.clone()).collect();
    assert_eq!(order1, order2, "positions are stable while sizes move");
}

#[test]
fn siblings_are_spread_out_and_do_not_sit_on_top_of_each_other() {
    let t = cache(&[("a", 1), ("b", 1), ("c", 1), ("d", 1), ("e", 1)], &[]);
    let sp = view_on("/r", &t);
    let kids: Vec<&SceneNode> = sp.nodes.iter().filter(|n| n.parent.is_some()).collect();
    assert_eq!(kids.len(), 5);
    for (i, a) in kids.iter().enumerate() {
        for b in kids.iter().skip(i + 1) {
            let d = a.target.sub(b.target).len();
            assert!(d > a.target_half + b.target_half, "{} and {} overlap", a.name, b.name);
        }
    }
}

#[test]
fn a_lone_child_sits_directly_below_its_parent() {
    let t = cache(&[("only", 100)], &[]);
    let sp = view_on("/r", &t);
    let c = node(&sp, "only");
    assert!(c.target.x.abs() < 1e-5 && c.target.z.abs() < 1e-5, "straight down, not off to a side");
}

#[test]
fn the_focus_children_are_expanded_too_so_it_reads_as_a_tree() {
    // Three levels below the root is what makes this a tree rather than a hub
    // with spokes.
    let t = cache(&[("a", 900), ("b", 500)], &[("a", "inner", 300)]);
    let sp = view_on("/r/a", &t);
    let inner = node(&sp, "inner");
    let mid = inner.parent.expect("inner has a parent");
    assert_eq!(sp.nodes[mid].name, "a");
    assert!(sp.nodes[mid].parent.is_some(), "and that parent hangs off the root");
    // Three distinct levels.
    let ys: std::collections::BTreeSet<i64> =
        sp.nodes.iter().map(|n| (n.target.y * 1000.0) as i64).collect();
    assert_eq!(ys.len(), 3, "root, its children, and their children");
}

#[test]
fn a_crowded_subtree_does_not_spread_through_its_siblings() {
    // The point of sizing each subtree before placing it: a directory with many
    // children of its own needs more room than its childless siblings.
    let mut kids: Vec<(&str, u64)> = vec![("busy", 100), ("quiet1", 100), ("quiet2", 100)];
    kids.sort();
    let grand: Vec<(String, String, u64)> =
        (0..12).map(|i| ("busy".to_string(), format!("g{i:02}"), 10u64)).collect();
    let grand_refs: Vec<(&str, &str, u64)> =
        grand.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), *c)).collect();
    let t = cache(&kids, &grand_refs);
    let sp = view_on("/r", &t);

    // Every node in "busy"'s subtree must stay clear of its aunts.
    let busy = node_index(&sp, "busy");
    let subtree: Vec<usize> =
        (0..sp.nodes.len()).filter(|&i| i == busy || sp.nodes[i].parent == Some(busy)).collect();
    let aunts: Vec<usize> = (0..sp.nodes.len())
        .filter(|&i| sp.nodes[i].parent == sp.nodes[busy].parent && i != busy)
        .collect();
    assert!(aunts.len() >= 2 && subtree.len() > 5, "the fixture is actually crowded");
    for &i in &subtree {
        for &j in &aunts {
            let (a, b) = (&sp.nodes[i], &sp.nodes[j]);
            let flat = (a.target.x - b.target.x).hypot(a.target.z - b.target.z);
            assert!(flat > a.target_half + b.target_half, "{} overlaps {}", a.name, b.name);
        }
    }
}

#[test]
fn a_huge_directory_shows_its_biggest_children_not_all_of_them() {
    // A hundred boxes is a wall with nothing legible on it, so only the largest
    // are drawn.
    let kids: Vec<(String, u64)> = (0..400).map(|i| (format!("d{i:03}"), i as u64 + 1)).collect();
    let refs: Vec<(&str, u64)> = kids.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let t = cache(&refs, &[]);
    let sp = view_on("/r", &t);
    assert!(sp.nodes.len() <= SPINE_CAP + 2, "got {} nodes", sp.nodes.len());
    assert!(sp.nodes.len() > 5, "but it still shows a useful number of them");
    // The ones kept are the big ones: d399 is the largest, d000 the smallest.
    assert!(sp.nodes.iter().any(|n| n.name == "d399"), "the largest is kept");
    assert!(!sp.nodes.iter().any(|n| n.name == "d000"), "the smallest is dropped");
}

#[test]
fn the_focused_directory_survives_the_cap_however_small_it_is() {
    // Dropping the one box the user is actually looking for would be the worst
    // possible trade.
    let mut kids: Vec<(String, u64)> =
        (0..400).map(|i| (format!("d{i:03}"), i as u64 + 100)).collect();
    kids.push(("speck".into(), 1));
    let refs: Vec<(&str, u64)> = kids.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let t = cache(&refs, &[]);
    let sp = view_on("/r/speck", &t);
    let speck = node(&sp, "speck");
    assert!(speck.is_focus, "the focus is drawn even though it is the smallest");
}

#[test]
fn the_camera_goes_to_the_current_directory_rather_than_the_tree_coming_to_it() {
    // The focus used to be dragged to the middle of its level so it could be
    // found at a glance, which is what made stepping between siblings swap two
    // subtrees over. Now it stays where the tree put it and the camera does the
    // travelling.
    let t = cache(&[("aaa", 900), ("bbb", 800), ("mid", 5), ("zzz", 700)], &[]);
    let mut sp = view_on("/r/mid", &t);
    settle(&mut sp);
    let f = node(&sp, "mid");
    assert!(f.is_focus);
    assert!(
        f.target.x.hypot(f.target.z) > 1e-3,
        "a small directory is left out on the ring, at {:?}",
        f.target
    );
    let off = sp.cam.target.sub(f.target).len();
    assert!(off < f.target_half, "the camera is looking straight at it, {off} away");
}

#[test]
fn which_child_takes_the_middle_does_not_depend_on_where_the_user_is() {
    // The middle goes to the largest directory — a fact about the tree. Whoever
    // is standing where must not change it, or every step sideways would swap
    // two subtrees over.
    let t = cache(&[("aaa", 900), ("bbb", 800), ("mid", 5), ("zzz", 700)], &[]);
    let middle_when_on = |focus: &str| {
        let sp = view_on(focus, &t);
        sp.nodes
            .iter()
            .find(|n| n.parent == Some(0) && n.target.x.hypot(n.target.z) < 1e-5)
            .map(|n| n.name.clone())
    };
    assert_eq!(middle_when_on("/r"), Some("aaa".into()), "the biggest child");
    for focus in ["/r/mid", "/r/zzz", "/r/bbb"] {
        assert_eq!(middle_when_on(focus), Some("aaa".into()), "still, standing on {focus}");
    }
}

// -- box sizing -------------------------------------------------------------

#[test]
fn box_sizes_are_clamped_between_a_minimum_and_a_maximum() {
    // Six orders of magnitude: without clamping the small ones vanish. The
    // clamp is on the size a level's own scale is then applied to, so divide
    // that back out to see it.
    let t = cache(&[("tiny", 1), ("mid", 50_000), ("huge", 40_000_000_000)], &[]);
    let sp = view_on("/r", &t);
    for n in &sp.nodes {
        let unit = n.target_half / n.scale;
        assert!(
            (BOX_MIN..=BOX_MAX).contains(&unit),
            "{} has half-extent {unit} of its level, outside {BOX_MIN}..{BOX_MAX}",
            n.name
        );
    }
    assert!(node(&sp, "huge").target_half > node(&sp, "mid").target_half);
    assert!(node(&sp, "mid").target_half > node(&sp, "tiny").target_half);
}

#[test]
fn a_directory_is_measured_against_its_siblings_not_against_the_whole_scene() {
    // One level of the tree contains the next by construction, so a single
    // scale over the lot would peg its top to a container and flatten its
    // contents. Sizing within a set of siblings also means a box cannot change
    // because something in another branch came into view.
    let t = cache(&[("a", 1_000_000), ("b", 10)], &[("b", "x", 9), ("b", "y", 1)]);
    let sp = view_on("/r/b", &t);
    // "x" dwarfs "y" inside "b", even though both are specks next to "a".
    let (x, y) = (node(&sp, "x"), node(&sp, "y"));
    assert_eq!(x.depth, y.depth, "same level, so the scale is the only difference");
    assert!(x.target_half > y.target_half * 1.2, "{} vs {}", x.target_half, y.target_half);
}

#[test]
fn an_empty_directory_still_gets_a_box_worth_clicking() {
    let t = cache(&[("full", 5_000_000), ("empty", 0)], &[]);
    let sp = view_on("/r", &t);
    let empty = node(&sp, "empty");
    assert!(empty.target_half >= BOX_MIN * empty.scale);
}

#[test]
fn directories_of_equal_size_get_equal_boxes_mid_scale() {
    // The root carries the same total as its children combined, so scope the
    // "flat range" claim to a tree whose every node really is the same size.
    let mut t = SizeTree::new();
    for name in ["a", "b"] {
        let p = PathBuf::from("/r").join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
    }
    let r = t.id_of(Path::new("/r")).unwrap();
    t.mark_listed(r);
    let sp = view_on("/r", &t);
    let (a, b) = (node(&sp, "a").target_half, node(&sp, "b").target_half);
    assert!((a - b).abs() < 1e-6, "same size, same box");
    assert!(a > BOX_MIN && a < BOX_MAX, "a flat range sits mid-scale, not on the floor");
}

#[test]
fn the_share_is_measured_against_the_directory_above() {
    // A directory with no subdirectories of its own would otherwise always read
    // 0 %, which is exactly the case the cursor starts on.
    let t = cache(&[("big", 750), ("leaf", 250)], &[]);
    let mut sp = view_on("/r/leaf", &t);
    let leaf = node(&sp, "leaf");
    assert!(leaf.is_focus);
    let share = sp.selected_share().expect("a share against the root");
    assert!((share - 25.0).abs() < 1.0, "250 of 1000 is a quarter, got {share}");
    // The tree's own root has nothing to be a share of.
    sp.selected = 0;
    assert!(sp.selected_share().is_none());
}

// -- connectors -------------------------------------------------------------

#[test]
fn every_child_is_linked_to_its_parent() {
    let t = cache(&[("a", 900), ("b", 500)], &[("a", "deep", 300)]);
    let mut sp = view_on("/r/a", &t);
    settle(&mut sp);
    let links = sp.links();
    assert_eq!(links.len(), sp.nodes.len() - 1, "one link per non-root node");
    for (from, to) in &links {
        assert!(from.y > to.y, "links run downward, parent to child");
        assert!(from.sub(*to).len() > 0.0, "and have length");
    }
}

#[test]
fn a_link_starts_and_ends_on_the_boxes_it_joins() {
    let t = cache(&[("only", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    let ki = node_index(&sp, "only");
    let pi = sp.nodes[ki].parent.expect("a parent");
    // `links()` skips the root, so link k belongs to node k+1.
    let (from, to) = sp.links()[ki - 1];
    let root = &sp.nodes[pi];
    let kid = &sp.nodes[ki];
    // Anchored on the facing faces, so the line emerges from the boxes rather
    // than starting inside them.
    assert!((from.y - (root.target.y - root.target_half)).abs() < 1e-3);
    assert!((to.y - (kid.target.y + kid.target_half)).abs() < 1e-3);
}

// -- following the other panel ----------------------------------------------

#[test]
fn moving_the_other_panel_moves_the_focus_and_flies_the_camera() {
    let t = cache(&[("a", 900), ("b", 500)], &[("a", "deep", 300)]);
    let mut sp = view_on("/r/a", &t);
    let mut clock = Clock::new();
    clock.settle(&mut sp);
    let before = sp.cam.target;

    // The other panel navigated into /r/b.
    sp.set_focus(Path::new("/r/b"));
    sp.sync_from(&t);
    assert!(node(&sp, "b").is_focus, "the focus followed");
    assert!(sp.needs_frames(), "and the camera has somewhere to go");

    // It flies there rather than cutting: one frame is only part of the way.
    clock.step(&mut sp, 33);
    let mid = sp.cam.target;
    assert!(mid.sub(before).len() > 1e-4, "the camera started moving");
    clock.settle(&mut sp);
    assert!(sp.cam.target.sub(mid).len() > 1e-4, "and kept going past the first frame");
}

#[test]
fn re_focusing_the_same_directory_changes_nothing() {
    let t = cache(&[("a", 900)], &[]);
    let mut sp = view_on("/r/a", &t);
    settle(&mut sp);
    sp.set_focus(Path::new("/r/a"));
    assert!(!sp.needs_frames(), "no work for a focus that did not move");
}

#[test]
fn stepping_between_siblings_moves_the_camera_and_leaves_them_where_they_are() {
    // The headline of the fixed layout: a directory keeps its place in the
    // world whoever is standing in it, so walking about is the camera's job.
    // (A level can still breathe as the branch under it opens and closes; what
    // it may never do is put anything somewhere else. Here "a" reserves room
    // for its own box either way, so the positions are identical outright.)
    let t = cache(&[("a", 900), ("b", 500), ("c", 100)], &[("a", "inner", 300)]);
    let mut sp = view_on("/r/a", &t);
    settle(&mut sp);
    let where_it_was = |sp: &Space3d, name: &str| (node(sp, name).target, node(sp, name).scale);
    let before: Vec<((V3, f32), &str)> =
        ["r", "a", "b", "c"].iter().map(|n| (where_it_was(&sp, n), *n)).collect();
    let cam_before = sp.cam.target;

    sp.set_focus(Path::new("/r/b"));
    sp.sync_from(&t);
    settle(&mut sp);

    for ((pos, scale), name) in &before {
        let (now, now_scale) = where_it_was(&sp, name);
        assert!(now.sub(*pos).len() < 1e-5, "{name} moved from {pos:?} to {now:?}");
        assert!((now_scale - scale).abs() < 1e-6, "{name} changed level");
    }
    assert!(
        sp.cam.target.sub(cam_before).len() > node(&sp, "b").target_half,
        "the camera is what moved"
    );
}

#[test]
fn the_anchor_only_ever_moves_by_a_transform_nothing_can_see() {
    // Walking down far enough has to re-hang the world lower, or a long descent
    // would run the scene into the floating-point floor. That is a change of
    // coordinates and nothing else: the camera and every box move with it, so
    // the picture is identical on both sides of the step.
    let mut t = cache(&[("a", 900), ("b", 100)], &[("a", "inner", 300)]);
    for name in ["one", "two"] {
        let p = PathBuf::from("/r/a/inner").join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
        t.add_file(id, &p.join("f"), 50);
    }
    let mut sp = view_on("/r/a/inner", &t);
    settle(&mut sp);
    assert_eq!(sp.nodes[0].name, "r", "two levels above, as far as the crawl reaches");

    // Where "one" is on screen, before: as a direction from the eye, which is
    // the one thing a change of coordinates must not touch.
    let bearing = |sp: &Space3d, name: &str| {
        let (pos, _) = sp.drawn(node_index(sp, name));
        pos.sub(sp.cam.eye()).norm()
    };
    let before = bearing(&sp, "one");
    let framing = sp.drawn(node_index(&sp, "one")).1 / sp.cam.dist;

    sp.set_focus(Path::new("/r/a/inner/one"));
    sp.sync_from(&t);
    // Deliberately *not* settled: this is about the frame drawn immediately
    // after the re-anchor, before anything has had a chance to animate.
    assert_eq!(sp.nodes[0].name, "a", "the world re-hung one level lower");
    assert!(bearing(&sp, "one").sub(before).len() < 1e-4, "it is in the same place on screen");
    let after = sp.drawn(node_index(&sp, "one")).1 / sp.cam.dist;
    assert!((after - framing).abs() < 1e-4, "and drawn at the same size, {framing} vs {after}");
}

#[test]
fn the_camera_closes_in_as_the_tree_goes_deeper() {
    // Each level is smaller than the last, so the same framing at a deeper one
    // means a nearer camera. Nothing decides that separately — it falls out of
    // the fit.
    let mut t = cache(&[("a", 900)], &[("a", "inner", 300)]);
    for name in ["one", "two"] {
        let p = PathBuf::from("/r/a/inner").join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
        t.add_file(id, &p.join("f"), 50);
    }
    let dist_on = |focus: &str| {
        let mut sp = view_on(focus, &t);
        sp.set_viewport(400, 300);
        settle(&mut sp);
        sp.cam.dist
    };
    let shallow = dist_on("/r");
    let deeper = dist_on("/r/a");
    let deepest = dist_on("/r/a/inner");
    assert!(deeper < shallow, "one level in: {deeper} vs {shallow}");
    assert!(deepest < deeper, "two levels in: {deepest} vs {deeper}");
}

#[test]
fn the_cursor_highlight_marks_the_directory_the_other_panel_points_at() {
    let t = cache(&[("aaa", 900), ("bbb", 500), ("ccc", 100)], &[]);
    let mut sp = view_on("/r", &t);
    assert!(!sp.nodes.iter().any(|n| n.is_cursor), "nothing marked until told");

    sp.set_cursor(Some(Path::new("/r/bbb")));
    sp.sync_from(&t);
    let marked: Vec<&str> =
        sp.nodes.iter().filter(|n| n.is_cursor).map(|n| n.name.as_str()).collect();
    assert_eq!(marked, ["bbb"], "exactly the directory under the cursor");
    assert!(sp.boxes(&pal()).iter().any(|b| b.cursor), "and it is drawn lit");

    // Moving the cursor moves the highlight, and only the highlight.
    let cam = sp.cam;
    let goal_before = sp.goal;
    sp.set_cursor(Some(Path::new("/r/ccc")));
    sp.sync_from(&t);
    assert!(node(&sp, "ccc").is_cursor && !node(&sp, "bbb").is_cursor);
    assert_eq!(sp.cam, cam, "the camera does not move");
    assert_eq!(sp.goal, goal_before, "not even its destination — only the highlight");
}

#[test]
fn the_highlight_clears_when_the_cursor_leaves_a_directory() {
    // On a file, or on `..`, there is no box to point at.
    let t = cache(&[("aaa", 900)], &[]);
    let mut sp = view_on("/r", &t);
    sp.set_cursor(Some(Path::new("/r/aaa")));
    sp.sync_from(&t);
    assert!(node(&sp, "aaa").is_cursor);
    sp.set_cursor(None);
    sp.sync_from(&t);
    assert!(!sp.nodes.iter().any(|n| n.is_cursor), "the highlight is gone");
}

#[test]
fn setting_the_same_cursor_again_is_free() {
    let t = cache(&[("aaa", 900)], &[]);
    let mut sp = view_on("/r", &t);
    sp.set_cursor(Some(Path::new("/r/aaa")));
    sp.sync_from(&t);
    let synced = sp.synced_at;
    // Called every loop iteration, so an unchanged cursor must not force a
    // re-layout on every frame.
    sp.set_cursor(Some(Path::new("/r/aaa")));
    assert_eq!(sp.synced_at, synced, "no work for a cursor that did not move");
}

#[test]
fn the_highlighted_directory_survives_the_child_cap() {
    // The one box the user is about to step into must not be the one dropped.
    let kids: Vec<(String, u64)> = (0..400).map(|i| (format!("d{i:03}"), i as u64 + 100)).collect();
    let mut kids = kids;
    kids.push(("speck".into(), 1));
    let refs: Vec<(&str, u64)> = kids.iter().map(|(n, s)| (n.as_str(), *s)).collect();
    let t = cache(&refs, &[]);
    let mut sp = view_on("/r", &t);
    sp.set_cursor(Some(Path::new("/r/speck")));
    sp.sync_from(&t);
    assert!(node(&sp, "speck").is_cursor, "kept despite being the smallest");
}

// -- growing while the scan runs --------------------------------------------

#[test]
fn the_tree_grows_as_the_crawler_finds_directories() {
    // First pass: the crawler has only seen one subdirectory.
    let partial = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &partial);
    settle(&mut sp);
    assert_eq!(sp.nodes.len(), 2, "root plus the one child known so far");

    // It then finds two more. They must appear without disturbing the rest.
    let fuller = cache(&[("a", 100), ("b", 200), ("c", 300)], &[]);
    sp.sync_from(&fuller);
    assert_eq!(sp.nodes.len(), 4, "the new directories joined the tree");
    assert!(sp.needs_frames(), "and they animate into place");
}

#[test]
fn a_newly_found_directory_grows_out_of_its_parent() {
    // Starting a new node at its parent's position is what makes a scan read as
    // a tree growing rather than boxes blinking into existence.
    let partial = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &partial);
    let mut clock = Clock::new();
    clock.settle(&mut sp);

    let fuller = cache(&[("a", 100), ("b", 200)], &[]);
    sp.sync_from(&fuller);
    clock.step(&mut sp, 16);

    let i = node_index(&sp, "b");
    let (pos, half) = sp.drawn(i);
    let target = sp.nodes[i].target;
    let root = sp.nodes[0].target;
    assert!(
        pos.sub(root).len() < pos.sub(target).len(),
        "one frame in, the new box is still near its parent"
    );
    assert!(half < sp.nodes[i].target_half, "and is still growing to size");
    clock.settle(&mut sp);
    assert!(sp.drawn(i).0.sub(target).len() < 1e-3, "then it arrives");
}

#[test]
fn a_directory_that_disappears_is_forgotten() {
    let full = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &full);
    settle(&mut sp);
    let gone = cache(&[("a", 100)], &[]);
    sp.sync_from(&gone);
    assert_eq!(sp.nodes.len(), 2);
    assert!(!sp.shown.contains_key(Path::new("/r/b")), "its animation state went too");
}

// -- fading in and out ------------------------------------------------------

#[test]
fn a_new_box_is_invisible_on_the_frame_it_appears() {
    // Seeded at sync time rather than on the first animation step: a frame can
    // be drawn before the animation is ever stepped, and that frame would
    // otherwise show the box at full size and full strength.
    let partial = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &partial);
    settle(&mut sp);

    let fuller = cache(&[("a", 100), ("b", 200)], &[]);
    sp.sync_from(&fuller);
    let b = sp.boxes(&pal());
    let newcomer = b.iter().find(|x| x.name == "b").expect("the new box");
    assert_eq!(newcomer.fade, 0.0, "drawn fully transparent before it animates");
    assert!(newcomer.max.y - newcomer.min.y < 1e-6, "and at no size");
}

#[test]
fn a_new_box_fades_in_rather_than_appearing() {
    let partial = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &partial);
    let mut clock = Clock::new();
    clock.settle(&mut sp);

    let fuller = cache(&[("a", 100), ("b", 200)], &[]);
    sp.sync_from(&fuller);
    clock.step(&mut sp, 16);
    let mid = sp.shown[Path::new("/r/b")].fade;
    assert!(mid > 0.0 && mid < 1.0, "part way in after one frame, got {mid}");
    clock.settle(&mut sp);
    assert_eq!(sp.shown[Path::new("/r/b")].fade, 1.0, "then fully present");
}

#[test]
fn a_departing_box_fades_out_where_it_stood() {
    let full = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &full);
    let mut clock = Clock::new();
    clock.settle(&mut sp);
    let was_at = sp.drawn(node_index(&sp, "b")).0;

    let gone = cache(&[("a", 100)], &[]);
    sp.sync_from(&gone);
    assert!(!sp.nodes.iter().any(|n| n.name == "b"), "out of the tree at once");
    assert_eq!(sp.ghosts.len(), 1, "but still on screen, fading");
    let g = sp.boxes(&pal()).into_iter().find(|x| x.name == "b").expect("ghost box");
    assert!(g.fade > 0.5, "starting from where it was");
    let centre = crate::space3d::vec3::v3(
        (g.min.x + g.max.x) * 0.5,
        (g.min.y + g.max.y) * 0.5,
        (g.min.z + g.max.z) * 0.5,
    );
    assert!(centre.sub(was_at).len() < 1e-3, "and in the place it occupied");

    clock.step(&mut sp, 33);
    assert!(sp.ghosts[0].fade < g.fade, "it dissolves");
    clock.settle(&mut sp);
    assert!(sp.ghosts.is_empty(), "and is dropped once invisible");
}

#[test]
fn a_fading_box_cannot_be_selected_or_clicked() {
    let full = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &full);
    settle(&mut sp);
    let gone = cache(&[("a", 100)], &[]);
    sp.sync_from(&gone);
    assert_eq!(sp.ghosts.len(), 1);

    // Hand-place bounds for every drawn box, the ghost included.
    let boxes = sp.boxes(&pal());
    sp.bounds = (0..boxes.len())
        .map(|i| Some((i as f32 * 20.0, 0.0, i as f32 * 20.0 + 10.0, 10.0)))
        .collect();
    let ghost_i = boxes.len() - 1;
    let x = ghost_i as f32 * 20.0 + 5.0;
    assert!(!sp.pick(x, 5.0), "clicking a fading box selects nothing");
    assert!(sp.selected < sp.nodes.len(), "and the cursor stays on a real node");
}

#[test]
fn a_directory_that_comes_straight_back_does_not_ghost_itself() {
    let full = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &full);
    settle(&mut sp);
    let gone = cache(&[("a", 100)], &[]);
    sp.sync_from(&gone);
    assert_eq!(sp.ghosts.len(), 1);
    // Back again before it finished fading: it must be drawn once, as itself.
    sp.sync_from(&full);
    assert!(sp.ghosts.is_empty(), "its ghost was cancelled");
    let drawn = sp.boxes(&pal());
    assert_eq!(
        drawn.iter().filter(|x| x.name == "b").count(),
        1,
        "and it is drawn once, as itself, not alongside its own ghost"
    );
}

#[test]
fn ghosts_keep_the_animation_running_until_they_are_gone() {
    let full = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &full);
    settle(&mut sp);
    let gone = cache(&[("a", 100)], &[]);
    sp.sync_from(&gone);
    let mut clock = Clock::new();
    // The camera may settle long before the fade finishes; the fade must keep
    // asking for frames on its own or it would freeze half-visible.
    for _ in 0..200 {
        clock.step(&mut sp, 33);
        if sp.ghosts.is_empty() {
            break;
        }
        assert!(sp.needs_frames(), "a fading box keeps the frames coming");
    }
    assert!(sp.ghosts.is_empty());
}

// -- repaint throttling -----------------------------------------------------

#[test]
fn the_image_is_not_rebuilt_faster_than_the_frame_rate() {
    // The other panel's cursor moving is a change of picture, and on a graphics
    // terminal a change of picture is a whole image re-shipped to the terminal.
    // Held-down arrow keys deliver those faster than any terminal can swallow
    // them, so the rebuilds have to be rationed.
    let t = cache(&[("a", 100), ("b", 200)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);

    let t0 = Instant::now();
    assert!(sp.claim_repaint(t0), "the first paint is never held back");
    assert!(!sp.claim_repaint(t0 + Duration::from_millis(5)), "a paint 5 ms later is");
    assert!(!sp.claim_repaint(t0 + Duration::from_millis(32)), "and so is one at 32 ms");
    assert!(sp.claim_repaint(t0 + Duration::from_millis(33)), "a paint a frame later is not");
    assert!(!sp.claim_repaint(t0 + Duration::from_millis(40)), "which starts the gap again");
}

#[test]
fn a_held_back_paint_asks_for_the_frame_that_delivers_it() {
    // Refusing a paint on a settled scene would leave the picture stale for
    // good unless the refusal itself brings a frame back round.
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    assert!(!sp.needs_frames(), "an idle 3D panel costs no CPU");

    let now = Instant::now();
    assert!(sp.claim_repaint(now));
    assert!(!sp.needs_frames(), "a paint that went through owes nothing");

    assert!(!sp.claim_repaint(now + Duration::from_millis(5)));
    assert!(sp.needs_frames(), "one that was held back is owed a frame");

    assert!(sp.claim_repaint(now + Duration::from_millis(33)));
    assert!(!sp.needs_frames(), "and stops asking once it has had it");
}

#[test]
fn an_uncollected_paint_does_not_pin_the_frame_ticker_on() {
    // A panel that is not being drawn — behind a dialog, hidden, too small —
    // never comes back to collect. The debt has to lapse, or an idle app would
    // sit at 30 fps for ever.
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    let now = Instant::now();
    assert!(sp.claim_repaint(now));
    assert!(!sp.claim_repaint(now));
    assert!(sp.needs_frames());
    // Nothing collected it; four frame intervals on, it is written off. (The
    // clock here is the real one, so the wait has to actually elapse.)
    std::thread::sleep(Duration::from_millis(150));
    assert!(!sp.needs_frames(), "the uncollected paint lapsed");
}

#[test]
fn drawing_the_cell_art_settles_a_paint_the_image_path_was_refused() {
    // Switching to the cell fallback (a dialog opened, graphics went off) still
    // repaints the scene, so it must clear the debt rather than leave the view
    // asking for frames to deliver a paint that has already happened.
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    let now = Instant::now();
    assert!(sp.claim_repaint(now));
    assert!(!sp.claim_repaint(now));
    assert!(sp.needs_frames());
    sp.mark_painted(Instant::now());
    assert!(!sp.needs_frames(), "the cell art paid the debt off");
}

// -- camera -----------------------------------------------------------------

#[test]
fn the_camera_settles_and_then_stops_asking_for_frames() {
    // An idle 3D panel must cost no CPU.
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    assert!(!sp.needs_frames());
    sp.orbit(0.8, 0.2);
    assert!(sp.needs_frames(), "an orbit starts it again");
    settle(&mut sp);
    assert!(!sp.needs_frames());
}

#[test]
fn smoothing_is_frame_rate_independent() {
    // The same elapsed time must reach the same pose whether it arrived as a few
    // slow frames or many fast ones.
    let t = cache(&[("a", 100)], &[]);
    let run = |step_ms: u64, steps: u32| {
        let mut sp = view_on("/r", &t);
        let mut clock = Clock::new();
        clock.settle(&mut sp);
        sp.orbit(1.0, 0.0);
        for _ in 0..steps {
            clock.step(&mut sp, step_ms);
        }
        sp.cam.yaw
    };
    let slow = run(100, 6); // 600 ms in 6 frames
    let fast = run(20, 30); // 600 ms in 30 frames
    assert!((slow - fast).abs() < 0.02, "10 fps {slow} vs 50 fps {fast}");
}

#[test]
fn orbiting_takes_the_short_way_round() {
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    let start = sp.cam.yaw;
    sp.goal.yaw = start + std::f32::consts::TAU + 0.1;
    settle(&mut sp);
    assert!(
        vec3::wrap_angle(sp.cam.yaw - start).abs() < 0.2,
        "a 360°+ request is a small move, not a full spin"
    );
}

/// The scene has to fit whatever shape the panel is, and fill it — not crop on
/// a narrow one nor sit stranded in the middle of a wide one.
#[test]
fn the_view_refits_to_the_shape_of_the_panel() {
    use crate::space3d::raster3d;
    let t = cache(&[("a", 900), ("b", 500), ("c", 300), ("d", 100)], &[]);
    for (w, h) in [(400u32, 300u32), (160, 400), (900, 200), (120, 120)] {
        let mut sp = view_on("/r", &t);
        sp.set_viewport(w, h);
        settle(&mut sp);
        let boxes = sp.boxes(&pal());
        let bounds = raster3d::project_bounds(w, h, &boxes, sp.cam.eye(), sp.cam.target);

        // Everything the camera is framing — the current directory and what is
        // inside it — must land on the raster.
        let mut widest: f32 = 0.0;
        let mut tallest: f32 = 0.0;
        for (i, b) in bounds.iter().enumerate().take(sp.nodes.len()) {
            let Some((x0, y0, x1, y1)) = *b else { continue };
            assert!(
                x0 > -1.0 && y0 > -1.0 && x1 < w as f32 + 1.0 && y1 < h as f32 + 1.0,
                "{} is cropped at {w}x{h}: {:?}",
                sp.nodes[i].name,
                b
            );
            widest = widest.max(x1 - x0);
            tallest = tallest.max(y1 - y0);
        }
        // …and it must actually use the space, rather than shrinking to a dot in
        // the middle. Measured across the whole framed set, not one box.
        let (mut sx0, mut sy0) = (f32::MAX, f32::MAX);
        let (mut sx1, mut sy1) = (f32::MIN, f32::MIN);
        for b in bounds.iter().take(sp.nodes.len()).flatten() {
            sx0 = sx0.min(b.0);
            sy0 = sy0.min(b.1);
            sx1 = sx1.max(b.2);
            sy1 = sy1.max(b.3);
        }
        let used = ((sx1 - sx0) / w as f32).max((sy1 - sy0) / h as f32);
        assert!(used > 0.6, "the scene only spanned {:.0}% of {w}x{h}", used * 100.0);
        let _ = (widest, tallest);
    }
}

#[test]
fn a_narrower_panel_pulls_the_camera_back() {
    // The vertical field of view is fixed, so a narrow panel has a narrow
    // horizontal one — and fitting only the vertical would crop the sides.
    let t = cache(&[("a", 900), ("b", 500)], &[]);
    let dist_at = |w: u32, h: u32| {
        let mut sp = view_on("/r", &t);
        sp.set_viewport(w, h);
        settle(&mut sp);
        sp.cam.dist
    };
    let wide = dist_at(600, 300);
    let narrow = dist_at(150, 300);
    assert!(narrow > wide, "narrow {narrow} should be further back than wide {wide}");
}

#[test]
fn zooming_survives_a_resize_and_a_change_of_directory() {
    // Zoom is held as a factor on the fit, not as a distance, so the view does
    // not snap back to the default framing the moment anything else changes.
    let t = cache(&[("a", 900), ("b", 500)], &[("a", "inner", 100)]);
    let mut sp = view_on("/r", &t);
    sp.set_viewport(400, 300);
    settle(&mut sp);
    let default_dist = sp.cam.dist;

    sp.zoom(0.6);
    settle(&mut sp);
    assert!(sp.cam.dist < default_dist, "zoomed in");
    let zoomed = sp.zoom;

    sp.set_viewport(200, 400);
    settle(&mut sp);
    assert_eq!(sp.zoom, zoomed, "a resize re-fits without discarding the zoom");

    sp.set_focus(Path::new("/r/a"));
    sp.sync_from(&t);
    settle(&mut sp);
    assert_eq!(sp.zoom, zoomed, "nor does moving to another directory");

    // Home puts it back.
    sp.reset_view();
    settle(&mut sp);
    assert_eq!(sp.zoom, 1.0, "Home restores the default framing");
}

#[test]
fn a_degenerate_viewport_is_ignored_rather_than_dividing_by_zero() {
    let t = cache(&[("a", 900)], &[]);
    let mut sp = view_on("/r", &t);
    sp.set_viewport(400, 300);
    settle(&mut sp);
    let before = sp.cam.dist;
    sp.set_viewport(0, 0);
    sp.set_viewport(10, 0);
    assert_eq!(sp.cam.dist, before, "a zero-sized panel changes nothing");
    assert!(sp.cam.dist.is_finite());
}

#[test]
fn zoom_and_pitch_stay_within_sane_limits() {
    let t = cache(&[("a", 100)], &[]);
    let mut sp = view_on("/r", &t);
    for _ in 0..100 {
        sp.zoom(0.5);
        sp.orbit(0.0, -1.0);
    }
    assert!(sp.goal.dist >= DIST_MIN && sp.goal.pitch >= PITCH_MIN);
    for _ in 0..100 {
        sp.zoom(2.0);
        sp.orbit(0.0, 1.0);
    }
    assert!(sp.goal.dist <= DIST_MAX && sp.goal.pitch <= PITCH_MAX);
}

// -- navigation -------------------------------------------------------------

#[test]
fn the_selection_follows_the_directory_not_the_index() {
    let t = cache(&[("a", 10), ("b", 10), ("c", 10)], &[]);
    let mut sp = view_on("/r", &t);
    sp.selected = node_index(&sp, "c");
    sp.remember_selection();
    // A directory sorting before "c" turns up mid-scan, shifting the indices.
    let t2 = cache(&[("a", 10), ("b", 10), ("bb", 10), ("c", 10)], &[]);
    sp.sync_from(&t2);
    assert_eq!(sp.selected_node().map(|n| n.name.as_str()), Some("c"), "cursor stayed on 'c'");
}

#[test]
fn arrows_step_to_the_neighbour_in_that_direction() {
    let t = cache(&[("a", 10), ("b", 10), ("c", 10)], &[]);
    let mut sp = view_on("/r", &t);
    // Hand-place the projected bounds: a | b | c, left to right.
    sp.bounds = vec![None; sp.nodes.len()];
    for (k, name) in ["a", "b", "c"].iter().enumerate() {
        let (x, i) = (k as f32 * 20.0, node_index(&sp, name));
        sp.bounds[i] = Some((x, 0.0, x + 10.0, 10.0));
    }
    sp.selected = node_index(&sp, "a");
    sp.remember_selection();
    sp.step(1.0, 0.0);
    assert_eq!(sp.selected_node().unwrap().name, "b");
    sp.step(1.0, 0.0);
    assert_eq!(sp.selected_node().unwrap().name, "c");
    sp.step(1.0, 0.0);
    assert_eq!(sp.selected_node().unwrap().name, "c", "and stops at the edge");
    sp.step(-1.0, 0.0);
    assert_eq!(sp.selected_node().unwrap().name, "b");
}

#[test]
fn clicking_picks_the_box_under_the_pointer() {
    let t = cache(&[("a", 10), ("b", 10)], &[]);
    let mut sp = view_on("/r", &t);
    sp.bounds = vec![None; sp.nodes.len()];
    let (ia, ib) = (node_index(&sp, "a"), node_index(&sp, "b"));
    sp.bounds[ia] = Some((0.0, 0.0, 10.0, 10.0));
    sp.bounds[ib] = Some((20.0, 0.0, 30.0, 10.0));
    assert!(sp.pick(25.0, 5.0));
    assert_eq!(sp.selected_node().unwrap().name, "b");
    assert!(!sp.pick(100.0, 100.0), "a click on empty space selects nothing");
    assert_eq!(sp.selected_node().unwrap().name, "b", "and leaves the selection alone");
}

#[test]
fn two_levels_of_contents_are_shown_without_asking() {
    // You can see what is inside each subdirectory before deciding to go there,
    // without having to select it first.
    let t = cache(&[("a", 900), ("b", 500)], &[("b", "inner", 200)]);
    let sp = view_on("/r", &t);
    let inner = node(&sp, "inner");
    assert_eq!(sp.nodes[inner.parent.expect("a parent")].name, "b");
    // …but not a third level, which is detail the view cannot render legibly.
    let deepest =
        sp.nodes.iter().filter(|n| !n.context).map(|n| depth_of(&sp, n)).max().unwrap_or(0);
    assert_eq!(deepest, DEPTH_BELOW, "contents go exactly {DEPTH_BELOW} levels down");
}

/// How many links separate a node from the current directory.
fn depth_of(sp: &Space3d, n: &SceneNode) -> u8 {
    let mut d = 0u8;
    let mut cur = n;
    while let Some(p) = cur.parent {
        if cur.is_focus {
            break;
        }
        d += 1;
        cur = &sp.nodes[p];
    }
    d
}

// -- edge cases -------------------------------------------------------------

#[test]
fn a_directory_the_crawler_has_not_noticed_yet_is_still_the_subject() {
    // Its parent was listed before it existed, so the way down to it from the
    // anchor is broken. Losing the context around it is fine; losing the
    // directory the whole view is about is not.
    let t = cache(&[("a", 100)], &[]);
    // "/r" has been walked, so it is a perfectly good anchor — it just has
    // never heard of the directory the other panel is standing in.
    assert!(t.get(Path::new("/r")).is_some_and(|n| n.listed));
    assert!(t.get(Path::new("/r/brand-new")).is_none());

    let sp = view_on("/r/brand-new", &t);
    let f = sp.nodes.iter().find(|n| n.is_focus).expect("the current directory is drawn");
    assert_eq!(f.name, "brand-new");
    assert_eq!(f.depth, 0, "the world hangs from it rather than from anything above");
}

#[test]
fn an_unknown_directory_is_harmless() {
    let t = SizeTree::new();
    let mut sp = view_on("/nowhere", &t);
    assert_eq!(sp.nodes.len(), 1, "just the root placeholder");
    assert!(sp.links().is_empty());
    sp.step(1.0, 0.0); // must not panic
    sp.reset_view();
    settle(&mut sp);
}

#[test]
fn a_filesystem_root_is_labelled_by_its_path() {
    // "/" has no file name; falling back to the path keeps it from being blank.
    assert_eq!(display_name(Path::new("/")), "/");
    assert_eq!(display_name(Path::new("/usr/lib")), "lib");
}

#[test]
fn colour_is_keyed_by_name_so_a_rebuild_does_not_recolour_the_tree() {
    assert_eq!(hue_for("downloads"), hue_for("downloads"));
    assert_ne!(hue_for("downloads"), hue_for("documents"));
}

#[test]
fn only_local_paths_are_crawlable() {
    use crate::vfs::VfsPath;
    assert!(is_crawlable(&VfsPath::local(PathBuf::from("/tmp"))));
    let mut remote = VfsPath::local(PathBuf::from("/tmp"));
    remote.scheme = "sftp".into();
    assert!(!is_crawlable(&remote), "the crawler walks the real filesystem only");
}

// -- the fsn style ----------------------------------------------------------

/// A cache holding `/r` with `kids` subdirectories, each carrying one file, and
/// `files` sitting directly in `/r` itself.
fn cache_with_files(kids: &[(&str, u64)], files: &[(&str, u64)]) -> SizeTree {
    let mut t = SizeTree::new();
    let r = t.ensure(Path::new("/r"));
    t.mark_listed(r);
    for (name, size) in files {
        t.add_file(r, &PathBuf::from("/r").join(name), *size);
    }
    for (name, size) in kids {
        let p = PathBuf::from("/r").join(name);
        let id = t.ensure(&p);
        t.mark_listed(id);
        t.add_file(id, &p.join("f"), *size);
    }
    t
}

fn fsn_on(focus: &str, t: &SizeTree) -> Space3d {
    let mut sp = Space3d::new(PathBuf::from(focus));
    sp.set_style(Space3dStyle::Fsn);
    sp.sync_from(t);
    sp
}

#[test]
fn the_classic_look_is_what_you_get_unless_you_ask_for_the_other_one() {
    let t = cache(&[("a", 10)], &[]);
    assert_eq!(view_on("/r", &t).style, Space3dStyle::Cubes);
}

#[test]
fn every_fsn_platform_stands_on_the_ground_rather_than_hanging_in_space() {
    // The whole point of the style: one plane, and everything on it. The Cubes
    // layout drops each level below the last, which is what this must not do.
    let t = cache(&[("a", 10), ("b", 20)], &[("a", "a1", 5)]);
    let sp = fsn_on("/r", &t);
    for n in &sp.nodes {
        assert_eq!(n.target.y, 0.0, "{} floats at y={}", n.name, n.target.y);
    }
    let boxes = sp.boxes(&pal());
    for b in &boxes {
        assert!(b.min.y >= 0.0, "nothing may sink below the ground plane");
    }
}

#[test]
fn contents_recede_from_the_camera_one_row_per_level() {
    let t = cache(&[("a", 10)], &[("a", "a1", 5)]);
    let sp = fsn_on("/r", &t);
    let z = |name: &str| node(&sp, name).target.z;
    assert!(z("a") > z("r"), "a child sits further away than its parent");
    assert!(z("a1") > z("a"), "and a grandchild further still");
}

#[test]
fn the_signpost_above_sits_behind_the_focus_not_in_front_of_it() {
    // It says where you are; it must not stand between the camera and the
    // directory the view is actually about.
    let t = cache(&[("a", 10)], &[]);
    let mut sp = fsn_on("/r/a", &t);
    sp.sync_from(&t);
    let up = node(&sp, "r");
    let focus = node(&sp, "a");
    assert!(up.context, "the parent is drawn as context");
    assert!(up.target.z < focus.target.z, "and nearer the camera than the focus");
}

#[test]
fn sibling_subtrees_are_laid_side_by_side_and_never_overlap() {
    let t = cache(&[("a", 10), ("b", 20), ("c", 30)], &[("a", "a1", 5), ("b", "b1", 5)]);
    let sp = fsn_on("/r", &t);
    let mut spans: Vec<(f32, f32, &str)> = sp
        .nodes
        .iter()
        .filter(|n| n.parent == Some(node_index(&sp, "r")))
        .map(|n| (n.target.x - n.target_plat, n.target.x + n.target_plat, n.name.as_str()))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    for w in spans.windows(2) {
        assert!(w[0].1 <= w[1].0, "{} overlaps {}", w[0].2, w[1].2);
    }
}

#[test]
fn a_platform_carries_the_files_that_sit_directly_in_its_directory() {
    let t = cache_with_files(&[("sub", 10)], &[("one.txt", 4096), ("two.zip", 8192)]);
    let sp = fsn_on("/r", &t);
    let names: Vec<&str> = node(&sp, "r").files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"one.txt") && names.contains(&"two.zip"), "got {names:?}");
    // The crawler's list is of the largest files in the whole *subtree*, so the
    // ones belonging to a subdirectory have to be filtered back out — otherwise
    // a directory would stand its children's contents on its own platform.
    assert!(!names.iter().any(|n| n.contains('/')), "only its own files: {names:?}");
}

#[test]
fn the_classic_look_draws_no_files_at_all() {
    let t = cache_with_files(&[("sub", 10)], &[("one.txt", 4096)]);
    let sp = view_on("/r", &t);
    assert!(sp.nodes.iter().all(|n| n.files.is_empty()), "Cubes draws directories alone");
}

#[test]
fn file_solids_are_scenery_and_cannot_be_selected_or_clicked() {
    // `bounds` is indexed in step with `boxes`, and navigation only ever looks
    // at the first `nodes.len()` of it. If the solids were not strictly at the
    // tail, clicking one would select an unrelated directory.
    let t = cache_with_files(&[("sub", 10)], &[("a.txt", 4096), ("b.zip", 8192)]);
    let mut sp = fsn_on("/r", &t);
    settle(&mut sp);
    let boxes = sp.boxes(&pal());
    assert!(boxes.len() > sp.nodes.len(), "there are solids beyond the directories");
    for b in &boxes[sp.nodes.len()..] {
        assert!(!b.selected && !b.cursor && !b.focus, "a solid is never the selection");
    }
    // Every directory box comes first, in node order.
    for (i, n) in sp.nodes.iter().enumerate() {
        assert_eq!(boxes[i].name, n.name, "box {i} must still be node {i}");
    }
}

#[test]
fn only_the_focus_and_its_children_stand_files_on_their_platforms() {
    // Two hundred platforms each carrying a grid would be an unreadable carpet
    // and a great deal to rasterize every frame.
    let t = cache(&[("a", 10)], &[("a", "a1", 5)]);
    let sp = fsn_on("/r", &t);
    assert!(node(&sp, "a1").files.is_empty(), "a grandchild carries none");
}

#[test]
fn a_file_solid_is_shaped_and_coloured_by_what_kind_of_file_it_is() {
    use crate::space3d::raster3d::Shape;
    let p = pal();
    assert_eq!(file_look("zip", &p), (Shape::Drum, p.archive));
    assert_eq!(file_look("pdf", &p), (Shape::Sheet, p.doc));
    assert_eq!(file_look("png", &p), (Shape::Frustum, p.image));
    assert_eq!(file_look("mp3", &p), (Shape::Wedge, p.media));
    assert_eq!(file_look("exe", &p), (Shape::Pyramid, p.exec));
    // Anything the theme has no accent for is a plain block in the plain colour.
    assert_eq!(file_look("rs", &p), (Shape::Block, p.file));
    assert_eq!(file_look("", &p), (Shape::Block, p.file));
}

#[test]
fn a_file_solids_height_follows_its_size_but_stays_within_bounds() {
    assert!(file_height(0) >= FILE_H_MIN, "an empty file still has a solid to see");
    assert!(file_height(u64::MAX) <= FILE_H_MAX, "and a huge one is not a skyscraper");
    assert!(file_height(10_000_000) > file_height(1_000), "bigger files stand taller");
}

#[test]
fn the_file_grid_covers_its_platform_whatever_it_is_holding() {
    // A grid that did not scale with the platform would leave a big directory
    // showing a handful of specks marooned on a wide slab.
    for n in [1usize, 4, 9, 16] {
        let cols = grid_cols(n);
        let step = grid_step(PLATFORM_MAX, cols);
        let spanned = cols as f32 * step;
        assert!(spanned <= PLATFORM_MAX * 2.0, "{n} files overflow the platform");
        if n > 1 {
            assert!(spanned > PLATFORM_MAX * 0.5, "{n} files leave the platform mostly bare");
        }
    }
}

#[test]
fn links_run_across_the_ground_between_the_platforms_they_join() {
    let t = cache(&[("a", 10)], &[]);
    let mut sp = fsn_on("/r", &t);
    // Settled: while a newly-found box is still growing out of its parent, its
    // link is legitimately collapsed to a point at the parent's own position.
    settle(&mut sp);
    let links = sp.links();
    assert!(!links.is_empty(), "the child is joined to its parent");
    for (p, c) in &links {
        assert!(p.y > 0.0 && p.y <= PLATFORM_H, "a link skims the ground, not the sky");
        assert!((p.y - c.y).abs() < 1e-6, "and stays level along its length");
        assert!(c.z > p.z, "running away from the camera, parent to child");
    }
}

#[test]
fn switching_style_re_lays_the_scene_out_and_re_aims_the_camera() {
    let t = cache(&[("a", 10), ("b", 20)], &[]);
    let mut sp = view_on("/r", &t);
    settle(&mut sp);
    let (cubes_pitch, _) = (sp.goal_angles().1, 0);
    assert!(sp.nodes.iter().any(|n| n.target.y != 0.0), "the classic tree hangs below its root");

    sp.set_style(Space3dStyle::Fsn);
    sp.sync_from(&t);
    settle(&mut sp);
    assert!(sp.nodes.iter().all(|n| n.target.y == 0.0), "the fsn scene stands on the ground");
    assert!(
        sp.goal_angles().1 < cubes_pitch,
        "and the camera drops to look across the ground rather than down on it"
    );
}

#[test]
fn setting_the_style_it_already_has_changes_nothing() {
    let t = cache(&[("a", 10)], &[]);
    let mut sp = fsn_on("/r", &t);
    settle(&mut sp);
    let before = (sp.cam, sp.nodes.len());
    sp.set_style(Space3dStyle::Fsn);
    assert_eq!((sp.cam, sp.nodes.len()), before, "a no-op switch must not re-aim anything");
    assert!(sp.settled, "nor restart the animation");
}

#[test]
fn the_camera_stays_above_the_ground_at_every_angle_it_allows() {
    // The ground is painted as a backdrop rather than rasterized, which is only
    // correct while the eye is above it. `PITCH_MIN` is what guarantees that.
    let t = cache(&[("a", 10)], &[]);
    let mut sp = fsn_on("/r", &t);
    for _ in 0..40 {
        sp.orbit(0.3, -0.3);
    }
    settle(&mut sp);
    assert!(sp.cam.pitch >= PITCH_MIN, "pitch is clamped above the horizontal");
    assert!(sp.cam.eye().y > sp.cam.target.y - 1e-3, "so the eye never drops under the plane");
}

// -- scrubbing through history ----------------------------------------------
//
// The timeline swaps in a whole new `SizeTree` per revision rather than editing
// the last one, because going back in time makes directories *smaller* and the
// crawler's tree only ever grows. These tests are what say the swap reads as a
// morph rather than a jump cut — the property the whole feature rests on.

/// A revision's tree, built the way `sizes::from_paths` builds one.
fn revision(entries: &[(&str, u64)], epoch: u64) -> SizeTree {
    crate::sizes::from_paths(Path::new("/r"), entries.iter().copied(), epoch)
}

#[test]
fn a_directory_present_in_both_revisions_keeps_its_animation_state() {
    let before = revision(&[("a/f", 100), ("b/f", 200)], 1);
    let mut sp = view_on("/r", &before);
    settle(&mut sp);
    let kept = sp.drawn(node_index(&sp, "a")).0;

    // `a` grows, `b` is untouched.
    let after = revision(&[("a/f", 100_000), ("b/f", 200)], 2);
    sp.sync_from(&after);

    // It did not blink out and back: the box is still where it was, and now
    // animating toward a new size rather than snapping to it.
    let i = node_index(&sp, "a");
    assert!(sp.shown.contains_key(Path::new("/r/a")), "its animation state survived the swap");
    assert!(sp.drawn(i).0.sub(kept).len() < 1e-3, "it starts the frame where it stood");
    assert!(sp.drawn(i).1 < sp.nodes[i].target_half, "and grows into its new size");
    settle(&mut sp);
    assert!((sp.drawn(i).1 - sp.nodes[i].target_half).abs() < 1e-3, "then it arrives");
}

#[test]
fn a_directory_added_by_a_later_revision_grows_out_of_its_parent() {
    let before = revision(&[("a/f", 100)], 1);
    let mut sp = view_on("/r", &before);
    settle(&mut sp);

    let after = revision(&[("a/f", 100), ("new/f", 50)], 2);
    sp.sync_from(&after);

    let b = sp.boxes(&pal());
    let newcomer = b.iter().find(|x| x.name == "new").expect("the added directory");
    assert_eq!(newcomer.fade, 0.0, "it is invisible on the frame it appears");
    let i = node_index(&sp, "new");
    assert_eq!(sp.drawn(i).1, 0.0, "and starts at no size at all");
}

/// Scrubbing *backwards* is the case the crawler could never produce: a
/// directory that has not been created yet must leave the scene by fading, not
/// by vanishing between frames.
#[test]
fn a_directory_not_yet_created_fades_out_when_scrubbing_backwards() {
    let now = revision(&[("a/f", 100), ("later/f", 50)], 1);
    let mut sp = view_on("/r", &now);
    settle(&mut sp);
    assert!(sp.boxes(&pal()).iter().any(|x| x.name == "later"));

    // Step back to before `later` existed.
    let past = revision(&[("a/f", 100)], 2);
    sp.sync_from(&past);

    assert!(!sp.nodes.iter().any(|n| n.path.ends_with("later")), "it is gone from the scene");
    let ghost = sp.boxes(&pal()).iter().any(|x| x.name == "later");
    assert!(ghost, "but is still drawn, fading out where it stood");
    settle(&mut sp);
    assert!(!sp.boxes(&pal()).iter().any(|x| x.name == "later"), "and then it is gone");
}

/// The anchor is what `rebase` keys off. A revision tree marks nothing above the
/// repository root as listed, so the anchor pins there and stays put across every
/// scrub — which is what makes the transition a pure morph with no re-framing.
#[test]
fn the_anchor_does_not_move_between_revisions() {
    let a = revision(&[("x/f", 1)], 1);
    let mut sp = view_on("/r", &a);
    settle(&mut sp);
    let anchor = sp.root.clone();

    for (n, entries) in [(2u64, &[("x/f", 9_999)][..]), (3, &[("y/f", 5)][..]), (4, &[][..])] {
        sp.sync_from(&revision(entries, n));
        assert_eq!(sp.root, anchor, "the scene stayed in the same frame of reference");
    }
}

/// Revisions differing wildly in size must not make a box vanish and reappear —
/// the scene has to stay legible while a drag runs through many commits.
#[test]
fn scrubbing_rapidly_keeps_the_scene_bounded() {
    let mut sp = view_on("/r", &revision(&[("a/f", 1)], 1));
    settle(&mut sp);
    for n in 2..40u64 {
        let entries: Vec<(&str, u64)> =
            if n % 2 == 0 { vec![("a/f", 10), ("b/f", 20)] } else { vec![("a/f", 10)] };
        sp.sync_from(&crate::sizes::from_paths(Path::new("/r"), entries.into_iter(), n));
    }
    assert!(sp.ghosts.len() <= MAX_GHOSTS, "rapid scrubbing cannot pile ghosts up without limit");
    assert!(sp.boxes(&pal()).iter().any(|x| x.name == "a"), "the scene is still drawn");
}

/// A revision's listing is complete by construction, so the view must never
/// claim it is still scanning.
#[test]
fn a_revision_never_reads_as_still_scanning() {
    let sp = view_on("/r", &revision(&[("a/f", 1), ("deep/deeper/f", 2)], 1));
    assert!(!sp.scanning, "a revision arrives whole; there is nothing left to wait for");
}
