//! `chiefd-log` — the daemon-level observability stream.
//!
//! # The gap this closes
//!
//! Every log chiefd had was per-company — `<dir>/.chief/logs/exceptions.jsonl`,
//! `<dir>/.chief/logs/pi-startup.jsonl`, `<dir>/.chief/bus/events.jsonl`, and before the
//! company became its own directory they hung off `orgs/<slug>/` instead. A
//! company launch spends its first minutes BEFORE any of those paths exist, so
//! the one window an operator most needs to see wrote nothing anywhere. A
//! `chiefd_launch_company` that ran 4 minutes 34 seconds behind a bare spinner
//! could not be explained from disk at all; it took SSH, `/proc`, `ss`, a pane
//! capture and `strings` on the release binary, and still did not answer where
//! the time went.
//!
//! Now every chiefd program appends to `<log-root>/<program>.jsonl` from its
//! first line of `main`, and every step that can block is a span whose exit
//! line carries a measured `detail.durationMs`. The root is one of exactly two
//! named answers — the company's own `<dir>/.chief/log/`, or `$HOME/.chief/log/`
//! for a box-wide program that belongs to no company; see [`sink`] for why
//! there are two and why a third would be a guess.
//!
//! # Why it is its own crate
//!
//! The sink lived in `chiefd_core::orglog`, and `chief-cli` — the operator
//! client, which owns exactly the minutes that were invisible — is forbidden to
//! link `chiefd-core` (the backend/client boundary guard, rules 5 and 7). A
//! leaf is the only place a definition both sides need can live, the same shape
//! `host-primitives` already has. The sink MOVED here; it
//! was not copied, because two copies of a line schema is the failure this
//! workspace keeps a guard family for.
//!
//! # Using it
//!
//! A binary calls [`install`] once, at the top of `main`. Everything after that
//! is ordinary `tracing`:
//!
//! ```no_run
//! chiefd_log::install("chiefd");
//!
//! let span = tracing::info_span!("daemon.start", company = "acme");
//! let _entered = span.enter();
//! tracing::info!(event = "daemon.spawned", pid = 4242_u64, "spawned the company daemon");
//! // The exit line carries `detail.durationMs` when `span` drops.
//! ```
//!
//! Two conventions make the file worth reading:
//!
//! * State `event = "noun.verb"` on a call site worth grepping for. Without one
//!   the message names the line, which is fine for a one-off and poor for a
//!   step somebody will search for during an incident.
//! * Wrap anything that can block in a span. A step that can take minutes and
//!   logs nothing is the exact defect this crate exists to delete.

#![forbid(unsafe_code)]

pub mod isotime;
pub mod layer;
pub mod sink;

pub use isotime::{civil_from_days, iso_millis, now_iso_millis, timestamp};
pub use layer::SinkLayer;
pub use sink::{dropped_lines, heartbeat_interval, max_bytes, OrgLog, NO_ORGANIZATION};

use std::sync::atomic::{AtomicBool, Ordering};

use tracing_subscriber::layer::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// How long ago `since` was, in whole milliseconds.
///
/// The unit is fixed at milliseconds everywhere, and saturates rather than
/// wrapping, so `detail.durationMs` means one thing in every line and two
/// records are comparable without reading the call site that produced them. A
/// step measured in seconds here and millis there is a step nobody can rank.
#[must_use]
pub fn elapsed_ms(since: std::time::Instant) -> u64 {
    duration_ms(since.elapsed())
}

/// A [`Duration`](std::time::Duration) in whole milliseconds, saturating.
#[must_use]
pub fn duration_ms(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// How long ago `since` was, in whole MICROseconds.
///
/// # Why a second unit exists, when the rule above says one unit everywhere
///
/// The rule was written for steps measured in seconds and minutes, where a
/// millisecond is beneath notice. The click path is not that: its whole budget
/// is 50ms, so milliseconds quantise it into three
/// or four buckets and a 400µs regression rounds to zero. A number that cannot
/// express the difference between "fast" and "twice as slow" is not a
/// measurement.
///
/// The unit is in the FIELD NAME — `elapsed_us`, never `elapsed_ms` — so no
/// reader can mistake one for the other, which is the property the one-unit
/// rule was protecting.
#[must_use]
pub fn elapsed_us(since: std::time::Instant) -> u64 {
    duration_us(since.elapsed())
}

/// A [`Duration`](std::time::Duration) in whole microseconds, saturating.
#[must_use]
pub fn duration_us(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// Install the process-wide subscriber: the console formatter this program
/// already had, plus the file sink.
///
/// `service` names the stream file and should be the program name (`chief`,
/// `chiefd`, `beacond`), so one directory listing reads as a list of the
/// programs that have run.
///
/// # Colour is decided by the SINK, never by the inherited environment (gh#502)
///
/// `tracing_subscriber`'s formatter defaults ANSI *on* whenever the `ansi`
/// feature is compiled in — it does not look at the destination — and chiefd in
/// production writes to a regular file no terminal ever renders. The damage is
/// not cosmetic: the formatter colours the SEPARATOR, so the bytes on disk for
/// `docstore_mounted=true` are
/// `docstore_mounted\x1b[0m\x1b[2m=\x1b[0mtrue`, the literal substring
/// `docstore_mounted=true` never occurs, and an operator `grep 'key=value'`
/// returns a silent, confident zero against a log that plainly states the fact.
/// That refused a real deploy and mis-routed several live investigations in one
/// day. `crates/chiefd-daemon/tests/log_file_sink_is_plain_text.rs` asserts it
/// on the bytes that reach disk.
///
/// The rule is written here ONCE. It was pasted into three `main` functions,
/// which is two more places for it to be got wrong.
///
/// # Panics
///
/// Never in practice. Installing twice in one process is a programming error
/// the subscriber reports; this returns quietly rather than aborting a daemon
/// over its own logging.
pub fn install(service: &str) {
    let console = tracing_subscriber::fmt::layer()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_filter(ConsoleGate);
    // A failed install means somebody already installed one. A logger must
    // never be the reason a program stops.
    // `Option<Layer>` is a `Layer`: with no directory to write into, the file
    // half is simply absent and the console half still carries every line. See
    // `sink::log_root_from_env` for why that beats inventing a path the process
    // probably cannot write.
    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(console)
        .with(SinkLayer::from_env(service))
        .try_init();
}

/// Whether the console layer has been switched off for the rest of the process.
static CONSOLE_SILENCED: AtomicBool = AtomicBool::new(false);

/// **The terminal now belongs to a full-screen program. Stop writing to it.**
///
/// Call this at the exact moment a TUI takes the glass — Pi's, ratatui's, or
/// anyone's. From here on every event still reaches
/// `<dir>/.chief/log/<program>.jsonl` and NOTHING reaches stdout.
///
/// # Why this is a phase, not a program
///
/// The same defect has now been reported on two surfaces, and the second one is
/// why this is a runtime call rather than a second `install_*` function. A
/// program is not "a TUI program" or "a CLI program"; it is a CLI program that
/// at some instant hands its terminal over. `chief` is the clearest case:
/// its preflight and "starting the Founder" lines are the operator's ONLY
/// feedback and removed an 84-second silence, so they must print — and fifteen
/// seconds later the Founder pane exists and the very same process is painting
/// `founder.session.heartbeat` across Pi's interface. One install decision
/// cannot serve both halves; a boundary can.
///
/// Nothing is silenced in the sense that matters. The file sink is untouched,
/// so `waited_ms=15079 pid=9047 alive=true` is still recorded and still
/// answers "is this hung or working" — it just stops being painted over the
/// thing the operator is trying to read.
///
/// Idempotent, and deliberately one-way: a terminal handed to a TUI is not
/// handed back inside the same process.
pub fn terminal_belongs_to_a_tui() {
    console_off();
}

/// LOGS GO TO THE FILE. NOWHERE ELSE.
///
/// The operator's ruling, 2026-08-14, after a run of `chief attach` painted
/// forty `INFO chief::http` lines over the thing they were trying to read:
/// "make sure you never log to the screen… always log to only the file, don't
/// log anywhere else."
///
/// So this is thrown once, for the whole program, rather than at a phase
/// boundary — the boundary that used to exist was an attempt to decide WHEN a
/// terminal stopped belonging to the operator, and the honest answer is that it
/// never belonged to the log. The file sink is untouched and is now the only
/// record, which is what makes it worth reading.
///
/// What still reaches a terminal is what a person asked for: this program's own
/// `println!` progress and refusals (`chief attach: starting ChiefD…`). Those
/// are output, not logging, and the distinction is the whole rule — if you find
/// yourself reaching for `tracing` to tell the operator something, you want
/// `println!`; if you find yourself reaching for `println!` to record what
/// happened, you want `tracing`.
///
/// Idempotent and one-way.
pub fn console_off() {
    CONSOLE_SILENCED.store(true, Ordering::Relaxed);
}

/// The switch [`terminal_belongs_to_a_tui`] throws, as a per-layer filter.
///
/// `callsite_enabled` deliberately answers `sometimes()` rather than a cached
/// verdict: `tracing` caches a callsite's interest the first time it sees it,
/// so a filter that answered `always()` before the hand-over would keep every
/// line printing forever afterwards. Answering `sometimes()` costs one atomic
/// load per event and is what makes the switch actually able to flip.
struct ConsoleGate;

impl<S> tracing_subscriber::layer::Filter<S> for ConsoleGate {
    fn enabled(
        &self,
        _meta: &tracing::Metadata<'_>,
        _cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !CONSOLE_SILENCED.load(Ordering::Relaxed)
    }

    fn callsite_enabled(
        &self,
        _meta: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
}

/// The one filter both installs use: `RUST_LOG` when set, `info` otherwise.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}
