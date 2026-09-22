//! Drawing the editor's tag view: a label column, a value column, and the
//! status row that says which file and which kind of tag is being edited.

use super::editor::{Row, TagEditor};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Widest the label column is allowed to get, so a long translated label cannot
/// crowd out the values it is labelling.
const MAX_LABEL: usize = 22;

/// Draw the field list into `area`. Returns where the caret goes when a value
/// is being typed into.
pub(crate) fn render(
    f: &mut Frame,
    area: Rect,
    te: &mut TagEditor,
    theme: &Theme,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let rows = te.rows();
    let label_w = rows
        .iter()
        .map(|r| te.row_text(r).0.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(1, MAX_LABEL.min(area.width.saturating_sub(4) as usize))
        + 2;

    let top = crate::util::scroll::scroll_to_visible(te.top(), te.cursor(), area.height as usize);
    te.set_view(area, top);

    let base = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    let label_style = Style::default().fg(theme.dialog_fg).bg(theme.panel_bg);
    let head =
        Style::default().fg(theme.panel_border).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    // An item the program has no name for is shown dimmed, to say it is carried
    // through a save rather than editable.
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);

    let mut caret = None;
    let vw = (area.width as usize).saturating_sub(label_w + 1);
    for (i, row) in rows.iter().skip(top).take(area.height as usize).enumerate() {
        let idx = top + i;
        let y = area.y + i as u16;
        let (label, value) = te.row_text(row);
        let selected = idx == te.cursor();
        let editable = matches!(row, Row::Field(_, _));

        if let Row::Heading(_) = row {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    pad_right(&format!("  {value}"), area.width as usize),
                    head,
                ))),
                Rect { x: area.x, y, width: area.width, height: 1 },
            );
            continue;
        }

        let vstyle = if selected && editable {
            theme.dialog_selection
        } else if editable {
            base
        } else {
            dim
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    pad_right(&format!("  {}", ellipsize(&label, label_w - 2)), label_w),
                    if editable { label_style } else { dim },
                ),
                Span::styled(pad_right(&ellipsize(&value, vw), vw), vstyle),
            ])),
            Rect { x: area.x, y, width: area.width, height: 1 },
        );

        // The caret sits in the value column of the row being typed into.
        if let Some((edit_row, cur)) = te.caret()
            && edit_row == idx
        {
            let col = area.x + label_w as u16 + (cur.min(vw.saturating_sub(1))) as u16;
            caret = Some(Position::new(col, y));
        }
    }
    caret
}

/// The status row: the file, whether it has been changed, how many fields are
/// set, and which kind of tag is being edited — so it is plain that an MP3 is
/// getting ID3v2 and an Ogg its Vorbis comments.
pub(crate) fn render_status(
    f: &mut Frame,
    area: Rect,
    name: &str,
    te: &TagEditor,
    dirty: bool,
    theme: &Theme,
) {
    let mark = if dirty { "[+]" } else { "   " };
    let kind = tag_kind(te.tag_type());
    let name = ellipsize(name, area.width.saturating_sub(40) as usize);
    let text = format!(" {name} {mark}  {kind}  Alt-T: {} ", "bytes");
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            pad_right(&text, area.width as usize),
            theme.menubar.add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// How a tag kind is named in the status row — the names the formats go by,
/// rather than lofty's identifiers.
fn tag_kind(t: lofty::tag::TagType) -> &'static str {
    use lofty::tag::TagType;
    match t {
        TagType::Id3v1 => "ID3v1",
        TagType::Id3v2 => "ID3v2",
        TagType::VorbisComments => "Vorbis comments",
        TagType::Mp4Ilst => "MP4 ilst",
        TagType::Ape => "APE",
        TagType::RiffInfo => "RIFF INFO",
        TagType::AiffText => "AIFF text",
        _ => "tags",
    }
}
