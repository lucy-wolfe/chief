//! `chief sidebar <company>` — the THIN CLIENT that lives in a rail pane.
//!
//! # What it is, stated as an absence
//!
//! It holds no company, no roster, no desired set, no selection, no scroll
//! offset, no accent memo and no placement copy. It makes no HTTP request. It
//! has no chiefd client to build, no bearer to read off disk and no changefeed
//! to park on. It cannot be wrong about the company, because it has never been
//! told one.
//!
//! What it owns is a terminal: raw mode, the alternate screen, mouse reporting,
//! and one pty. Everything it does is
//!
//! ```text
//!   stdin bytes  ->  the session socket
//!   frames       ->  stdout, verbatim
//!   SIGWINCH     ->  one Resize message
//! ```
//!
//! That is herdr's client (`src/client/mod.rs:1572` forwards raw input; the
//! server blits cell frames back), and the reason it is worth copying is the
//! boot: **a thin client has no state to load, so its first frame is a PUSH.**
//! A freshly minted window's rail paints in one socket round trip. The rail it
//! replaces spent that moment on discovery, a beacond health wait, an
//! authenticated company round trip and a key read off disk — measured at a
//! median 11ms and a tail of **804ms**, every millisecond of it a blank pane on
//! the operator's glass.
//!
//! # `sidebar.frame.painted` is written HERE, and it has to be
//!
//! The brain composes the frame, but only this process can say the bytes
//! reached a pty. The event therefore fires after the write and the flush, and
//! it names the gesture the frame answers — which the frame carries, because
//! the brain↔client socket is where `gesture_id` crosses processes now.
//! `elapsed_us` needs no second field on the wire: a gesture id IS the click's
//! wall clock in microseconds ([`super::gesture`]).

use std::io::Write as _;
use std::path::Path;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use super::wire::{Frames, ToBrain, ToClient, PROTOCOL};

/// How long the client waits between attempts to reach its session's brain.
///
/// It is a BOOT RACE and not a fallback: `attach` starts the actuator and mints
/// rail panes, and a rail can be running before the brain has bound its socket.
/// A client that gave up would leave a window with no sidebar until something
/// re-minted it.
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

/// How long the rail holds its last true frame before it says the brain is
/// gone.
///
/// #1207. The #1204 exclusion — hold the last frame, never flicker — was right
/// FOR A RECONNECT, and it does not survive two hours. On 2026-08-23 every rail
/// in a company held a frame that still looked alive while the actuator lay
/// dead, and the operator found out by dragging a border.
///
/// The threshold is set above anything a supervised restart can take: the
/// crash-loop ceiling is ten seconds and a boot is about one, so twenty seconds
/// of consecutive failed connects is not a restart in progress — it is a fault.
/// Below it nothing changes at all.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// How often the stale line's duration is rewritten. Slow on purpose: a number
/// that ticks every 150 ms is a distraction, and this one is read once.
const STALE_REFRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// The one line the rail writes over a frozen frame.
///
/// It says NOTHING about the company, which is what keeps it inside this file's
/// standing rule against a rail having a second answer about anything: it
/// states the one fact only the transport knows — this rail cannot reach the
/// brain and has not for a while — exactly as the boot line already does. And
/// it names the remedy, because an operator reading it is one command away.
#[must_use]
pub fn stale_line(gone: std::time::Duration) -> String {
    format!(
        "chief: the session brain has been gone {} — the actuator is not running; \
         run `chief` here to restart it",
        crate::actuate::crash_loop::human_duration(gone)
    )
}

/// Whether to write the stale line now.
///
/// Pure so the threshold and the refresh can be pinned without a socket, a
/// clock or a terminal.
#[must_use]
pub fn should_write_stale(
    failing_for: std::time::Duration,
    since_last_write: Option<std::time::Duration>,
) -> bool {
    if failing_for < STALE_AFTER {
        return false;
    }
    match since_last_write {
        None => true,
        Some(elapsed) => elapsed >= STALE_REFRESH,
    }
}

/// What the operator is shown while there is no brain to draw them one.
///
/// Deliberately plain text and not a rendered rail: the renderer lives in the
/// brain, and a client that drew its own frame would be a second answer about
/// the company — which is the whole class of defect this stage deletes. It says
/// what is true and nothing more.
const WAITING: &str = "chief: waiting for this company's session brain…";

/// The rail's terminal, held for the life of the process and given back on the
/// way out however the client leaves.
///
/// # Why the terminal is taken BEFORE anything else
///
/// A rail pane is minted blank by `split-window`. Nothing paints it until the
/// first frame, and until then it is a white rectangle in canonical mode with
/// echo on — which is exactly what the operator photographed. The verb takes
/// the glass as its FIRST act, so the pane reads as a rail rather than as a
/// hole in the window from the first millisecond.
///
/// # Why a guard rather than a matching call
///
/// A pane left in raw mode on the alternate screen with mouse reporting on is
/// unusable, and every early return out of the verb is a return that would
/// leave it that way. [`Drop`] covers all of them, and a panic besides.
pub struct Glass;

impl Glass {
    /// Take the terminal — raw mode, the alternate screen, the mouse — and put
    /// one honest line on it.
    ///
    /// `EnableMouseCapture` emits `?1000h ?1002h ?1003h ?1006h`. That full set
    /// is deliberate and not defensive: tmux's `root` table forwards a LEFT
    /// CLICK unconditionally, but forwards the wheel and the drag only when
    /// `#{||:#{pane_in_mode},#{mouse_any_flag}}` holds, so a rail that asked
    /// for no mouse reporting would take clicks and silently lose every wheel
    /// event to copy-mode. `?1006h` is SGR extended reporting, which is the
    /// grammar [`super::input`] decodes.
    ///
    /// # Errors
    /// Any terminal error taking the terminal. The terminal is given back
    /// before returning, because a verb that could not take the glass must not
    /// leave it half-taken.
    pub fn take() -> std::io::Result<Self> {
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        if let Err(error) = execute!(out, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let glass = Self;
        let _ = write!(out, "\x1b[H\x1b[2J{WAITING}");
        let _ = out.flush();
        Ok(glass)
    }
}

impl Drop for Glass {
    fn drop(&mut self) {
        // Nothing useful can be done with a failure here, and unwinding out of
        // a `Drop` would abort. Each step is attempted whatever the one before
        // it did: leaving raw mode on because the alternate screen refused to
        // go back would be the worst of the possible outcomes.
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

/// Blit one frame, inside synchronized-update brackets.
///
/// # The tearing this ends
///
/// A frame changes the selection marker, four rows of names and a section
/// title, and a terminal is free to paint each cell the instant it arrives — so
/// one frame can be up to three visibly distinct pictures. `?2026h`/`?2026l` is
/// the DEC private-mode pair that makes a supporting terminal buffer the whole
/// frame and present it in one go; a terminal that does not support it ignores
/// both, so this can only help and needs no capability probe.
///
/// **The end marker is emitted whatever the write did**, and that is enforced
/// by a [`Drop`] guard rather than by the absence of an early return. Mode 2026
/// is LATCHED: a terminal told to hold its screen back and never told to stop
/// shows the operator a frozen rail for the life of the pane, which is far
/// worse than the tearing this removes.
///
/// Nothing here ERASES. The brain composes whole frames — every cell of every
/// frame is written — so the screen passes through no blank state at all, which
/// matters because the operator's tmux 3.3a honours no synchronized-update
/// marker and an `ED2` would reach the glass alone.
fn blit(bytes: &[u8]) {
    /// Ends the synchronized update when it goes out of scope, however it does.
    struct Synchronized;

    impl Drop for Synchronized {
        fn drop(&mut self) {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[?2026l");
            let _ = out.flush();
        }
    }

    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?2026h");
    let _synchronized = Synchronized;
    let _ = out.write_all(bytes);
}

/// Why the client could not run at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// This process is not in a tmux pane, so it has no pane to name.
    #[error(
        "the sidebar draws inside a tmux pane and this process is not in one. \
         `chief attach <company>` opens the company, and the rail comes with it."
    )]
    NotInAPane,
    /// The brain accepted this client and dropped it without a single frame,
    /// which is what a brain does to a rail it will not speak to.
    #[error(
        "this company's session brain refused this rail — most likely because \
         `chief` was upgraded under a live session and this pane is still \
         running the old binary. Reattach the company to replace it."
    )]
    Refused,
}

/// Run the rail in this pane until the pane closes.
///
/// The whole verb: connect, say hello, forward, blit. A brain that goes away —
/// the actuator restarting — is a RECONNECT rather than an exit, because the
/// pane must not die under the operator.
///
/// # Errors
/// [`ClientError::NotInAPane`] when `TMUX_PANE` is unset, and
/// [`ClientError::Refused`] when the brain will not speak to this build.
pub async fn run(socket: &Path) -> Result<(), ClientError> {
    // The client's own pane. tmux tells a program which pane it is in through
    // `TMUX_PANE`, and that is the only honest answer: a pane id derived any
    // other way would be this process guessing at its own placement.
    let pane = std::env::var("TMUX_PANE").map_err(|_| ClientError::NotInAPane)?;
    // STDIN ON ITS OWN THREAD. A blocking read is how bytes come off a pty, and
    // it is the right shape besides: the thread does nothing but move bytes,
    // and the process exits out from under it when the pane closes.
    let (keys, mut typed) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut stdin = std::io::stdin();
        let mut buffer = [0_u8; 4096];
        while let Ok(count) = stdin.read(&mut buffer) {
            if count == 0 || keys.send(buffer.get(..count).unwrap_or_default().to_vec()).is_err() {
                return;
            }
        }
    });
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .ok()
        .map(Box::new);
    // #1207: how long consecutive failed connects have been running, and when
    // the stale line was last written. Both are plain `Instant`s, the same
    // category as `RECONNECT_DELAY` — a transport ladder, not a reconcile
    // cadence.
    let mut failing_since: Option<std::time::Instant> = None;
    let mut last_written: Option<std::time::Instant> = None;
    loop {
        let mut painted_any = false;
        let outcome =
            attempt(socket, &pane, &mut typed, winch.as_deref_mut(), &mut painted_any).await;
        if painted_any {
            // The frame repaints the whole rail, so the line needs no erase.
            failing_since = None;
            last_written = None;
        }
        match outcome {
            Attempt::Ended => return Ok(()),
            Attempt::Refused => return Err(ClientError::Refused),
            Attempt::Retry => {
                let now = std::time::Instant::now();
                let since = *failing_since.get_or_insert(now);
                if should_write_stale(
                    now.saturating_duration_since(since),
                    last_written.map(|written| now.saturating_duration_since(written)),
                ) {
                    let mut out = std::io::stdout();
                    // Row 0, its own line erased first. Everything below keeps
                    // the last true frame.
                    let _ = write!(
                        out,
                        "\x1b[H\x1b[2K{}",
                        stale_line(now.saturating_duration_since(since))
                    );
                    let _ = out.flush();
                    last_written = Some(now);
                }
                // A BOOT RACE, NOT A FALLBACK. Nothing is drawn from local
                // state while we wait; the pane keeps whatever the last frame
                // put there, which is the last TRUE picture of the company.
                //
                // The one wait in this file, and it is the reconnect ladder of
                // a transport rather than a reconcile cadence: there is no
                // injected clock in a process whose whole job is one socket.
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a transport reconnect ladder in a client with no injected clock; \
                              the Clock seam governs reconcile duties, and this is neither"
                )]
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// What one connection attempt concluded.
enum Attempt {
    /// The operator closed the rail. Nothing is left to do.
    Ended,
    /// The brain is not there, or went away mid-session. Try again.
    Retry,
    /// The brain accepted and dropped this client without a frame.
    Refused,
}

/// One connection to the brain, from `Hello` to the socket closing.
async fn attempt(
    socket: &Path,
    pane: &str,
    typed: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    winch: Option<&mut tokio::signal::unix::Signal>,
    painted_any: &mut bool,
) -> Attempt {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let Ok(stream) = tokio::net::UnixStream::connect(socket).await else {
        return Attempt::Retry;
    };
    let (mut incoming, mut outgoing) = stream.into_split();
    let (columns, rows) = pane_size();
    let hello =
        ToBrain::Hello { protocol: PROTOCOL, pane: pane.to_owned(), columns, rows }.encode();
    if outgoing.write_all(&hello).await.is_err() {
        return Attempt::Retry;
    }
    tracing::info!(
        event = "sidebar.client.connected",
        pane = %pane,
        columns,
        rows,
        "this rail is a thin client of its session's brain; its first frame is a push"
    );
    let mut frames = Frames::new();
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut painted: usize = 0;
    let mut resized = winch;
    loop {
        tokio::select! {
            bytes = typed.recv() => {
                let Some(bytes) = bytes else { return Attempt::Ended };
                if outgoing.write_all(&ToBrain::Input(bytes).encode()).await.is_err() {
                    return Attempt::Retry;
                }
            }
            // SIGWINCH IS NOT IN THE BYTE STREAM, so it is the one thing this
            // client has to say in its own words.
            () = window_changed(resized.as_deref_mut()) => {
                let (columns, rows) = pane_size();
                let message = ToBrain::Resize { columns, rows }.encode();
                if outgoing.write_all(&message).await.is_err() {
                    return Attempt::Retry;
                }
            }
            read = incoming.read(&mut buffer) => {
                let Ok(count) = read else { return Attempt::Retry };
                if count == 0 {
                    // A CONNECTION THAT NEVER CARRIED A FRAME WAS REFUSED. The
                    // brain pushes one the instant a client attaches, so a
                    // close with none is the brain declining to speak to this
                    // build — and retrying that for ever would be a spin.
                    return if painted == 0 { Attempt::Refused } else { Attempt::Retry };
                }
                frames.feed(buffer.get(..count).unwrap_or_default());
                loop {
                    match frames.next_to_client() {
                        Ok(Some(ToClient::Frame { gesture, bytes })) => {
                            painted = painted.saturating_add(1);
                            // #1207: a real frame is what resets the staleness
                            // clock. Without this the rail would accumulate
                            // "failing" time across connections that succeeded,
                            // and a brain that dropped every few seconds would
                            // eventually be reported as gone while it was
                            // painting.
                            *painted_any = true;
                            paint(gesture, &bytes);
                        }
                        // A rail never asks the one question this answers, so
                        // an answer to it is not addressed to this connection.
                        // Dropped rather than blitted: writing a JSON payload
                        // into the operator's pane would be worse than silence.
                        Ok(Some(
                            ToClient::Company(_)
                            | ToClient::WakeAccepted { .. }
                            | ToClient::WakeRejected { .. },
                        )) => {}
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(
                                event = "sidebar.client.unreadable",
                                diagnostic = %error,
                                "this rail cannot frame what its brain is sending; reconnecting"
                            );
                            return Attempt::Retry;
                        }
                    }
                }
            }
        }
    }
}

/// Park until this pane is resized, or for ever when no signal could be
/// installed.
///
/// An arm that never fires is exactly what a client with no SIGWINCH handler
/// should have: the alternative is a `select!` that spins on a `None`.
async fn window_changed(signal: Option<&mut tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Write one frame to the pane and say when it landed.
///
/// **THE FRAME IS ON THE GLASS.** Not "a layout was applied", not "a wake was
/// posted" — the bytes have been written and flushed to this pane's pty and
/// tmux has the cells. That is the last instant any process can honestly claim,
/// and it is the endpoint of the plan's "click → sidebar repaint".
fn paint(gesture: Option<u64>, bytes: &[u8]) {
    blit(bytes);
    let Some(gesture) = gesture else {
        return;
    };
    // A GESTURE ID IS THE CLICK'S WALL CLOCK IN MICROSECONDS, so the elapsed
    // time is a subtraction and needs no second field on the wire. A clock that
    // stepped backwards between the two reads yields no line rather than a
    // negative one: a funnel with an impossible number in it is worse than a
    // funnel with a gap.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_micros()).ok());
    let Some(elapsed_us) = now.and_then(|now| now.checked_sub(gesture)) else {
        return;
    };
    tracing::info!(
        event = "sidebar.frame.painted",
        gesture_id = gesture,
        elapsed_us,
        bytes = bytes.len(),
        "the frame answering this gesture has been flushed to the pane"
    );
}

/// This pane's size, right now.
///
/// `crossterm::terminal::size` is an ioctl on this process's own tty, which IS
/// the pane — never a tmux round trip, and never a value somebody else told us.
/// The fallback is the smallest honest terminal rather than a remembered value:
/// a client that could not measure itself must not claim a size.
fn pane_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((80, 24))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{should_write_stale, stale_line, STALE_AFTER, STALE_REFRESH};

    /// Below the threshold the rail behaves exactly as it did: hold the last
    /// true frame, write nothing, never flicker. That is the #1204 rule and it
    /// is unchanged for every ordinary reconnect.
    #[test]
    fn a_short_reconnect_writes_nothing() {
        assert!(!should_write_stale(Duration::ZERO, None));
        assert!(!should_write_stale(Duration::from_millis(150), None));
        assert!(!should_write_stale(STALE_AFTER - Duration::from_millis(1), None));
    }

    /// Past it, once.
    #[test]
    fn a_long_reconnect_writes_one_honest_line() {
        assert!(should_write_stale(STALE_AFTER, None));
        assert!(should_write_stale(Duration::from_secs(120), None));
    }

    #[test]
    fn the_line_is_refreshed_no_faster_than_every_ten_seconds() {
        let failing = Duration::from_secs(120);
        assert!(!should_write_stale(failing, Some(Duration::ZERO)));
        assert!(!should_write_stale(failing, Some(STALE_REFRESH - Duration::from_millis(1))));
        assert!(should_write_stale(failing, Some(STALE_REFRESH)));
    }

    /// The sentence says nothing about the COMPANY — only what the transport
    /// knows — and names the remedy, because the operator reading it is one
    /// command away from fixing it.
    #[test]
    fn the_line_states_the_transport_fact_and_the_remedy() {
        let line = stale_line(Duration::from_secs(125));
        assert!(line.contains("session brain has been gone"), "{line}");
        assert!(line.contains("2m 5s"), "the duration is on the glass: {line}");
        assert!(line.contains("run `chief`"), "{line}");
        assert!(
            !line.contains("people") && !line.contains("department"),
            "a rail never gets a second answer about the company: {line}"
        );
    }
}
