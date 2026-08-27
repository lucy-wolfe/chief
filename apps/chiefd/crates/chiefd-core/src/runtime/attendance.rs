//! Whether anybody is converging this company — the one upward fact chiefd
//! kept, derived from a request it already answers.
//!
//! # The blindness this removes
//!
//! At 22:17:40Z on a live company the whole tmux server went away with all
//! eleven panes and all five people in it. chiefd committed a supervision cycle
//! five seconds later, and every five seconds after that, reporting a healthy
//! pass and a headcount of five. It was not wrong by accident: #751/P8-P10
//! deleted every report from the actuator, and
//! `chiefd-api/src/docstore/desired.rs` wrote the consequence down as an
//! accepted loss — "No actuator is no longer an answer chiefd can give". A
//! supervisor that cannot tell whether the thing it supervises exists is not
//! supervising, so the loss is not acceptable and this is the repair.
//!
//! # Why this is not the upward channel that was deleted
//!
//! Nothing new travels up. The actuator reads the desired set
//! (`POST /v1/org/runtime/desired`) on every round of its loop, and its loop
//! runs at least as often as its own changefeed ceiling
//! (`Schedule::idle_wait`, 30 s) even when the company is silent. chiefd
//! already serves those reads; all this type does is remember WHEN the last one
//! landed. There is no session, socket, window, pane or layout in it — the
//! facts `desired.rs` bars — and no new route, body or verb to carry one.
//!
//! An actuator that has stopped reading is an actuator that has stopped
//! converging, whatever the reason: its process died, its tmux server died
//! under it, or the operator closed the terminal. chiefd does not learn WHICH,
//! and deliberately does not try: the distinction needs a display, and the
//! decision it drives — say so, every pass — is the same for all three.
//!
//! # Seeded at boot, never absent
//!
//! The handle is created with the daemon's own start time rather than as an
//! `Option` or a `None`-means-never sentinel. A company that has just booted is
//! attended for one lapse window and then, if nobody has come for its desired
//! set, is not — one rule, no special first case, and no way to spell "I have
//! no opinion" into a signal whose whole job is to have one.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// How long chiefd may go unread before it reports that nothing is converging
/// this company.
///
/// Three times the actuator's own 30-second changefeed ceiling
/// (`chief_cli::actuate::resident::Schedule::idle_wait`): a single missed round
/// is a slow network, silence across three consecutive ceilings is nobody
/// there. The same "three windows" rule
/// [`crate::store::supervisor_watermark::SUPERVISOR_DUTY_STALE_MULTIPLE`]
/// applies to a hosted duty, for the same reason.
pub const ACTUATOR_LAPSE_MS: i64 = 90_000;

/// When an actuator last asked this company what should be running.
///
/// Cheap to clone; every clone shares one cell. The daemon, the health
/// gatherer and the desired-set route each hold one.
#[derive(Debug, Clone)]
pub struct ActuatorAttendance {
    last_read_ms: Arc<AtomicI64>,
}

impl ActuatorAttendance {
    /// Seed a fresh handle at `now_millis` — the daemon's boot instant.
    #[must_use]
    pub fn new(now_millis: i64) -> Self {
        Self { last_read_ms: Arc::new(AtomicI64::new(now_millis)) }
    }

    /// Record that the desired set was read at `now_millis`.
    ///
    /// Monotonic: a stamp older than the one already recorded is discarded, so
    /// two concurrent readers cannot make the company look less attended than
    /// the later of them proved it to be.
    pub fn record_read(&self, now_millis: i64) {
        self.last_read_ms.fetch_max(now_millis, Ordering::Relaxed);
    }

    /// Epoch millis of the last read (or of the seed, if there has been none).
    #[must_use]
    pub fn last_read_ms(&self) -> i64 {
        self.last_read_ms.load(Ordering::Relaxed)
    }

    /// How long chiefd has gone unread as of `now_millis`. Never negative — a
    /// clock that stepped backwards reads as "just now" rather than as a
    /// negative age some caller would compare against a threshold.
    #[must_use]
    pub fn silent_ms(&self, now_millis: i64) -> i64 {
        (now_millis - self.last_read_ms()).max(0)
    }

    /// Whether an actuator is converging this company as of `now_millis`.
    #[must_use]
    pub fn attended(&self, now_millis: i64) -> bool {
        self.silent_ms(now_millis) <= ACTUATOR_LAPSE_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT: i64 = 1_700_000_000_000;

    #[test]
    fn a_freshly_booted_company_is_attended() {
        let attendance = ActuatorAttendance::new(BOOT);
        assert!(attendance.attended(BOOT));
    }

    #[test]
    fn silence_past_the_lapse_window_is_unattended() {
        let attendance = ActuatorAttendance::new(BOOT);
        assert!(attendance.attended(BOOT + ACTUATOR_LAPSE_MS), "the boundary is still attended");
        assert!(!attendance.attended(BOOT + ACTUATOR_LAPSE_MS + 1));
    }

    #[test]
    fn a_read_restores_attendance() {
        let attendance = ActuatorAttendance::new(BOOT);
        let late = BOOT + ACTUATOR_LAPSE_MS * 10;
        assert!(!attendance.attended(late));
        attendance.record_read(late);
        assert!(attendance.attended(late));
        assert_eq!(attendance.silent_ms(late), 0);
    }

    /// Two readers racing must not be able to age the company backwards: the
    /// later proof of attendance wins, whichever order the stamps arrive in.
    #[test]
    fn an_older_stamp_never_undoes_a_newer_one() {
        let attendance = ActuatorAttendance::new(BOOT);
        attendance.record_read(BOOT + 10_000);
        attendance.record_read(BOOT + 1_000);
        assert_eq!(attendance.last_read_ms(), BOOT + 10_000);
    }

    #[test]
    fn a_clock_that_stepped_backwards_reads_as_just_now() {
        let attendance = ActuatorAttendance::new(BOOT);
        assert_eq!(attendance.silent_ms(BOOT - 60_000), 0);
        assert!(attendance.attended(BOOT - 60_000));
    }

    #[test]
    fn every_clone_shares_one_cell() {
        let attendance = ActuatorAttendance::new(BOOT);
        let other = attendance.clone();
        other.record_read(BOOT + 5_000);
        assert_eq!(attendance.last_read_ms(), BOOT + 5_000);
    }
}
