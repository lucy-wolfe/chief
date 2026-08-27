//! Drawing the rail.
//!
//! Every row index this draws at must agree with [`super::click`]'s reading of
//! the same index — they are two halves of one contract, and a drift between
//! them is a rail that selects the row above the one the operator pointed at.
//! `super::tests` pins the contract from the click side; this file is
//! deliberately arithmetic-free beyond the offsets it is handed.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{department_indent, TreeRow, View, TREE_GUTTER};

/// The collapse control: two arrows, at the foot of the rail.
///
/// Pointing LEFT when the rail is open (click to push it away) and RIGHT when
/// it is collapsed (click to bring it back). A collapsed rail is four columns
/// wide, which is exactly what this row needs.
const COLLAPSE: &str = "<<";
/// The same control, collapsed.
const EXPAND: &str = ">>";

/// One disclosure language for every department.
const EXPANDED_DEPARTMENT: &str = "\u{2212}";
const COLLAPSED_DEPARTMENT: &str = "+";

/// One compact state language, in the same column as department disclosure.
const WORKING: &str = "\u{25cf}";
/// Idle. A SYMMETRIC concentric glyph, for the reason `department_card`'s own
/// `glyph` states at length: `\u{25d0}` does not share an optical centre with
/// the rings beside it in most terminal fonts, and reads as sitting low. This
/// surface draws the same state as that one — a second vocabulary would drift
/// the first time either moved.
const IDLE: &str = "\u{25ce}";
const STARTING: &str = "\u{25cc}";
/// A person whose boot keeps dying. A filled ring, not the dotted one:
/// `starting` is one motion under way and this is the same motion repeating.
const CRASHING: &str = "\u{25c9}";
/// A person chiefd's launch gate has DECLINED. A barred circle: the ring is
/// struck through, because nothing is going to happen for this person until
/// somebody fixes what the gate named.
const REFUSED: &str = "\u{2298}";
const SLEEPING: &str = "\u{25cf}";
/// The icon and colour that state a person's current runtime condition.
const fn status_icon(state: super::PersonState, light: bool) -> (&'static str, Color) {
    match (state, light) {
        (super::PersonState::Working, true) => (WORKING, Color::Rgb(0x00, 0x5e, 0x00)),
        (super::PersonState::Idle, true) => (IDLE, Color::Rgb(0x53, 0x53, 0x00)),
        (super::PersonState::Starting, true) => (STARTING, Color::Rgb(0x00, 0x5a, 0x5a)),
        (super::PersonState::Crashing, true) => (CRASHING, Color::Rgb(0x8a, 0x2b, 0x00)),
        (super::PersonState::Refused, true) => (REFUSED, Color::Rgb(0x7a, 0x00, 0x5e)),
        (super::PersonState::Sleeping, true) => (SLEEPING, Color::Rgb(0xff, 0x00, 0x00)),
        (super::PersonState::Working, false) => (WORKING, Color::Rgb(0x00, 0xc5, 0x00)),
        (super::PersonState::Idle, false) => (IDLE, Color::Rgb(0xaf, 0xaf, 0x00)),
        (super::PersonState::Starting, false) => (STARTING, Color::Rgb(0x00, 0xbd, 0xbd)),
        (super::PersonState::Crashing, false) => (CRASHING, Color::Rgb(0xff, 0x8c, 0x2b)),
        (super::PersonState::Refused, false) => (REFUSED, Color::Rgb(0xff, 0x5f, 0xd7)),
        (super::PersonState::Sleeping, false) => (SLEEPING, Color::Rgb(0xff, 0x00, 0x00)),
    }
}

/// What marks the person who HEADS the department being listed.
///
/// A word and not a symbol: the operator asked for the head to be named as one
/// — "ada (manager) idle" — and a glyph would need a legend the rail has no
/// room for. The leading space is part of it, so a row with no badge has no
/// stray gap.
const MANAGER: &str = " (manager)";

/// COLUMNS THE ROW MAY NOT WRITE IN, one at each edge.
///
/// The operator photographed both failures at once: `Departments` starting in
/// column ZERO, hard against the pane border, and `Portfolio Management (0)`
/// running off the right-hand edge mid-word. A terminal pane has no margin of
/// its own, so if this file does not reserve one there is not one.
///
/// The LEFT column is a quiet margin. Selection is a full-width background, so
/// it needs no separate arrow in that margin. The RIGHT column is reserved and
/// never written, which keeps a full-width row clear of the border.
const GUTTER: usize = TREE_GUTTER;
/// See [`GUTTER`]. Reserved at the right-hand edge, never drawn in.
const MARGIN: usize = 1;

/// What a truncated row ends with.
///
/// One cell, so the arithmetic that reserves room for it is `width - 1` and not
/// a guess. Clipping mid-word — which is what a rail with no truncation does —
/// reads as a rendering fault rather than as "there is more text here".
const ELLIPSIS: char = '\u{2026}';

/// Cut `text` to `width` columns, ending in [`ELLIPSIS`] when anything was lost.
///
/// Counts CHARACTERS, which is right for this surface and not in general: every
/// glyph the rail draws is one terminal cell (names are roster text, the badges
/// and dots below are all single-width). A CJK name would need a width-aware
/// crate; nothing here produces one, and pulling a dependency for a case the
/// product cannot currently reach is the speculative kind of work this repo
/// rules out.
fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push(ELLIPSIS);
    out
}

/// `left` at the left, `right` at the right, one space of separation minimum.
///
/// The LEFT side yields. A name is the thing that can be arbitrarily long and
/// the thing the operator can still identify from a prefix; the state tag beside
/// it is one of four known words and is useless truncated. So the name takes the
/// ellipsis and the tag is always drawn whole.
fn justify(left: &str, right: &str, width: usize) -> String {
    let right_len = right.chars().count();
    if right_len >= width {
        return fit(right, width);
    }
    // One space minimum between them, hence the extra 1.
    let room = width - right_len - 1;
    let left = fit(left, room);
    let gap = width - right_len - left.chars().count();
    format!("{left}{:gap$}{right}", "", gap = gap)
}

/// The style of a selected row: purple carried across the whole row.
///
/// The operator asked for "some kind of selection so even if the selection text
/// uses some color, you know it". The container is unmistakable at any width
/// and costs no text columns.
fn selected_row(light: bool) -> Style {
    selection_style_for(light)
}

/// Use the terminal's ANSI palette so Automatic theme changes remain the
/// colour authority.
///
/// Read on EVERY draw, and read through [`crate::appearance`] rather than
/// here: the bridge file, `TRIBES_THEME` and `COLORFGBG` are one derivation
/// with one order of precedence, and the actuator now seeds a pane's own
/// `COLORFGBG` from that same answer. A second copy of the rule in this file
/// is exactly the drift that would put a light rail beside a dark pane.
fn light_terminal_background() -> bool {
    crate::appearance::read_is_light()
}

/// The selected container palette for each terminal ground.
pub(super) fn selection_style_for(light: bool) -> Style {
    let (foreground, background) = if light {
        (Color::Rgb(0x5b, 0x21, 0xb6), Color::Rgb(0xed, 0xe7, 0xf6))
    } else {
        (Color::Rgb(0xd8, 0xb4, 0xfe), Color::Rgb(0x2e, 0x10, 0x65))
    };
    Style::default().fg(foreground).bg(background).add_modifier(Modifier::BOLD)
}

/// Draw the whole rail into `frame`.
pub fn draw(frame: &mut Frame<'_>, view: &View) {
    draw_with_appearance(frame, view, light_terminal_background());
}

pub(super) fn draw_with_appearance(frame: &mut Frame<'_>, view: &View, light: bool) {
    let area = frame.area();
    let height = usize::from(area.height);

    if view.collapsed() {
        // A collapsed rail is the control and nothing else. It still shows how
        // many departments are behind it, so the operator knows what they
        // pushed away.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}", view.departments().len()),
                dim(),
            ))),
            Rect { x: area.x, y: area.y, width: area.width, height: 1 },
        );
        draw_control(frame, area, view);
        return;
    }

    let width = usize::from(area.width);
    let visible = height.saturating_sub(1);
    let tree = view.tree_rows();
    let offset = view.scroll_offset();
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(height);
    for tree_row in tree.iter().skip(offset).take(visible) {
        lines.push(match tree_row {
            TreeRow::DepartmentSpacer(_) => Line::default(),
            TreeRow::Department(department) => {
                let chosen = view.selected() == Some(department.id.as_str())
                    && view.selected_person().is_none();
                let disclosure = if view.is_expanded(&department.id) {
                    EXPANDED_DEPARTMENT
                } else {
                    COLLAPSED_DEPARTMENT
                };
                // The row steps right one level per level below a top-level
                // department, so a sub-department renders nested under its
                // parent exactly as the store nests it, while the root and its
                // own children share the flush-left column. The indentation lives inside the body, before
                // the disclosure marker, so the disclosure and label move
                // together and the full-width selection card is untouched.
                let indent = department_indent(department.depth);
                let pad = " ".repeat(indent);
                let body = format!("{pad}{disclosure} {}", department.name);
                let count = format!("{}/{}", department.live, department.total);
                let content_width = width.saturating_sub(GUTTER + MARGIN);
                let label_len = fit(&body, content_width.saturating_sub(count.chars().count() + 1))
                    .chars()
                    .count()
                    .saturating_sub(indent + disclosure.chars().count() + 1);
                let text = justify(&body, &count, content_width);
                department_row(
                    format!(" {text}"),
                    GUTTER + indent + disclosure.chars().count() + 1,
                    label_len,
                    width,
                    chosen,
                    if department.live == 0 { dim() } else { Style::default() },
                    light,
                )
            }
            TreeRow::Person(department, person) => {
                let chosen = view.selected_person() == Some(person.id.as_str());
                let state = person.state();
                let (status, _) = status_icon(state, light);
                let badge = if person.manager { MANAGER } else { "" };
                // The name alone. The rail used to print `Name @name` after it,
                // but that handle is `person_short_identity(person.name)` — a
                // pure function of the very text beside it, so it added no fact
                // and doubled every row. The handle stays where it identifies
                // one thing: the pane border and the window name.
                let head = format!("{status} {}{badge}", person.name);
                // A person sits one status cell under their department's own
                // indented disclosure column, so the head reads as living in the
                // sub-department rather than out-dented from its own header.
                let indent = department_indent(department.depth);
                let pad = " ".repeat(indent);
                person_row(
                    format!(" {pad}{}", fit(&head, width.saturating_sub(GUTTER + MARGIN + indent))),
                    width,
                    chosen,
                    state,
                    person.manager,
                    light,
                )
            }
            TreeRow::Role(department, person) => {
                let chosen = view.selected_person() == Some(person.id.as_str());
                let indent = department_indent(department.depth);
                let pad = " ".repeat(indent);
                full_row(
                    format!(
                        "   {pad}{}",
                        fit(&person.title, width.saturating_sub(GUTTER + MARGIN + 2 + indent))
                    ),
                    width,
                    chosen,
                    dim(),
                    light,
                )
            }
        });
    }
    if !view.is_read() {
        lines.clear();
        lines.push(Line::from(Span::styled(unread_row(view), dim())));
    } else if tree.is_empty() {
        lines.push(Line::from(Span::styled(" Nobody works here", dim())));
    }

    frame.render_widget(
        Paragraph::new(lines),
        Rect { x: area.x, y: area.y, width: area.width, height: area.height.saturating_sub(1) },
    );
    draw_control(frame, area, view);
}

/// Paint one complete row. Selected rows include trailing spaces so their
/// background reaches both pane borders instead of stopping after the text.
fn full_row(
    text: String,
    width: usize,
    selected: bool,
    ordinary: Style,
    light: bool,
) -> Line<'static> {
    let fitted = fit(&text, width);
    let padding = width.saturating_sub(fitted.chars().count());
    let style = if selected { selected_row(light) } else { ordinary };
    Line::from(Span::styled(format!("{fitted}{:padding$}", ""), style))
}

/// Paint a department row with only its visible label made bold.
///
/// The disclosure cell and live count keep the row's prior style. A selected
/// row also keeps the existing full-width selection style; adding bold to the
/// label is idempotent there because that palette was already bold.
fn department_row(
    text: String,
    label_start: usize,
    label_len: usize,
    width: usize,
    selected: bool,
    ordinary: Style,
    light: bool,
) -> Line<'static> {
    let fitted = fit(&text, width);
    let padding = width.saturating_sub(fitted.chars().count());
    let padded = format!("{fitted}{:padding$}", "");
    let style = if selected { selected_row(light) } else { ordinary };
    let before = padded.chars().take(label_start).collect::<String>();
    let label = padded.chars().skip(label_start).take(label_len).collect::<String>();
    let after = padded.chars().skip(label_start.saturating_add(label_len)).collect::<String>();
    Line::from(vec![
        Span::styled(before, style),
        Span::styled(label, style.add_modifier(Modifier::BOLD)),
        Span::styled(after, style),
    ])
}

fn person_row(
    text: String,
    width: usize,
    selected: bool,
    state: super::PersonState,
    manager: bool,
    light: bool,
) -> Line<'static> {
    let fitted = fit(&text, width);
    let padding = width.saturating_sub(fitted.chars().count());
    let padded = format!("{fitted}{:padding$}", "");
    let ordinary = if selected {
        selected_row(light)
    } else if state.is_live() {
        Style::default()
    } else {
        dim()
    };
    let mut marks = Vec::new();
    let (status, colour) = status_icon(state, light);
    if let Some(at) = padded.find(status) {
        let style =
            if selected { selected_row(light).fg(colour) } else { Style::default().fg(colour) };
        marks.push((at, at + status.len(), style));
    }
    if manager {
        if let Some(at) = padded.find(MANAGER.trim_start()) {
            let manager_colour = selection_style_for(light).fg.unwrap_or(Color::Reset);
            let style =
                if selected { selected_row(light) } else { Style::default().fg(manager_colour) };
            marks.push((at, at + MANAGER.trim_start().len(), style));
        }
    }
    marks.sort_by_key(|mark| mark.0);
    let mut spans = Vec::new();
    let mut cursor = 0;
    for (start, end, style) in marks {
        if start > cursor {
            spans.push(Span::styled(padded[cursor..start].to_owned(), ordinary));
        }
        spans.push(Span::styled(padded[start..end].to_owned(), style));
        cursor = end;
    }
    if cursor < padded.len() {
        spans.push(Span::styled(padded[cursor..].to_owned(), ordinary));
    }
    Line::from(spans)
}

fn draw_control(frame: &mut Frame<'_>, area: Rect, view: &View) {
    let glyph = if view.collapsed() { EXPAND } else { COLLAPSE };
    let row = area.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {glyph}"), dim()))),
        Rect { x: area.x, y: area.y + row, width: area.width, height: 1 },
    );
}

/// What a rail draws where the company would be, before it has read one.
///
/// # Why there are two answers and not one
///
/// `…` means "still coming", and it is the truth for exactly as long as an
/// answer is still on its way — the moment between a rail's birth and its first
/// read. It becomes a LIE once a read has been tried and refused, because
/// nothing is on its way: the rail will ask again, but what the operator is
/// looking at is a failure, not a wait. A rail whose reads all fail used to
/// draw `…` for ever, so a company nobody could read was indistinguishable on
/// the glass from one that was about to appear.
///
/// Neither answer claims anything about who works at the company, so the
/// honesty rule is untouched: "Nobody works here" is still reachable only from
/// a company this rail has actually read.
fn unread_row(view: &View) -> &'static str {
    if view.is_unreadable() {
        " could not read the company"
    } else {
        " …"
    }
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
