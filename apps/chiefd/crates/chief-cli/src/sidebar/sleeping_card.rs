//! The focused card for one sleeping person.
//!
//! Selection and wake are separate operator actions. The sidebar places this
//! program in the permanent focus body. This program asks the session brain to
//! wake the person only after the operator activates its button.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::{Frame, Terminal};

use super::wire::{Frames, ToBrain, ToClient, PROTOCOL};
use crate::actuate::launch_catalog::PersonModel;

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
const WAKE_REJECTED: &str = "Wake was not accepted. Select Wake Up to try again.";
/// What a card whose person the LAUNCH GATE has declined says where the button
/// would be. It is not a button and it never becomes one while the gate holds.
const CANNOT_START: &str = "Cannot start";
/// The mark a cut reason ends in, the same character and for the same reason
/// as the rail's own (`super::render`): a sentence that stops with no mark
/// reads as a rendering fault rather than as "there is more text here".
const ELLIPSIS: char = '\u{2026}';

/// Break a refusal into the lines the card draws it on.
///
/// **THE REASON IS THE WHOLE POINT OF THIS CARD, so it gets every row the card
/// has.** A `Paragraph` with no `wrap` draws each source line exactly once and
/// clips it at the widget width: a 111-character sentence painted 68 columns of
/// itself and dropped the rest, with no ellipsis and four empty rows below — and
/// the part it dropped was the second half of a filesystem path, which is the
/// half that says which directory to repair.
///
/// Wrapping happens at word boundaries. A single token longer than the whole
/// width — a long path with no space in it — is split hard, because there is
/// no boundary to use and every character kept beats any character dropped.
/// Only text that cannot fit even wrapped is cut, and a cut always ends in
/// [`ELLIPSIS`], at a word boundary when the line has one.
///
/// Counts CHARACTERS for the same reason `super::render::fit` does: every
/// glyph this surface draws is one terminal cell, and nothing in the product
/// produces a wide one.
fn wrap_reason(reason: &str, width: usize, height: usize) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in reason.split_whitespace() {
        let mut rest = word;
        while !rest.is_empty() {
            let used = line.chars().count();
            let gap = usize::from(used > 0);
            let free = width.saturating_sub(used + gap);
            if rest.chars().count() <= free {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(rest);
                break;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            let head: String = rest.chars().take(width).collect();
            rest = &rest[head.len()..];
            lines.push(head);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() <= height {
        return lines;
    }
    lines.truncate(height);
    let last = lines.pop().unwrap_or_default();
    lines.push(cut(&last, width));
    lines
}

/// End `line` in [`ELLIPSIS`] within `width` columns, giving up a whole word
/// rather than half of one whenever the line has a boundary to give up.
fn cut(line: &str, width: usize) -> String {
    if line.chars().count() < width {
        return format!("{line}{ELLIPSIS}");
    }
    if let Some(space) = line.rfind(' ') {
        let head = &line[..space];
        if !head.is_empty() && head.chars().count() < width {
            return format!("{head}{ELLIPSIS}");
        }
    }
    let mut out: String = line.chars().take(width.saturating_sub(1)).collect();
    out.push(ELLIPSIS);
    out
}

/// The facts displayed by the sleeping card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// Stable person identity used for the wake action.
    pub person_id: String,
    /// Roster display name.
    pub name: String,
    /// Exact company role from the roster.
    pub role: String,
    /// Backend-owned effective Pi model fact. It contains no source path.
    pub model: PersonModel,
    /// The backend's exact refusal from the last wake attempt, if any.
    ///
    /// A refusal about the COMPANY's state — benched, paused, not yours. The
    /// button stays, because asking again is a real repair for it.
    pub refusal: Option<String>,
    /// chiefd's LAUNCH GATE's own reason for declining to start this person.
    ///
    /// Different from [`Card::refusal`] in the one way the operator can act
    /// on: the gate re-derives this answer against the disk on every pass, so
    /// pressing a button cannot change it. A card carrying it therefore offers
    /// no button at all — see [`State::Blocked`].
    pub blocked: Option<String>,
}

/// The card's local presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The button is available and no wake was requested.
    Sleeping,
    /// The wake was accepted and this spinner frame is visible.
    Waking {
        /// Index of the visible spinner glyph.
        frame: usize,
    },
    /// chiefd's LAUNCH GATE has declined this person, so nothing this card can
    /// do will start them.
    ///
    /// **A state that can never advance must stop promising.** The card sat at
    /// `◓ Waking up…` for five minutes and nineteen seconds about a person the
    /// gate had already refused — two surfaces, two answers, one person, and
    /// the more prominent one was the wrong one. There is no spinner here and
    /// no button: the gate's sentence is the whole of what this card has to
    /// say, and it clears itself the pass after the repair lands.
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Palette {
    ground: Color,
    panel: Color,
    text: Color,
    muted: Color,
    button: Color,
    button_text: Color,
}

fn palette(light: bool) -> Palette {
    if light {
        Palette {
            ground: Color::Rgb(0xf3, 0xf4, 0xf6),
            panel: Color::Rgb(0xff, 0xff, 0xff),
            text: Color::Rgb(0x11, 0x18, 0x27),
            muted: Color::Rgb(0x4b, 0x55, 0x63),
            button: Color::Rgb(0x5b, 0x21, 0xb6),
            button_text: Color::Rgb(0xff, 0xff, 0xff),
        }
    } else {
        Palette {
            ground: Color::Rgb(0x11, 0x18, 0x27),
            panel: Color::Rgb(0x1f, 0x29, 0x37),
            text: Color::Rgb(0xf9, 0xfa, 0xfb),
            muted: Color::Rgb(0xd1, 0xd5, 0xdb),
            button: Color::Rgb(0x7c, 0x3a, 0xed),
            button_text: Color::Rgb(0xff, 0xff, 0xff),
        }
    }
}

/// The card shares the rail's one appearance authority; see
/// [`crate::appearance`] for why the derivation is not repeated here.
fn is_light() -> bool {
    crate::appearance::read_is_light()
}

/// The exact button rectangle. Rendering and mouse hit-testing share it.
#[must_use]
pub fn button_rect(area: Rect) -> Rect {
    let width = area.width.clamp(1, 28);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height.saturating_sub(3) / 2);
    Rect { x, y, width, height: area.height.min(3) }
}

/// Whether a terminal cell activates the button.
#[must_use]
pub fn hits_button(button: Rect, column: u16, row: u16) -> bool {
    column >= button.x
        && column < button.x.saturating_add(button.width)
        && row >= button.y
        && row < button.y.saturating_add(button.height)
}

fn activates_button(input: &Event, button: Rect) -> bool {
    match input {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            matches!(key.code, KeyCode::Enter | KeyCode::Char(' '))
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
            hits_button(button, mouse.column, mouse.row)
        }
        _ => false,
    }
}

/// THE STATE A CARD OPENS IN, decided by what the brain handed it.
///
/// A card the brain gave the LAUNCH GATE's sentence to is a card about
/// somebody who cannot be started, so it never offers a wake to press — and
/// the brain replaces it the pass after the refusal clears. A card carrying a
/// WAKE refusal opens ordinary: that answer was about one attempt, and asking
/// again is a real repair for it.
#[must_use]
pub const fn initial_state(card: &Card) -> State {
    if card.blocked.is_some() {
        State::Blocked
    } else {
        State::Sleeping
    }
}

/// Advance the visible waking animation.
#[must_use]
pub const fn next_animation(state: State) -> State {
    match state {
        State::Sleeping => State::Sleeping,
        State::Waking { frame } => State::Waking { frame: (frame + 1) % SPINNER.len() },
        // Nothing is in flight, so nothing animates.
        State::Blocked => State::Blocked,
    }
}

fn draw(frame: &mut Frame<'_>, card: &Card, model: &str, state: State, light: bool) -> Rect {
    let colors = palette(light);
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(colors.ground)), area);
    let width = area.width.saturating_sub(4).min(72);
    let height = area.height.saturating_sub(2).clamp(1, 19);
    let panel = Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area.y.saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    };
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors.muted))
            .style(Style::default().bg(colors.panel).fg(colors.text))
            .padding(Padding::horizontal(2)),
        panel,
    );
    let inner = Rect {
        x: panel.x.saturating_add(3),
        y: panel.y.saturating_add(2),
        width: panel.width.saturating_sub(6),
        height: panel.height.saturating_sub(4),
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(card.name.as_str())
            .alignment(Alignment::Center)
            .style(Style::default().fg(colors.text).add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(card.role.as_str())
            .alignment(Alignment::Center)
            .style(Style::default().fg(colors.muted)),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Model  ", Style::default().fg(colors.muted)),
            Span::styled(model, Style::default().fg(colors.text)),
        ]))
        .alignment(Alignment::Center),
        chunks[2],
    );
    // THE GATE'S SENTENCE OUTRANKS A WAKE REFUSAL, for the same reason the
    // rail's `refused` outranks `held`: only one of the two names a repair
    // that is still true. A wake refusal is a record of one older attempt.
    if let Some(refusal) = card.blocked.as_deref().or(card.refusal.as_deref()) {
        let reason = chunks[3];
        frame.render_widget(
            Paragraph::new(
                wrap_reason(refusal, usize::from(reason.width), usize::from(reason.height))
                    .join("\n"),
            )
            .alignment(Alignment::Center)
            .style(Style::default().fg(colors.text)),
            reason,
        );
    }
    let button = button_rect(chunks[4]);
    let (label, button_style) = match state {
        State::Sleeping => (
            "Wake Up".to_owned(),
            Style::default().bg(colors.button).fg(colors.button_text).add_modifier(Modifier::BOLD),
        ),
        State::Waking { frame } => (
            format!("{}  Waking up…", SPINNER[frame % SPINNER.len()]),
            Style::default().bg(colors.panel).fg(colors.text).add_modifier(Modifier::BOLD),
        ),
        // No verb, because there is no action. The word says what the gate
        // said and offers the operator nothing to press that would repeat it.
        State::Blocked => (
            CANNOT_START.to_owned(),
            Style::default().bg(colors.panel).fg(colors.muted).add_modifier(Modifier::BOLD),
        ),
    };
    frame.render_widget(
        Paragraph::new(label)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).border_style(button_style))
            .style(button_style),
        button,
    );
    button
}

fn model_label(model: &PersonModel) -> String {
    model.label()
}

fn activate(socket: &Path, pane: &str, person_id: &str) -> std::io::Result<bool> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.write_all(
        &ToBrain::WakePerson {
            protocol: PROTOCOL,
            pane: pane.to_owned(),
            person: person_id.to_owned(),
        }
        .encode(),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut frames = Frames::new();
    let mut bytes = [0_u8; 1024];
    loop {
        let count = stream.read(&mut bytes)?;
        if count == 0 {
            return Ok(false);
        }
        frames.feed(&bytes[..count]);
        match frames
            .next_to_client()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
        {
            Some(ToClient::WakeAccepted { person }) if person == person_id => return Ok(true),
            Some(ToClient::WakeRejected { person }) if person == person_id => return Ok(false),
            _ => {}
        }
    }
}

fn apply_activation(card: &mut Card, state: &mut State, accepted: bool) -> bool {
    if accepted {
        *state = State::Waking { frame: 0 };
    } else {
        card.refusal = Some(WAKE_REJECTED.to_owned());
    }
    accepted
}

struct Glass;

impl Glass {
    fn take() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut output = std::io::stdout();
        if let Err(error) = enter_screen(&mut output) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

fn enter_screen(output: &mut impl std::io::Write) -> std::io::Result<()> {
    if let Err(error) = execute!(output, EnterAlternateScreen, event::EnableMouseCapture) {
        let _ = execute!(output, event::DisableMouseCapture, LeaveAlternateScreen);
        return Err(error);
    }
    Ok(())
}

impl Drop for Glass {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), event::DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// Run the focused sleeping-person card until the actuator replaces this
/// process with Pi in the same pane.
pub fn run(company_dir: &Path, mut card: Card) -> std::io::Result<()> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "TMUX_PANE is not set"))?;
    let socket = company_dir.join(".chief/run/rail.sock");
    let _glass = Glass::take()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut state = initial_state(&card);
    let model = model_label(&card.model);
    let mut last_tick = Instant::now();
    loop {
        let mut button = Rect::default();
        terminal.draw(|frame| button = draw(frame, &card, &model, state, is_light()))?;
        let timeout = if matches!(state, State::Waking { .. }) {
            Duration::from_millis(90).saturating_sub(last_tick.elapsed())
        } else {
            Duration::from_millis(100)
        };
        if event::poll(timeout)? {
            let activate_button = activates_button(&event::read()?, button);
            if activate_button && state == State::Sleeping {
                let accepted = activate(&socket, &pane, &card.person_id)?;
                if apply_activation(&mut card, &mut state, accepted) {
                    last_tick = Instant::now();
                }
            }
        } else if matches!(state, State::Waking { .. }) {
            state = next_animation(state);
            last_tick = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::actuate::launch_catalog::PersonModelState;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn mouse_hitbox_is_the_exact_button_rectangle() {
        let button = button_rect(Rect::new(10, 20, 40, 3));
        assert!(hits_button(button, button.x, button.y));
        assert!(hits_button(button, button.right() - 1, button.bottom() - 1));
        assert!(!hits_button(button, button.right(), button.y));
        assert!(!hits_button(button, button.x, button.bottom()));
    }

    #[test]
    fn enter_and_space_activate_the_focused_button() {
        let button = Rect::new(1, 1, 10, 3);
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            assert!(activates_button(
                &Event::Key(crossterm::event::KeyEvent::new(
                    code,
                    crossterm::event::KeyModifiers::NONE
                )),
                button,
            ));
        }
        assert!(!activates_button(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            )),
            button,
        ));
    }

    #[test]
    fn waking_animation_rotates_and_never_returns_to_a_wake_button() {
        let mut state = State::Waking { frame: 0 };
        for expected in [1, 2, 3, 0] {
            state = next_animation(state);
            assert_eq!(state, State::Waking { frame: expected });
        }
    }

    fn rendered(light: bool, state: State) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let card = fixture_card(None);
        terminal
            .draw(|frame| {
                let model = model_label(&card.model);
                draw(frame, &card, &model, state, light);
            })
            .expect("draw");
        terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
    }

    fn fixture_card(refusal: Option<&str>) -> Card {
        Card {
            person_id: "ada".to_owned(),
            name: "Ada Lovelace".to_owned(),
            role: "Quant Analyst".to_owned(),
            model: PersonModel {
                state: PersonModelState::Selected,
                provider: Some("openai".to_owned()),
                model: Some("gpt-5.6".to_owned()),
            },
            refusal: refusal.map(str::to_owned),
            blocked: None,
        }
    }

    /// The same person, with chiefd's LAUNCH GATE's own sentence on them.
    fn blocked_card(reason: &str) -> Card {
        Card { blocked: Some(reason.to_owned()), ..fixture_card(None) }
    }

    #[test]
    fn light_and_dark_cards_keep_exact_person_facts_and_button_copy() {
        for light in [true, false] {
            let text = rendered(light, State::Sleeping);
            for expected in ["Ada Lovelace", "Quant Analyst", "openai/gpt-5.6", "Wake Up"] {
                assert!(text.contains(expected), "missing {expected:?} from {text:?}");
            }
        }
    }

    #[test]
    fn a_refused_card_keeps_the_exact_reason_visible_and_the_button_actionable() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let card = fixture_card(Some("This person is not staffed."));
        let model = model_label(&card.model);
        terminal
            .draw(|frame| {
                draw(frame, &card, &model, State::Sleeping, true);
            })
            .expect("draw");
        let text: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("This person is not staffed."));
        assert!(text.contains("Wake Up"));
    }

    /// The launch gate's own sentence about a live refused person: a long
    /// reason with a filesystem path in it, against a card 68 columns wide.
    const LONG_REASON: &str = "this person has no agent home \
(/root/companies/finalcheck-labs/.chief/agent/jordan); the next hire-path pass \
creates it";

    /// The card's rows, as the operator reads them.
    ///
    /// Row by row, NOT the whole buffer in one string: a wrapped sentence is
    /// spread over several rows with the card's border and padding between
    /// them, so the flat-buffer read the older tests use cannot see it at all.
    fn rendered_rows(card: &Card, state: State) -> Vec<String> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let model = model_label(&card.model);
        terminal
            .draw(|frame| {
                draw(frame, card, &model, state, true);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let width = usize::from(buffer.area.width);
        buffer
            .content
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    /// Every row's words in reading order, one space apart. Wrapping breaks a
    /// sentence at its spaces, so rejoining the rows this way reproduces the
    /// sentence exactly when — and only when — no character of it was lost.
    fn reading_order(rows: &[String]) -> String {
        rows.iter()
            .flat_map(|row| {
                row.chars()
                    .map(|glyph| if glyph == '│' { ' ' } else { glyph })
                    .collect::<String>()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// **THE REASON IS THE ONE THING THIS CARD EXISTS TO DELIVER.**
    ///
    /// On a live refused person the card painted
    /// `this person has no agent home (/root/companies/finalcheck-labs/.ch` —
    /// cut mid-path, with no ellipsis to say it had been cut, so the operator
    /// could not even read which directory to repair. Four rows under that
    /// text were blank. A `Paragraph` with no `wrap` clips at the widget width
    /// and the card is 68 columns of a 111-character sentence.
    ///
    /// A short reason passes over this defect exactly as the test above does,
    /// so this one drives a reason LONGER than the card is wide and asserts
    /// the WHOLE sentence is present across the rendered rows.
    #[test]
    fn a_reason_longer_than_the_card_is_wide_is_wrapped_whole_and_not_clipped() {
        for card in [blocked_card(LONG_REASON), fixture_card(Some(LONG_REASON))] {
            let rows = rendered_rows(&card, State::Sleeping);
            let text = reading_order(&rows);
            assert!(
                text.contains(LONG_REASON),
                "the whole sentence must survive the render: {text:?}"
            );
            assert!(
                !text.contains(ELLIPSIS),
                "and nothing is cut, because the card had the rows to spare: {text:?}"
            );
            assert!(
                rows.iter().any(|row| row.contains(".chief/agent/jordan")),
                "the path's own end is what says which directory to repair: {rows:?}"
            );
        }
    }

    /// A reason too long for the card even wrapped is CUT VISIBLY. Silence is
    /// the defect; an ellipsis is the honest answer.
    #[test]
    fn a_reason_too_long_for_the_whole_card_ends_in_a_visible_ellipsis() {
        let reason = format!("{LONG_REASON} {LONG_REASON} {LONG_REASON} {LONG_REASON}");
        let rows = rendered_rows(&blocked_card(&reason), State::Sleeping);
        let text = reading_order(&rows);
        assert!(text.contains(ELLIPSIS), "a cut says it is a cut: {text:?}");
        assert!(
            text.contains("this person has no agent home"),
            "and the sentence still starts where it started: {text:?}"
        );
    }

    #[test]
    fn wrapping_keeps_every_word_and_never_exceeds_the_width() {
        let lines = wrap_reason(LONG_REASON, 68, 6);
        assert!(lines.len() > 1, "111 characters do not fit on one 68-column row: {lines:?}");
        assert!(lines.iter().all(|line| line.chars().count() <= 68), "{lines:?}");
        assert_eq!(lines.join(" "), LONG_REASON);
    }

    #[test]
    fn a_token_longer_than_the_width_is_split_rather_than_dropped() {
        // No space to break at, so there is no boundary to prefer; keeping
        // every character beats dropping any of them.
        let path = "/root/companies/finalcheck-labs/.chief/agent/jordan";
        let lines = wrap_reason(path, 20, 6);
        assert!(lines.iter().all(|line| line.chars().count() <= 20), "{lines:?}");
        assert_eq!(lines.concat(), path);
    }

    #[test]
    fn a_cut_gives_up_a_whole_word_rather_than_half_of_one() {
        // The one row this card can show is `alpha bravo`, exactly 11 columns
        // wide, so the mark has to displace something. It displaces the whole
        // of `bravo` — never `alpha brav…`, and never a path mid-segment.
        assert_eq!(wrap_reason("alpha bravo charlie", 11, 1), vec!["alpha\u{2026}".to_owned()]);
        // With no boundary to give up there is nothing to prefer, so the mark
        // takes the last column and every earlier one is kept.
        assert_eq!(wrap_reason("alphabravocharlie", 11, 1), vec!["alphabravo\u{2026}".to_owned()]);
    }

    #[test]
    fn a_card_with_no_room_draws_no_reason_rather_than_a_bare_mark() {
        assert!(wrap_reason(LONG_REASON, 0, 6).is_empty());
        assert!(wrap_reason(LONG_REASON, 68, 0).is_empty());
    }

    /// **A CARD FOR SOMEBODY THE LAUNCH GATE HAS REFUSED OFFERS NO WAKE.**
    ///
    /// The operator watched `◓ Waking up…` for five minutes and nineteen
    /// seconds about a person the gate had already declined, while the rail
    /// row beside it read `refused`. A button is a promise, and this card has
    /// nothing to promise: pressing it would ask chiefd for the launch its own
    /// gate re-derives a refusal for on every pass.
    #[test]
    fn a_gate_refused_card_shows_the_reason_and_offers_no_button() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let card = blocked_card("required files 'settings.json' and 'agent.md' are missing");
        let model = model_label(&card.model);
        terminal
            .draw(|frame| {
                draw(frame, &card, &model, State::Blocked, true);
            })
            .expect("draw");
        let text: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(
            text.contains("required files 'settings.json' and 'agent.md' are missing"),
            "the gate's own sentence is the whole of what this card says: {text:?}"
        );
        assert!(text.contains(CANNOT_START), "and it says so where the button was: {text:?}");
        assert!(!text.contains("Wake Up"), "a refused person is not wakeable: {text:?}");
        assert!(
            !SPINNER.iter().any(|glyph| text.contains(glyph)),
            "and nothing spins about an outcome that is not coming: {text:?}"
        );
    }

    /// THE STATE A BLOCKED CARD STARTS IN, decided by what the brain handed it
    /// rather than by anything the card can reach. `run` cannot be driven in a
    /// unit test — it needs a pty — so the rule it applies is stated here.
    #[test]
    fn the_gates_sentence_is_what_makes_a_card_blocked() {
        assert_eq!(initial_state(&blocked_card("no home on disk")), State::Blocked);
        assert_eq!(initial_state(&fixture_card(None)), State::Sleeping);
        // A WAKE REFUSAL IS NOT A GATE REFUSAL. It is one older attempt's
        // answer, and asking again is a real repair for it, so the button
        // stays.
        assert_eq!(initial_state(&fixture_card(Some(WAKE_REJECTED))), State::Sleeping);
    }

    /// Nothing animates a state that cannot advance.
    #[test]
    fn a_blocked_card_never_animates_and_never_becomes_a_button() {
        let mut state = State::Blocked;
        for _ in 0..8 {
            state = next_animation(state);
            assert_eq!(state, State::Blocked);
        }
    }

    #[test]
    fn a_failed_screen_setup_emits_full_terminal_rollback() {
        #[derive(Default)]
        struct FailOnce {
            failed: bool,
            bytes: Vec<u8>,
        }
        impl std::io::Write for FailOnce {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.failed {
                    self.failed = true;
                    return Err(std::io::Error::other("injected setup failure"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = FailOnce::default();
        assert!(enter_screen(&mut output).is_err());
        let rollback = String::from_utf8_lossy(&output.bytes);
        assert!(rollback.contains("?1000l"), "mouse capture is disabled: {rollback:?}");
        assert!(rollback.contains("?1049l"), "alternate screen is left: {rollback:?}");
    }

    #[test]
    fn light_and_dark_cards_use_their_exact_surface_tokens() {
        for light in [true, false] {
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).expect("terminal");
            let mut button = Rect::default();
            terminal
                .draw(|frame| {
                    let card = fixture_card(None);
                    let model = model_label(&card.model);
                    button = draw(frame, &card, &model, State::Sleeping, light);
                })
                .expect("draw");
            let colors = palette(light);
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, colors.ground);
            assert_eq!(buffer[(button.x + 1, button.y + 1)].bg, colors.button);
            assert_eq!(buffer[(button.x + 1, button.y + 1)].fg, colors.button_text);
        }
    }

    #[test]
    fn waking_frame_replaces_the_button_copy_immediately() {
        let text = rendered(true, State::Waking { frame: 2 });
        assert!(text.contains("Waking up…"));
        assert!(!text.contains("Wake Up"));
    }

    fn channel(value: u8) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Color) -> f64 {
        let Color::Rgb(red, green, blue) = color else {
            panic!("card colors must be explicit RGB tokens, got {color:?}");
        };
        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }

    fn contrast(foreground: Color, background: Color) -> f64 {
        let (lighter, darker) = {
            let foreground = luminance(foreground);
            let background = luminance(background);
            if foreground >= background {
                (foreground, background)
            } else {
                (background, foreground)
            }
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn every_card_text_border_button_and_spinner_pair_meets_wcag_aa() {
        for light in [true, false] {
            let colors = palette(light);
            for (name, foreground, background) in [
                ("name/model/waking spinner", colors.text, colors.panel),
                ("role/model label/panel border", colors.muted, colors.panel),
                ("wake button", colors.button_text, colors.button),
            ] {
                let ratio = contrast(foreground, background);
                assert!(ratio >= 4.5, "{name} contrast is {ratio:.2}:1");
            }
            for color in [
                colors.ground,
                colors.panel,
                colors.text,
                colors.muted,
                colors.button,
                colors.button_text,
            ] {
                assert!(matches!(color, Color::Rgb(..)), "no Reset or named color: {color:?}");
            }
        }
    }

    #[test]
    fn a_typed_rejection_returns_immediately_instead_of_waiting_for_timeout() {
        let dir = tempfile::tempdir().expect("temporary socket directory");
        let socket = dir.path().join("rail.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("listen");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("card connection");
            let mut request = [0_u8; 1024];
            assert!(stream.read(&mut request).expect("request") > 0);
            stream
                .write_all(&ToClient::WakeRejected { person: "ada".to_owned() }.encode())
                .expect("typed rejection");
        });
        let started = Instant::now();
        assert!(!activate(&socket, "%8", "ada").expect("valid rejection"));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().expect("server");
    }

    #[test]
    fn a_true_wake_rejection_stays_visible_and_actionable() {
        let mut card = fixture_card(None);
        let mut state = State::Sleeping;
        assert!(!apply_activation(&mut card, &mut state, false));
        assert_eq!(state, State::Sleeping);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let model = model_label(&card.model);
        terminal
            .draw(|frame| {
                draw(frame, &card, &model, state, true);
            })
            .expect("draw");
        let text: String =
            terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains(WAKE_REJECTED));
        assert!(text.contains("Wake Up"));
    }
}
