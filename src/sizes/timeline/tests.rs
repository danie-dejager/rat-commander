//! Tests for the history scrubber's bookkeeping.
//!
//! What matters here is that dragging through a repository's history stays
//! bounded: a cached revision is instant, a held key does not spawn a process
//! per commit, and a reply for a revision already scrubbed past never becomes
//! what the scene shows.

use super::*;
use std::path::Path;

fn rev(short: &str, time: i64) -> Rev {
    Rev { oid: format!("oid-{short}"), short: short.into(), time, subject: format!("s{short}") }
}

/// Five revisions, newest first, as `git log` reports them.
fn timeline() -> Timeline {
    let revs = (0..5).map(|i| rev(&format!("r{i}"), 1_000 - i)).collect();
    Timeline::new(0, PathBuf::from("/repo"), revs)
}

fn past_debounce(t: &Timeline) -> Instant {
    t.pending_since.unwrap_or_else(Instant::now) + DEBOUNCE + Duration::from_millis(1)
}

fn want(f: Fetch) -> String {
    match f {
        Fetch::Want { oid, .. } => oid,
        Fetch::Idle => panic!("expected a fetch"),
    }
}

fn deliver(t: &mut Timeline, oid: &str) {
    t.deliver(oid.to_string(), vec![("a.txt".to_string(), 10)]);
}

#[test]
fn it_opens_on_the_newest_revision() {
    let t = timeline();
    assert_eq!(t.current().unwrap().short, "r0");
    assert_eq!(t.position(), (5, 5), "newest reads as the last of five");
}

#[test]
fn stepping_back_walks_down_the_history() {
    let mut t = timeline();
    t.step(-1);
    assert_eq!(t.current().unwrap().short, "r1", "negative is back in time");
    t.step(-2);
    assert_eq!(t.current().unwrap().short, "r3");
    t.step(1);
    assert_eq!(t.current().unwrap().short, "r2", "positive is forward again");
    assert_eq!(t.position(), (3, 5));
}

#[test]
fn stepping_stops_at_both_ends_rather_than_wrapping() {
    let mut t = timeline();
    t.step(-100);
    assert_eq!(t.current().unwrap().short, "r4", "the oldest commit");
    t.step(100);
    assert_eq!(t.current().unwrap().short, "r0", "the newest");
}

/// The debounce is what keeps a held key from spawning a process per commit.
#[test]
fn a_scrub_is_not_fetched_until_it_settles() {
    let mut t = timeline();
    let start = Instant::now();
    assert_eq!(t.poll(start), Fetch::Idle, "nothing fetched while the drag is still moving");
    t.step(-1);
    assert_eq!(t.poll(start), Fetch::Idle);
    assert_eq!(want(t.poll(past_debounce(&t))), "oid-r1", "and then it goes");
}

#[test]
fn only_one_fetch_runs_at_a_time() {
    let mut t = timeline();
    let oid = want(t.poll(past_debounce(&t)));
    assert_eq!(oid, "oid-r0");
    // Scrubbing on while it is in flight retargets but starts nothing new.
    t.step(-3);
    assert_eq!(t.poll(past_debounce(&t)), Fetch::Idle);
    deliver(&mut t, &oid);
    assert_eq!(want(t.poll(past_debounce(&t))), "oid-r3", "the new target goes once it is free");
}

/// A commit's tree is immutable, so ground already covered costs nothing.
#[test]
fn scrubbing_back_over_a_cached_revision_fetches_nothing() {
    let mut t = timeline();
    for oid in ["oid-r0", "oid-r1"] {
        let got = want(t.poll(past_debounce(&t)));
        assert_eq!(got, oid);
        deliver(&mut t, &got);
        t.step(-1);
    }
    // Back to r0, which is cached: it becomes current with no fetch of its own.
    t.seek(0);
    let f = t.poll(past_debounce(&t));
    assert!(t.tree().is_some(), "the scene can be drawn immediately");
    assert_eq!(t.current().unwrap().short, "r0");
    // Anything it does ask for is a prefetch, never the revision on screen.
    if let Fetch::Want { oid, .. } = f {
        assert_ne!(oid, "oid-r0");
    }
}

/// While you look at one commit, the next one along is already coming.
#[test]
fn the_next_revision_along_is_prefetched() {
    let mut t = timeline();
    let first = want(t.poll(past_debounce(&t)));
    deliver(&mut t, &first);
    t.step(-1); // heading back in time
    let second = want(t.poll(past_debounce(&t)));
    deliver(&mut t, &second);
    // Settled on r1 with nothing owed, so the spare capacity goes to r2.
    assert_eq!(want(t.poll(past_debounce(&t))), "oid-r2", "the one we are heading for");
}

/// A reply that arrives after the user has scrubbed onward is kept — it cost a
/// `git` call already — but it must not become what the scene shows.
#[test]
fn a_stale_reply_is_cached_but_not_shown() {
    let mut t = timeline();
    let slow = want(t.poll(past_debounce(&t)));
    assert_eq!(slow, "oid-r0");
    t.step(-2);
    deliver(&mut t, &slow);
    assert!(t.tree().is_none(), "the scene is not showing a revision we left");
    assert_eq!(t.current().unwrap().short, "r2");
    // Scrubbing back to it needs no second fetch.
    t.seek(0);
    t.poll(past_debounce(&t));
    assert!(t.tree().is_some(), "but it was kept");
}

#[test]
fn a_failed_fetch_does_not_wedge_the_timeline() {
    let mut t = timeline();
    let oid = want(t.poll(past_debounce(&t)));
    t.fail(&oid);
    assert_eq!(want(t.poll(past_debounce(&t))), oid, "it can be tried again");
}

#[test]
fn the_delivered_tree_is_built_at_the_repository_root() {
    let mut t = timeline();
    t.poll(past_debounce(&t));
    t.deliver("oid-r0".into(), vec![("src/a.rs".into(), 10), ("b".into(), 5)]);
    let tree = t.tree().expect("delivered");
    // Paths must match what the scene already knows, or nothing would line up.
    assert_eq!(tree.total_of(Path::new("/repo")).0, 15);
    assert_eq!(tree.total_of(Path::new("/repo/src")).0, 10);
}

/// Each delivery stamps a new epoch, which is how the scene notices a swap. Two
/// revisions holding the same number of directories must not look identical.
#[test]
fn every_delivery_stamps_a_fresh_epoch() {
    let mut t = timeline();
    t.poll(past_debounce(&t));
    t.deliver("oid-r0".into(), vec![("a".into(), 1)]);
    let first = t.tree().unwrap().dirs_seen;
    t.seek(1);
    t.poll(past_debounce(&t));
    t.deliver("oid-r1".into(), vec![("b".into(), 1)]);
    let second = t.tree().unwrap().dirs_seen;
    assert_ne!(first, second, "the same shape at a different revision still reads as new");
}

#[test]
fn the_cache_is_bounded_but_never_evicts_what_is_on_screen() {
    let revs = (0..(CACHE_MAX + 10)).map(|i| rev(&format!("r{i}"), 1_000 - i as i64)).collect();
    let mut t = Timeline::new(0, PathBuf::from("/repo"), revs);
    for i in 0..(CACHE_MAX + 10) {
        t.seek(i);
        t.poll(past_debounce(&t));
        deliver(&mut t, &format!("oid-r{i}"));
    }
    assert!(t.cache.len() <= CACHE_MAX + 1, "bounded: {}", t.cache.len());
    assert!(t.tree().is_some(), "and the revision on screen survived the eviction");
}

#[test]
fn an_empty_history_is_harmless() {
    let mut t = Timeline::new(0, PathBuf::from("/repo"), Vec::new());
    assert!(t.current().is_none());
    assert_eq!(t.poll(Instant::now()), Fetch::Idle);
    t.step(-1);
    assert!(t.tree().is_none());
    assert_eq!(t.position(), (0, 0));
}
