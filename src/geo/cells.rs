//! The map in character cells, for a terminal without graphics.
//!
//! Each cell is two braille dots across and four down — about square dots, as
//! a cell is about twice as tall as it is wide — and the map is laid out on
//! that grid of dots with the same geometry the pixel map uses. The areas pick
//! each cell's background (sea, land, a GeoJSON polygon) by what most of its
//! dots are; the lines (coasts, borders, rivers, GeoJSON) become the braille
//! dots drawn over it, in the colour of the most important line through the
//! cell. It works on a 16-colour terminal too: every colour is one of the
//! theme's own.

use super::cover::Coverage;
use super::draw::{self, Part, Scene, Widths, cities, fill_layer, is_seam, stroke_layer};
use super::palette::CellPalette;
use super::world::world;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// What a dot's area is, in rising order of what a cell shows first.
const SEA: u8 = 0;
const LAND: u8 = 1;
const FEATURE_AREA: u8 = 2;

/// What line a dot is on, in rising order of importance.
const COAST: u8 = 1;
const BORDER: u8 = 2;
const RIVER: u8 = 3;
const DIM: u8 = 4;
const FEATURE: u8 = 5;
const SELECTED: u8 = 6;

/// Braille bit for dot (`dx`, `dy`) of a cell.
const BRAILLE: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Draw `scene` into `area`.
pub fn render(f: &mut Frame, area: Rect, scene: &Scene, pal: &CellPalette) {
    let (cols, rows) = (area.width as usize, area.height as usize);
    if cols == 0 || rows == 0 {
        return;
    }
    let (w, h) = (cols as u32 * 2, rows as u32 * 4);
    let p = scene.view.project(w, h);
    let world = world();
    let widths = Widths::for_scale(0.5);
    let n = (w * h) as usize;
    let mut cov = Coverage::new(w, h);

    // The areas, as ids per dot.
    let mut area_ids = vec![SEA; n];
    let mut mark_area = |cov: &mut Coverage, id: u8, draw: &mut dyn FnMut(&mut Coverage)| {
        cov.clear();
        draw(cov);
        cov.for_each(|x, y, c| {
            if c >= 0.5 {
                area_ids[y * w as usize + x] = id;
            }
        });
    };
    mark_area(&mut cov, LAND, &mut |c| fill_layer(c, &p, &world.land));
    mark_area(&mut cov, SEA, &mut |c| fill_layer(c, &p, &world.lakes));
    let doc = scene.doc;
    mark_area(&mut cov, FEATURE_AREA, &mut |c| {
        draw::features(c, &p, doc, Part::Fill, 0.0, |_, _| true)
    });

    // The lines, keeping the most important one on each dot.
    let mut line_ids = vec![0u8; n];
    let mut mark_line = |cov: &mut Coverage, id: u8, draw: &mut dyn FnMut(&mut Coverage)| {
        cov.clear();
        draw(cov);
        cov.for_each(|x, y, c| {
            let i = y * w as usize + x;
            if c >= 0.3 && line_ids[i] < id {
                line_ids[i] = id;
            }
        });
    };
    mark_line(&mut cov, COAST, &mut |c| {
        stroke_layer(c, &p, &world.land, true, |_| widths.coast, is_seam);
        stroke_layer(c, &p, &world.lakes, true, |_| widths.coast, |_, _| false);
    });
    mark_line(&mut cov, BORDER, &mut |c| {
        stroke_layer(c, &p, &world.borders, false, |_| widths.border, |_, _| false)
    });
    mark_line(&mut cov, RIVER, &mut |c| {
        // Only the bigger rivers: at this resolution the rest are noise.
        stroke_layer(
            c,
            &p,
            &world.rivers,
            false,
            |rank| if rank >= 4 { widths.river } else { 0.0 },
            |_, _| false,
        )
    });
    let chosen = |oi: usize| scene.object.is_none_or(|o| o == oi);
    let picked = |oi: usize, fi: usize| scene.selected == Some((oi, fi));
    for (id, mine) in [(DIM, false), (FEATURE, true)] {
        let include = |oi: usize, fi: usize| chosen(oi) == mine && !picked(oi, fi);
        mark_line(&mut cov, id, &mut |c| draw::features(c, &p, doc, Part::Stroke, 1.0, include));
    }
    mark_line(&mut cov, SELECTED, &mut |c| draw::features(c, &p, doc, Part::Stroke, 1.4, picked));

    // Cell by cell: the background most of its dots have, the line dots in
    // braille over it.
    let mut cells: Vec<Vec<(char, Style)>> = Vec::with_capacity(rows);
    for cy in 0..rows {
        let mut row = Vec::with_capacity(cols);
        for cx in 0..cols {
            let mut counts = [0u8; 3];
            let mut bits = 0u8;
            let mut top = 0u8;
            for (dx, column) in BRAILLE.iter().enumerate() {
                for (dy, bit) in column.iter().enumerate() {
                    let i = (cy * 4 + dy) * w as usize + cx * 2 + dx;
                    counts[area_ids[i] as usize] += 1;
                    if line_ids[i] > 0 {
                        bits |= bit;
                        top = top.max(line_ids[i]);
                    }
                }
            }
            let area = (0..3).rev().max_by_key(|&a| counts[a]).unwrap_or(0);
            let bg = match area as u8 {
                LAND => pal.land,
                FEATURE_AREA => pal.feature_fill,
                _ => pal.ocean,
            };
            let fg = line_colour(top, pal);
            let ch = if bits == 0 {
                ' '
            } else {
                char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
            };
            row.push((ch, Style::default().fg(fg).bg(bg)));
        }
        cells.push(row);
    }

    // Points as a solid mark in their cell.
    for (oi, o) in doc.objects.iter().enumerate() {
        for (fi, feature) in o.features.iter().enumerate() {
            let colour = if picked(oi, fi) {
                pal.selected
            } else if chosen(oi) {
                pal.feature
            } else {
                pal.feature_dim
            };
            for s in &feature.shapes {
                let super::geojson::Shape::Point(q) = s else { continue };
                for off in p.copies(q[0] - 360.0, q[0] + 360.0) {
                    let (x, y) = p.xy(q[0] + off, q[1]);
                    let (cx, cy) = ((x / 2.0).floor(), (y / 4.0).floor());
                    if cx >= 0.0 && cy >= 0.0 && (cx as usize) < cols && (cy as usize) < rows {
                        let cell = &mut cells[cy as usize][cx as usize];
                        *cell = ('●', cell.1.fg(colour));
                    }
                }
            }
        }
    }

    // City names where they fit, biggest cities first.
    let budget = (cols * rows / 60).clamp(2, 40);
    let mut taken = vec![false; cols * rows];
    for ((x, y), name, _, _) in cities(&p, budget) {
        let (cx, cy) = ((x / 2.0).floor() as isize, (y / 4.0).floor() as isize);
        let label: Vec<char> = std::iter::once('•').chain(name.chars()).collect();
        let end = cx + label.len() as isize + 1;
        if cx < 0 || cy < 0 || end > cols as isize || cy >= rows as isize {
            continue;
        }
        let (cy, cx) = (cy as usize, cx as usize);
        let span = cy * cols + cx.saturating_sub(1)..cy * cols + end as usize;
        if taken[span.clone()].iter().any(|&t| t) {
            continue;
        }
        taken[span].iter_mut().for_each(|t| *t = true);
        for (i, c) in label.into_iter().enumerate() {
            let cell = &mut cells[cy][cx + i];
            *cell = (c, cell.1.fg(pal.label));
        }
    }

    let lines: Vec<Line> = cells
        .into_iter()
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut style = None;
            for (c, s) in row {
                if style != Some(s) && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), style.unwrap_or_default()));
                }
                style = Some(s);
                run.push(c);
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, style.unwrap_or_default()));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn line_colour(id: u8, pal: &CellPalette) -> Color {
    match id {
        COAST => pal.coast,
        BORDER => pal.border,
        RIVER => pal.river,
        DIM => pal.feature_dim,
        FEATURE => pal.feature,
        SELECTED => pal.selected,
        _ => pal.coast,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::geojson::extract;
    use crate::geo::view::MapView;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn the_map_in_cells_has_coasts_in_braille_land_behind_them_and_the_data_on_top() {
        let doc = extract(
            r#"{"type":"Feature","geometry":{"type":"LineString","coordinates":[[-10,40],[30,40]]}}"#,
        );
        let scene = Scene {
            view: MapView { clon: 10.0, clat: 40.0, width: 60.0 },
            doc: &doc,
            object: None,
            selected: Some((0, 0)),
        };
        let theme = crate::ui::theme::Theme::mc();
        let pal = CellPalette::from_theme(&theme);
        let mut t = Terminal::new(TestBackend::new(60, 20)).unwrap();
        t.draw(|f| render(f, f.area(), &scene, &pal)).unwrap();
        let b = t.backend().buffer();
        let all: Vec<&ratatui::buffer::Cell> = b.content().iter().collect();
        assert!(all.iter().any(|c| c.bg == pal.land), "there is land in view");
        assert!(all.iter().any(|c| c.bg == pal.ocean), "and sea");
        assert!(
            all.iter().any(|c| c
                .symbol()
                .chars()
                .next()
                .is_some_and(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))),
            "lines are braille"
        );
        // The selected line runs across the middle row, in the selection colour.
        let middle: Vec<_> = (0..60).map(|x| &b[(x, 10)]).collect();
        assert!(middle.iter().filter(|c| c.fg == pal.selected).count() > 20, "the line along 40°N");
    }
}
