use super::*;

const KINDS: [SaverKind; 4] =
    [SaverKind::Starfield, SaverKind::Matrix, SaverKind::Clock, SaverKind::Pipes];

/// Draw into the middle of a larger buffer, returning how many cells show
/// something and whether anything outside `area` was touched.
fn frame(saver: &mut Saver, area: Rect) -> (usize, bool) {
    let full = Rect::new(0, 0, area.right() + 3, area.bottom() + 2);
    let mut buf = Buffer::empty(full);
    saver.render(&mut buf, area);
    let mut lit = 0;
    let mut outside = false;
    for y in 0..full.height {
        for x in 0..full.width {
            let cell = &buf[(x, y)];
            if area.contains((x, y).into()) {
                lit += usize::from(cell.symbol() != " ");
            } else if cell != &ratatui::buffer::Cell::default() {
                outside = true;
            }
        }
    }
    (lit, outside)
}

#[test]
fn every_animation_runs_long_and_stays_inside_its_area() {
    for kind in KINDS {
        let mut saver = Saver::new(kind, Rng::new(7));
        assert_eq!(saver.kind(), kind);
        let area = Rect::new(2, 1, 80, 24);
        let mut shown = 0;
        for tick in 0..2000 {
            saver.step();
            if tick % 50 == 0 {
                let (lit, outside) = frame(&mut saver, area);
                assert!(!outside, "{kind:?} drew outside its area at tick {tick}");
                shown = shown.max(lit);
            }
        }
        assert!(shown > 10, "{kind:?} shows something ({shown} cells)");
    }
}

#[test]
fn a_resize_starts_the_animation_afresh_at_the_new_size() {
    for kind in KINDS {
        let mut saver = Saver::new(kind, Rng::new(3));
        frame(&mut saver, Rect::new(0, 0, 120, 40));
        for _ in 0..100 {
            saver.step();
        }
        // Much smaller, then tiny: nothing may index past the new edges.
        for area in [Rect::new(0, 0, 30, 10), Rect::new(0, 0, 3, 2)] {
            let (_, outside) = frame(&mut saver, area);
            assert!(!outside, "{kind:?} at {area:?}");
            for _ in 0..200 {
                saver.step();
            }
            frame(&mut saver, area);
        }
    }
}

#[test]
fn random_picks_one_of_the_animations() {
    let kinds: std::collections::HashSet<_> = (0..64)
        .map(|seed| format!("{:?}", Saver::new(SaverKind::Random, Rng::new(seed)).kind()))
        .collect();
    assert!(kinds.len() > 1, "{kinds:?}");
    assert!(!kinds.contains("Random"));
}

#[test]
fn nothing_moves_before_the_first_draw() {
    let mut saver = Saver::new(SaverKind::Pipes, Rng::new(1));
    saver.step(); // no size yet: must not panic
    let (lit, _) = frame(&mut saver, Rect::new(0, 0, 40, 12));
    assert_eq!(lit, 0, "a fresh pipe screen starts empty");
}
