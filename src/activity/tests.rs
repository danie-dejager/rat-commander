use super::*;

fn log() -> ActivityLog {
    let mut log = ActivityLog::default();
    log.set_root(Some(PathBuf::from("/proj")));
    log
}

fn ev(kind: FsKind, path: &str) -> FsEvent {
    FsEvent::new(kind, path)
}

fn rows(log: &ActivityLog) -> Vec<(FsKind, String, u32)> {
    log.visible().map(|e| (e.kind, e.rel.display().to_string(), e.count)).collect()
}

#[test]
fn a_burst_of_writes_folds_into_one_row_with_a_count() {
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::Modify, "/proj/app.log"), t);
    for i in 1..400 {
        log.record(&ev(FsKind::Modify, "/proj/app.log"), t + Duration::from_millis(i));
    }
    log.record(&ev(FsKind::Written, "/proj/app.log"), t + Duration::from_millis(500));
    assert_eq!(rows(&log), vec![(FsKind::Written, "app.log".into(), 401)]);

    // Much later, the same file starts a new row.
    log.record(&ev(FsKind::Modify, "/proj/app.log"), t + Duration::from_secs(60));
    assert_eq!(log.visible_len(), 2);
}

#[test]
fn a_new_file_being_written_still_reads_as_created() {
    let mut log = log();
    let t = Instant::now();
    log.record(&FsEvent { dir: false, ..ev(FsKind::Create, "/proj/out/a.o") }, t);
    log.record(&ev(FsKind::Modify, "/proj/out/a.o"), t);
    log.record(&ev(FsKind::Written, "/proj/out/a.o"), t);
    assert_eq!(rows(&log), vec![(FsKind::Create, "out/a.o".into(), 3)]);
}

#[test]
fn a_rename_replaces_the_halves_reported_before_it() {
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::MovedAway, "/proj/old.txt"), t);
    log.record(&ev(FsKind::MovedHere, "/proj/new.txt"), t);
    let both = FsEvent { to: Some("/proj/new.txt".into()), ..ev(FsKind::Rename, "/proj/old.txt") };
    log.record(&both, t);
    let e = log.selected().unwrap();
    assert_eq!((e.kind, e.to.as_deref()), (FsKind::Rename, Some(Path::new("new.txt"))));
    assert_eq!(log.visible_len(), 1);
}

#[test]
fn events_outside_the_root_or_on_the_root_itself_are_not_listed() {
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::Create, "/elsewhere/x"), t);
    log.record(&ev(FsKind::Modify, "/proj"), t);
    log.record(&ev(FsKind::Create, "/projects/x"), t);
    assert_eq!(log.visible_len(), 0);
}

#[test]
fn the_filter_matches_relative_paths() {
    let mut log = log();
    let t = Instant::now();
    for p in ["/proj/src/main.rs", "/proj/README.md", "/proj/src/lib.rs"] {
        log.record(&ev(FsKind::Create, p), t);
    }
    log.set_filter(Some("*.rs"));
    assert_eq!(log.visible_len(), 2);
    log.set_filter(Some("readme"));
    assert_eq!(rows(&log), vec![(FsKind::Create, "README.md".into(), 1)]);
    log.set_filter(None);
    assert_eq!(log.visible_len(), 3);
}

#[test]
fn pausing_holds_events_back_until_resumed() {
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::Create, "/proj/a"), t);
    log.toggle_pause();
    log.record(&ev(FsKind::Create, "/proj/b"), t);
    log.record(&ev(FsKind::Create, "/proj/c"), t);
    assert_eq!((log.visible_len(), log.held()), (1, 2), "the rows stand still");
    log.toggle_pause();
    assert_eq!((log.visible_len(), log.held()), (3, 0));
}

#[test]
fn the_cursor_stays_on_its_row_as_new_ones_arrive_above() {
    let mut log = log();
    let t = Instant::now();
    for p in ["/proj/a", "/proj/b", "/proj/c"] {
        log.record(&ev(FsKind::Create, p), t);
    }
    log.move_cursor(1); // on "b"
    log.record(&ev(FsKind::Create, "/proj/d"), t);
    assert_eq!(log.selected().unwrap().rel, Path::new("b"));
    // At the top, it follows the newest.
    log.cursor = 0;
    log.record(&ev(FsKind::Create, "/proj/e"), t);
    assert_eq!(log.selected().unwrap().rel, Path::new("e"));
}

#[test]
fn enter_goes_to_the_file_or_to_where_it_was() {
    let root = std::env::temp_dir().join(format!("rc_activity_target_{}", std::process::id()));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/here.txt"), "x").unwrap();
    let mut log = ActivityLog::default();
    log.set_root(Some(root.clone()));
    let t = Instant::now();
    log.record(&ev(FsKind::Remove, root.join("sub/gone.txt").to_str().unwrap()), t);
    assert_eq!(log.target(), Some((root.join("sub"), None)), "a removed file's directory");
    log.record(&ev(FsKind::Modify, root.join("sub/here.txt").to_str().unwrap()), t);
    assert_eq!(log.target(), Some((root.join("sub"), Some("here.txt".into()))));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_new_root_starts_the_log_afresh_but_keeps_the_filter() {
    let mut log = log();
    log.set_filter(Some("*.rs"));
    log.record(&ev(FsKind::Create, "/proj/a.rs"), Instant::now());
    log.set_root(Some(PathBuf::from("/other")));
    assert_eq!(log.visible_len(), 0);
    log.record(&ev(FsKind::Create, "/other/b.txt"), Instant::now());
    assert_eq!(log.visible_len(), 0, "still filtered");
}

#[test]
fn the_rate_counts_events_per_second() {
    let mut log = log();
    let t = Instant::now();
    for _ in 0..5 {
        log.record(&ev(FsKind::Modify, "/proj/x"), t);
    }
    log.record(&ev(FsKind::Modify, "/proj/y"), t + Duration::from_secs(2));
    assert_eq!(log.rate(), vec![5, 0, 1]);
    log.advance_rate(t + Duration::from_secs(200));
    assert_eq!(log.rate().len(), RATE_SECONDS);
    assert!(log.rate().iter().all(|&n| n == 0), "a quiet minute scrolls it all away");
}

#[test]
fn rows_draw_with_age_glyph_path_and_count() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let theme = crate::ui::theme::Theme::mc();
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::Create, "/proj/src/new.rs"), t);
    log.record(&ev(FsKind::Modify, "/proj/target/app.log"), t);
    log.record(&ev(FsKind::Modify, "/proj/target/app.log"), t);
    let mut term = Terminal::new(TestBackend::new(40, 4)).unwrap();
    term.draw(|f| {
        render::render(f, f.area(), &mut log, true, &theme, t + Duration::from_secs(3));
    })
    .unwrap();
    let b = term.backend().buffer();
    let line = |y: u16| -> String { (0..40).map(|x| b[(x, y)].symbol()).collect() };
    assert!(
        line(0).starts_with("  3s ~ target/app.log") && line(0).ends_with("×2"),
        "{:?}",
        line(0)
    );
    assert!(line(1).starts_with("  3s + src/new.rs"), "{:?}", line(1));
    assert_eq!(b[(5, 1)].fg, theme.exec_fg, "a creation's mark is green");
}

#[test]
fn long_paths_give_up_their_directories_before_the_name() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let theme = crate::ui::theme::Theme::mc();
    let mut log = log();
    let t = Instant::now();
    let rename = FsEvent {
        to: Some("/proj/target/debug/deps/libaxum-8b529b4a.rlib".into()),
        ..ev(FsKind::Rename, "/proj/target/debug/deps/.tmpd4e441")
    };
    log.record(&rename, t);
    log.record(
        &ev(FsKind::Create, "/proj/target/debug/.fingerprint/axum-d4e441c3/lib-axum.json"),
        t,
    );
    let mut term = Terminal::new(TestBackend::new(44, 3)).unwrap();
    term.draw(|f| {
        render::render(f, f.area(), &mut log, true, &theme, t);
    })
    .unwrap();
    let b = term.backend().buffer();
    let line = |y: u16| -> String { (0..44).map(|x| b[(x, y)].symbol()).collect() };
    assert!(line(0).contains("…/axum-d4e441c3/lib-axum.json"), "{:?}", line(0));
    assert!(line(1).contains(".tmpd4e441 → libaxum-8b529b4a.rlib"), "{:?}", line(1));
}

#[test]
fn a_file_appended_to_for_long_stops_saying_it_is_new() {
    let mut log = log();
    let t = Instant::now();
    log.record(&ev(FsKind::Create, "/proj/build.log"), t);
    for s in 1..=10 {
        log.record(&ev(FsKind::Modify, "/proj/build.log"), t + Duration::from_secs(s));
    }
    assert_eq!(rows(&log), vec![(FsKind::Modify, "build.log".into(), 11)]);
}
