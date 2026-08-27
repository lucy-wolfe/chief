//! **Click, to pixels.** One gesture, timed against tmux's own grid.
//!
//! # What "visible" means here, and why it is the honest answer
//!
//! An attached terminal is sent exactly one thing: the cell grid tmux holds for
//! the session's ACTIVE window. `capture-pane -p` returns that grid, cell for
//! cell. So this harness's clock stops when the grid the operator is looking at
//! is no longer the grid they were looking at when they clicked — and no
//! earlier.
//!
//! Everything that was previously mistaken for "visible" is excluded by
//! construction:
//!
//! * **A command issued is not visible.** `select-window` returning tells you
//!   tmux accepted an instruction, not that the grid changed. This harness never
//!   looks at a command's result.
//! * **A window that is not on the glass is not visible.** Only panes of the
//!   ACTIVE window are captured, so a person's pane spawned into a background
//!   window contributes nothing until the window is selected.
//! * **A pane that has printed nothing is not visible.** A loading pane exists
//!   the instant `split-window` returns and shows an empty rectangle until its
//!   script's first write lands. [`Glass::satisfies`] requires painted cells,
//!   which is the difference between "the UI was created" and "the UI is there".
//!
//! The residual error is the sampling interval, and it is REPORTED rather than
//! hidden: every measurement carries how many samples it took.
//!
//! # Why it can be pointed at a real company
//!
//! Nothing in here knows anything about a company except its tmux session. It
//! drives whatever rail is on the glass, in whatever session it is given — a
//! throwaway fixture, or the operator's own 97MB company with its real daemon,
//! its real converge cycle and its real accumulated history. That is the whole
//! answer to the fixture defect: the harness cannot be run against an empty
//! company by accident, because the company is an argument and its weight is
//! printed on the report ([`super::Weight`]).
//!
//! # How a click is delivered
//!
//! `send-keys -H` with the SGR mouse bytes the rail's own terminal driver
//! decodes: `ESC [ < 0 ; col ; row M` to press and `… m` to release. tmux writes
//! them to the pane's stdin verbatim, which is byte-for-byte what tmux itself
//! forwards when a real pointer is clicked over that cell — crossterm cannot
//! tell the two apart, and neither can the rail.

use crate::actuate::host::{Socket, TmuxCmd};
use crate::actuate::runner::TmuxRunner;
use crate::actuate::trust::tags;

/// How long the harness keeps sampling at full rate after a click.
///
/// The budget the plan sets is 50ms, so the first fifth of a second is where
/// precision is worth paying for. A gesture still unanswered after this is not
/// going to be graded on a millisecond, and sampling it flat out for a minute
/// would put more load on tmux than the gesture itself does — which would make
/// the harness a cause of the number it is reporting.
const FINE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// The pause between samples inside [`FINE_WINDOW`].
const FINE_POLL: std::time::Duration = std::time::Duration::from_micros(500);

/// The pause between samples after it.
const COARSE_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// One pane of the glass, as tmux holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFrame {
    /// The tmux pane id (`%12`).
    pub id: String,
    /// `@organization_person_id`, when this pane belongs to somebody.
    pub person: Option<String>,
    /// Whether this pane is the rail.
    pub rail: bool,
    /// The pane's visible cells, exactly as tmux would blit them.
    pub text: String,
}

impl PaneFrame {
    /// Has anything been drawn in this pane?
    ///
    /// THE LINE BETWEEN "created" AND "visible". A pane exists the moment
    /// `split-window` returns and holds an empty rectangle until the program
    /// inside it writes. Counting that rectangle as the operator's answer is
    /// the same error as counting `sidebar.window.laid` as a frame.
    #[must_use]
    pub fn painted(&self) -> bool {
        !self.text.trim().is_empty()
    }
}

/// Everything an attached terminal would be showing, at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glass {
    /// The active window's tmux id (`@3`).
    pub window: String,
    /// The active window's `@organization_window_id` — the DEPARTMENT it is
    /// showing, or `__focus__` for the person window.
    pub logical: Option<String>,
    /// Every live pane of the active window, and nothing else. A pane in a
    /// background window is not on the glass.
    pub panes: Vec<PaneFrame>,
}

/// What a click is supposed to put on the glass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A department's own window, showing something.
    Department(String),
    /// This person's pane, painted, on the glass.
    Person(String),
}

impl Glass {
    /// Does this frame answer the gesture?
    ///
    /// Every arm requires PAINTED cells, never mere existence — see
    /// [`PaneFrame::painted`].
    #[must_use]
    pub fn satisfies(&self, target: &Target) -> bool {
        match target {
            // A department window whose panes are still blank is a window that
            // has been selected and has nothing on it. The rail alone does not
            // count: it is in every window, so it would make every gesture
            // instantly "visible".
            Target::Department(id) => {
                self.logical.as_deref() == Some(id.as_str())
                    && self.panes.iter().any(|pane| !pane.rail && pane.painted())
            }
            Target::Person(id) => self
                .panes
                .iter()
                .any(|pane| pane.person.as_deref() == Some(id.as_str()) && pane.painted()),
        }
    }

    /// The rail pane of this window, which is where a click is delivered.
    #[must_use]
    pub fn rail(&self) -> Option<&PaneFrame> {
        self.panes.iter().find(|pane| pane.rail)
    }
}

/// Why a measurement could not be taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BenchError {
    /// tmux would not answer at all.
    #[error("chief bench click: tmux would not answer ({0}). Is the company attached?")]
    Tmux(String),
    /// The session has no rail on the glass, so there is nothing to click.
    #[error(
        "chief bench click: the active window of '{session}' has no sidebar pane, so there is \
         no rail to click. `chief attach` opens the company with one."
    )]
    NoRail {
        /// The session that was asked.
        session: String,
    },
    /// The rail is not drawing the row the caller named.
    #[error(
        "chief bench click: no row of the rail contains '{wanted}'. What it is drawing right \
         now is:\n{drawn}"
    )]
    NoSuchRow {
        /// The text that was searched for.
        wanted: String,
        /// The rail, as the operator would see it.
        drawn: String,
    },
}

/// One click, measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    /// Click to the first instant the glass differed at all, in microseconds.
    /// `None` when nothing on the glass ever changed.
    pub first_change_us: Option<u64>,
    /// Click to the first instant the glass was CORRECT for the gesture.
    /// `None` when it never was, inside the budget.
    pub visible_us: Option<u64>,
    /// How many times the glass was read. The sampling error is bounded by the
    /// gap between two of these, so the count is part of the result rather than
    /// a detail of how it was obtained.
    pub samples: u64,
}

/// Read the whole glass: the active window, and every live pane in it.
///
/// One `list-panes` plus one `capture-pane` per pane of the active window —
/// typically two to six calls, at 0.086ms each over the persistent control
/// client.
///
/// # Errors
/// [`BenchError::Tmux`] when tmux cannot be reached or refuses the read.
pub fn read_glass(
    tmux: &dyn TmuxRunner,
    socket: &Socket,
    session: &str,
) -> Result<Glass, BenchError> {
    let format = format!(
        "#{{window_active}}\t#{{pane_id}}\t#{{window_id}}\t#{{pane_dead}}\t#{{{window}}}\t\
         #{{{person}}}\t#{{{rail}}}",
        window = tags::WINDOW,
        person = tags::PERSON,
        rail = tags::SIDEBAR,
    );
    let listed = run(tmux, socket, &["list-panes", "-s", "-t", session, "-F", &format])?;

    let mut window = String::new();
    let mut logical = None;
    let mut panes = Vec::new();
    for line in listed.lines() {
        let mut fields = line.split('\t');
        let mut next = || fields.next().unwrap_or_default().trim().to_owned();
        let (active, id, window_id, dead) = (next(), next(), next(), next());
        let (logical_id, person, rail) = (next(), next(), next());
        if active != "1" || dead == "1" {
            continue;
        }
        window = window_id;
        logical = (!logical_id.is_empty()).then_some(logical_id);
        // A PANE THAT VANISHED BETWEEN THE LIST AND THE CAPTURE IS NOT ON THE
        // GLASS. It is not an error either, and treating it as one is a
        // measurement that fails precisely when it matters: a gesture's whole
        // job is to churn panes — a loading panel is killed the instant its
        // person arrives — so this race fires during exactly the clicks worth
        // timing. It aborted a real cold-wake measurement before it was fixed.
        //
        // The `list-panes` above is NOT forgiven the same way: without it there
        // is no glass at all, and guessing an empty one would score every
        // gesture as instantly answered.
        let Ok(text) = run(tmux, socket, &["capture-pane", "-p", "-t", &id]) else {
            continue;
        };
        panes.push(PaneFrame {
            id,
            person: (!person.is_empty()).then_some(person),
            rail: !rail.is_empty() && rail != "0",
            text,
        });
    }
    Ok(Glass { window, logical, panes })
}

/// The row of the rail whose text contains `wanted`, 0-based from the top.
///
/// Matched against WHAT IS DRAWN, never against a roster: the harness clicks the
/// row the operator can see, which is the only thing a click can be about. A
/// name that is truncated on the glass is a name the operator cannot click by
/// that spelling either.
///
/// # Errors
/// [`BenchError::NoSuchRow`], carrying the rail as drawn, so a caller who named
/// a row that is scrolled out of view can see what IS there.
pub fn row_of(rail: &PaneFrame, wanted: &str) -> Result<usize, BenchError> {
    rail.text.lines().position(|line| line.contains(wanted)).ok_or_else(|| BenchError::NoSuchRow {
        wanted: wanted.to_owned(),
        drawn: rail.text.trim_end().to_owned(),
    })
}

/// The bytes a left click on `row` puts into a pane's stdin.
///
/// SGR extended mouse mode (`?1006`), which is what `EnableMouseCapture` asks
/// for and what tmux forwards for a real pointer. Press then release, because
/// crossterm reports `Down` on the press and the rail acts on `Down` — a press
/// with no release would leave the terminal believing a button is still held.
///
/// The wire coordinates are 1-BASED and crossterm subtracts one, so a click
/// meant for pane row `row` is sent as `row + 1`. The column is irrelevant to
/// the rail — `sidebar::click` resolves a row and never a column — so it is a
/// fixed 2, inside any rail wide enough to be readable.
#[must_use]
pub fn sgr_click(row: usize) -> Vec<String> {
    let press = format!("\x1b[<0;2;{}M", row + 1);
    let release = format!("\x1b[<0;2;{}m", row + 1);
    format!("{press}{release}").bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// Deliver one click into `pane` at `row`.
///
/// # Errors
/// [`BenchError::Tmux`] when tmux refuses the injection.
pub fn send_click(
    tmux: &dyn TmuxRunner,
    socket: &Socket,
    pane: &str,
    row: usize,
) -> Result<(), BenchError> {
    let hex = sgr_click(row);
    let mut argv = vec!["send-keys", "-t", pane, "-H"];
    argv.extend(hex.iter().map(String::as_str));
    run(tmux, socket, &argv).map(|_| ())
}

/// Click `row` of the rail and time the glass until it answers `target`.
///
/// The clock starts the instant before the click bytes are handed to tmux and
/// stops on a READ OF THE GRID, never on a command's return.
///
/// # Errors
/// [`BenchError`] when the glass cannot be read or the click cannot be
/// delivered. A gesture that simply never becomes visible is NOT an error — it
/// is a [`Measurement`] with no `visible_us`, which is a result and must be
/// reported as one.
pub fn measure(
    tmux: &dyn TmuxRunner,
    socket: &Socket,
    session: &str,
    row: usize,
    target: &Target,
    budget: std::time::Duration,
) -> Result<Measurement, BenchError> {
    let before = read_glass(tmux, socket, session)?;
    let rail = before.rail().ok_or_else(|| BenchError::NoRail { session: session.to_owned() })?;
    let rail_pane = rail.id.clone();

    let started = std::time::Instant::now();
    send_click(tmux, socket, &rail_pane, row)?;

    let mut measurement = Measurement { first_change_us: None, visible_us: None, samples: 0 };
    while started.elapsed() < budget {
        let now = read_glass(tmux, socket, session)?;
        let elapsed = chiefd_log::elapsed_us(started);
        measurement.samples += 1;
        if measurement.first_change_us.is_none() && now != before {
            measurement.first_change_us = Some(elapsed);
        }
        if now.satisfies(target) {
            measurement.visible_us = Some(elapsed);
            return Ok(measurement);
        }
        // THE ONLY WAIT IN THIS FILE, and it is the sampling interval of a
        // measurement instrument rather than a poll of a state machine — the
        // class `clippy.toml` names as the legitimate exception. There is no
        // event to subscribe to: tmux's control mode is attached with
        // `no-output` (`control::CLIENT_FLAGS`), so nothing pushes "the grid
        // changed", and asking is the only way to know.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(if started.elapsed() < FINE_WINDOW { FINE_POLL } else { COARSE_POLL });
    }
    Ok(measurement)
}

/// Run one tmux command, or report why it could not be run.
fn run(tmux: &dyn TmuxRunner, socket: &Socket, argv: &[&str]) -> Result<String, BenchError> {
    let cmd = TmuxCmd { argv: argv.iter().map(|arg| (*arg).to_owned()).collect() };
    let out = tmux.run(socket, &cmd).map_err(|error| BenchError::Tmux(error.to_string()))?;
    if out.status == 0 {
        Ok(out.stdout)
    } else {
        Err(BenchError::Tmux(format!("`{}` exited {}: {}", argv.join(" "), out.status, out.stderr)))
    }
}

#[cfg(test)]
mod tests {
    use super::{read_glass, row_of, sgr_click, BenchError, Glass, PaneFrame, Target};
    use crate::actuate::host::{HostErr, Socket, TmuxCmd, TmuxOut};
    use crate::actuate::runner::TmuxRunner;
    use std::sync::Mutex;

    /// A tmux that answers by what was asked, and can be told to refuse one
    /// exact command — which is the only way to reproduce a pane dying between
    /// the list and the capture.
    struct ScriptedRunner {
        answers: Vec<(&'static str, &'static str)>,
        refuse: Option<&'static str>,
        asked: Mutex<Vec<String>>,
    }

    impl TmuxRunner for ScriptedRunner {
        fn run(&self, _socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
            let asked = cmd.argv.join(" ");
            self.asked.lock().expect("not poisoned").push(asked.clone());
            if self.refuse.is_some_and(|refused| asked.contains(refused)) {
                return Ok(TmuxOut {
                    status: 1,
                    stdout: String::new(),
                    stderr: "can't find pane".to_owned(),
                });
            }
            let stdout = self
                .answers
                .iter()
                .find(|(pattern, _)| asked.contains(pattern))
                .map_or_else(String::new, |(_, reply)| (*reply).to_owned());
            Ok(TmuxOut { status: 0, stdout, stderr: String::new() })
        }
    }

    /// Two panes in the ACTIVE window and one in a background window, in the
    /// field order `read_glass` asks for.
    fn listing() -> &'static str {
        concat!(
            "1\t%1\t@3\t0\tengineering\t\t\t1\n",
            "1\t%4\t@3\t0\tengineering\tdev\t\t\n",
            "0\t%9\t@2\t0\texecutive\tceo\t\t\n",
        )
    }

    fn runner(refuse: Option<&'static str>) -> ScriptedRunner {
        ScriptedRunner {
            answers: vec![
                ("list-panes", listing()),
                ("capture-pane -p -t %1", "Departments\n  engineering"),
                ("capture-pane -p -t %4", "$ dev is working"),
            ],
            refuse,
            asked: Mutex::new(Vec::new()),
        }
    }

    /// ONLY THE ACTIVE WINDOW IS THE GLASS. A person spawned into a background
    /// window has not been shown to anybody, and counting them would score a
    /// gesture as answered while the operator is still looking at something
    /// else.
    #[test]
    fn only_the_active_windows_panes_are_read() {
        let tmux = runner(None);
        let glass = read_glass(&tmux, &Socket("s".to_owned()), "org-acme_").expect("a glass");

        assert_eq!(glass.window, "@3");
        assert_eq!(glass.logical.as_deref(), Some("engineering"));
        assert_eq!(glass.panes.len(), 2, "the background window's pane is not on the glass");
        assert!(glass.panes.iter().all(|pane| pane.id != "%9"));
        assert!(
            !tmux.asked.lock().expect("not poisoned").iter().any(|ask| ask.contains("-t %9")),
            "a pane that is not on the glass must not even be captured"
        );
    }

    /// A PANE THAT DIED MID-SAMPLE IS NOT AN ERROR, it is a pane that is no
    /// longer on the glass. The race fires during exactly the gestures worth
    /// timing — a loading panel is killed the instant its person arrives — and
    /// it aborted a real cold-wake measurement before this rule existed.
    #[test]
    fn a_pane_that_vanishes_between_the_list_and_the_capture_is_skipped() {
        let tmux = runner(Some("capture-pane -p -t %4"));
        let glass = read_glass(&tmux, &Socket("s".to_owned()), "org-acme_")
            .expect("one dead pane must not fail the whole read");

        assert_eq!(glass.panes.len(), 1, "the survivor is still read");
        assert_eq!(glass.panes[0].id, "%1");
    }

    /// The listing itself is NOT forgiven. Without it there is no glass, and an
    /// empty one would score every gesture as instantly answered.
    #[test]
    fn a_listing_that_fails_is_a_refusal_and_never_an_empty_glass() {
        let tmux = runner(Some("list-panes"));
        let refusal = read_glass(&tmux, &Socket("s".to_owned()), "org-acme_")
            .expect_err("no listing, no glass");
        assert!(matches!(refusal, BenchError::Tmux(_)), "{refusal:?}");
    }

    fn pane(id: &str, text: &str) -> PaneFrame {
        PaneFrame { id: id.to_owned(), person: None, rail: false, text: text.to_owned() }
    }

    fn glass(logical: &str, panes: Vec<PaneFrame>) -> Glass {
        Glass { window: "@1".to_owned(), logical: Some(logical.to_owned()), panes }
    }

    /// The same rule for a live person: their pane can be created, sized and
    /// selected while the program inside it has drawn nothing at all.
    #[test]
    fn a_persons_pane_counts_only_once_it_has_drawn_something() {
        let blank = PaneFrame { person: Some("dev".to_owned()), ..pane("%4", "") };
        let painted = PaneFrame { person: Some("dev".to_owned()), ..pane("%4", "$ ") };

        assert!(!glass("engineering", vec![blank]).satisfies(&Target::Person("dev".to_owned())));
        assert!(glass("engineering", vec![painted]).satisfies(&Target::Person("dev".to_owned())));
    }

    /// A department is answered by ITS OWN window with content in it. The
    /// window's logical id must match, so a click that landed on a different
    /// department is not scored as a fast one.
    #[test]
    fn a_department_is_visible_only_when_its_own_window_is_the_one_on_the_glass() {
        let people = vec![PaneFrame { person: Some("dev".to_owned()), ..pane("%4", "$ ") }];
        assert!(glass("engineering", people.clone())
            .satisfies(&Target::Department("engineering".to_owned())));
        assert!(
            !glass("executive", people).satisfies(&Target::Department("engineering".to_owned())),
            "another department's window is not the department that was clicked"
        );
    }

    /// THE RAIL IS IN EVERY WINDOW, so a department whose only painted pane is
    /// the rail is a window with nothing on it — the "sidebar full screen,
    /// right side blank" picture. Counting the rail would make every gesture
    /// instantly visible and the whole measurement meaningless.
    #[test]
    fn the_rail_alone_is_not_a_visible_department() {
        let rail = PaneFrame { rail: true, ..pane("%1", "Departments\n> engineering") };
        assert!(!glass("engineering", vec![rail])
            .satisfies(&Target::Department("engineering".to_owned())));
    }

    /// SGR extended mouse, press then release, 1-based on the wire. Pinned
    /// because a click delivered at the wrong row measures a different gesture
    /// than the one being reported, and nothing downstream could tell.
    #[test]
    fn a_click_is_the_sgr_bytes_a_real_pointer_would_produce() {
        let bytes = sgr_click(4);
        let rendered: String =
            bytes.iter().map(|hex| char::from(u8::from_str_radix(hex, 16).expect("hex"))).collect();
        assert_eq!(
            rendered, "\u{1b}[<0;2;5M\u{1b}[<0;2;5m",
            "pane row 4 is wire row 5, pressed and released"
        );
    }

    /// The harness clicks the row the OPERATOR sees, found in the rail's own
    /// drawn cells — never a row computed from a roster the glass may not be
    /// showing.
    #[test]
    fn a_row_is_found_by_what_the_rail_has_drawn() {
        let rail = PaneFrame {
            rail: true,
            ..pane("%1", "Departments\n  executive\n> engineering\n---\nPeople\n  dev")
        };
        assert_eq!(row_of(&rail, "engineering"), Ok(2));
        assert_eq!(row_of(&rail, "dev"), Ok(5));

        let missing = row_of(&rail, "quant").expect_err("a row nobody is drawing");
        let message = missing.to_string();
        assert!(message.contains("quant"), "{message}");
        assert!(
            message.contains("engineering"),
            "the refusal must show what IS drawn, or the operator cannot correct it: {message}"
        );
    }

    /// A frame that changed but is not yet CORRECT is a first-change, not a
    /// visible. The two are reported separately because the gap between them
    /// is the flicker the operator complains about.
    #[test]
    fn a_changed_frame_is_not_the_same_fact_as_a_correct_one() {
        let before = glass("executive", vec![pane("%2", "chief")]);
        let changed = glass("executive", vec![pane("%2", "ceo is thinking")]);
        assert_ne!(before, changed, "the grid moved");
        assert!(
            !changed.satisfies(&Target::Department("engineering".to_owned())),
            "a grid that moved is not a grid that answered"
        );
    }
}
