//! **One id per operator gesture, carried by every line that gesture causes.**
//!
//! # The defect this closes
//!
//! Before this existed, no client-side funnel in this product could be measured
//! honestly. `~/.chief/log/chief.jsonl` carries a `session` field, and on the
//! operator's own box **3,664 of 3,715 lines held the same value** — there is
//! one session. It carries a `pid`, and the rail process is replaced mid-episode
//! (every person click used to mint a window and boot a fresh rail into it), so
//! a pid does not name an episode either. Every number quoted about a click on
//! 2026-08-15 was therefore computed by taking each `sidebar.click` and pairing
//! it with the *next* line of some other kind — a nearest-next-in-time
//! heuristic, which is a guess whenever two gestures overlap, and gestures
//! overlap exactly when the product is slow.
//!
//! One measured consequence: a cold person click was reported as "1–37ms"
//! because the pairing landed on `sidebar.window.laid`, an event that fires a
//! median **2ms BEFORE** the wake it supposedly completed. The honest number was
//! 5,636ms. A correlator is not instrumentation polish; it is the precondition
//! for any sub-second claim about this product being true.
//!
//! # What an id is
//!
//! Microseconds since the Unix epoch at the instant the mouse event was
//! decoded, forced strictly increasing within a process. Three properties, each
//! load-bearing:
//!
//! * **Monotonic.** Sorting by id is sorting by gesture order, so a funnel reads
//!   top to bottom without a join.
//! * **Self-describing.** The id IS the click's wall-clock time to the
//!   microsecond, so `sidebar.click` needs no second field to answer "when", and
//!   an id can be compared against the `at` stamp of any other line in the file.
//! * **Unique without coordination.** One process mints these now — the session
//!   brain — so within it the counter is forced upward and even a clock that
//!   repeats or steps backwards cannot mint the same id twice. The property was
//!   load-bearing when every window ran its own rail, and it is kept rather than
//!   simplified away: it is what lets two boxes' logs be read side by side, and
//!   it costs one atomic compare-exchange.
//!
//! # How it travels
//!
//! Two ways, and no third:
//!
//! * **Inside the process that was clicked** — a `tracing` span
//!   ([`GestureId::span`]). `chiefd_log`'s layer resolves `gesture_id` from the
//!   enclosing span exactly as it resolves the company slug, so every line the
//!   gesture emits, at any depth, carries it without the call site knowing.
//! * **Across processes** — the FRAME the brain pushes to a thin rail client
//!   ([`crate::sidebar::wire::ToClient::Frame`]). There is exactly one process
//!   that decodes a mouse event now, so the only boundary the id has to cross
//!   is the one between the brain and the terminal that shows its answer — and
//!   the frame IS that answer, so the correlator rides the thing it is about
//!   and cannot drift from it. The client writes `sidebar.frame.painted` from
//!   it, which is the one event in this product meaning the operator can SEE
//!   something, and it needs no second field for the elapsed time because the
//!   id is the click's own wall clock.
//!
//!   It was the third field of the session's `SELECTION` tmux option until
//!   Stage 3 of that work deleted that option along with the
//!   rest of the cross-process bus. The rule it followed is unchanged and is
//!   why it moved here rather than into an option of its own: a SECOND channel
//!   holding a second copy of "the current gesture" is the shape that produces
//!   two answers.

use std::sync::atomic::{AtomicU64, Ordering};

/// The last id this process minted. Forces [`next`] strictly upward.
static LAST: AtomicU64 = AtomicU64::new(0);

/// One operator gesture, from the mouse event to the frame that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GestureId(u64);

/// Mint the id for a gesture that is starting NOW.
///
/// Called at exactly one place in the product — the brain's left-button-down
/// arm ([`crate::sidebar::brain`]) — because an id minted anywhere else would
/// name something that is not a gesture.
#[must_use]
pub fn next() -> GestureId {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_micros()).unwrap_or(u64::MAX));
    let mut previous = LAST.load(Ordering::Relaxed);
    loop {
        let candidate = mint(now, previous);
        match LAST.compare_exchange_weak(previous, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return GestureId(candidate),
            Err(seen) => previous = seen,
        }
    }
}

/// The whole rule [`next`] applies, as a pure function of the clock and the
/// last id minted.
///
/// A clock that repeated or stepped back still yields a NEW id. An id that
/// collided with the previous one would silently merge two funnels into one,
/// which is the failure this whole module exists to end.
///
/// # Why this is a function and not three lines inside the loop
///
/// So the SCALE can be proven without the process-global `LAST`. The test that
/// stood here bracketed a live `next()` between two clock readings, and it was
/// a flake with a mechanism: another test in the same binary mints ids
/// concurrently, every mint landing inside an already-used microsecond takes
/// the `previous + 1` branch, and `LAST` ends up ahead of the wall clock by
/// roughly the number of collisions — so a later `next()` legitimately returns
/// a value ABOVE the `after` reading. The upper bracket was never the property;
/// it was the monotonic guarantee doing exactly what it is for. Stated as a
/// pure function, the property is checkable exactly.
#[must_use]
const fn mint(now: u64, previous: u64) -> u64 {
    if now > previous {
        now
    } else {
        previous.saturating_add(1)
    }
}

impl GestureId {
    /// The raw number, as it appears in `detail.gesture_id`.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// An id read back off the wire, or `None`.
    ///
    /// Zero is refused: it is what an absent field becomes, and "no gesture"
    /// and "gesture 0" must not be the same fact.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// The span every line of this gesture is emitted inside.
    ///
    /// `info` because that is the production filter: a correlator visible only
    /// under `RUST_LOG` is a correlator nobody has when they need it. Its own
    /// two lines bracket the gesture, and the closing one carries the
    /// synchronous cost of the whole handler in `durationMs`.
    #[must_use]
    pub fn span(self) -> tracing::Span {
        tracing::info_span!("sidebar.gesture", gesture_id = self.0)
    }
}

impl std::fmt::Display for GestureId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::str::FromStr for GestureId {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.trim().parse::<u64>().ok().and_then(Self::from_raw).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::{next, GestureId};
    use std::collections::BTreeSet;

    /// THE RULE: two gestures are never the same gesture. A repeated id merges
    /// two funnels and every number computed from the merge is a lie.
    #[test]
    fn every_mint_is_a_new_id_even_when_the_clock_does_not_move() {
        let ids: BTreeSet<u64> = (0..10_000).map(|_| next().raw()).collect();
        assert_eq!(ids.len(), 10_000, "a mint repeated an id");
    }

    /// Monotonic, so sorting a log by `gesture_id` is sorting it by gesture
    /// order — including across the threads a tokio runtime may mint from.
    #[test]
    fn ids_increase_even_when_minted_from_several_threads() {
        let threads: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(|| (0..500).map(|_| next().raw()).collect::<Vec<_>>()))
            .collect();
        let mut all: Vec<u64> = threads
            .into_iter()
            .flat_map(|thread| thread.join().expect("the minting thread must finish"))
            .collect();
        let count = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), count, "two threads minted the same id");
    }

    /// An id is the click's wall clock, so it must be comparable against the
    /// `at` stamp of every other line in the same file.
    #[test]
    fn an_id_reads_as_microseconds_since_the_epoch() {
        // THE SCALE, checked exactly, against a stated `previous` rather than
        // against whatever the rest of this binary has already minted. An id
        // with nothing before it IS the clock reading — same unit, same origin
        // as every `at` stamp in the file it will be correlated against.
        let now = 1_786_060_800_000_000;
        assert_eq!(super::mint(now, 0), now, "an id is the microsecond it was minted in");
        assert_eq!(super::mint(now, now - 1), now, "and stays the clock while the clock leads");

        // A live mint is still exercised, and bounded from BELOW — which holds
        // unconditionally, because `mint` never answers below the clock. There
        // is deliberately no upper bracket: see `mint`'s own doc for the flake
        // that shape produced and why the excess it caught was correct.
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the epoch is in the past")
            .as_micros();
        let id = u128::from(next().raw());
        assert!(id >= before, "{id} is before {before}, so it is not this clock's microseconds");
        // Microseconds and not millis or nanos, stated as a magnitude: a
        // millisecond id would be a thousand times smaller than the reading
        // above and a nanosecond one a thousand times larger.
        assert!(id < before.saturating_mul(2), "{id} is not on the same scale as {before}");
    }

    /// THE MONOTONIC GUARANTEE, which is what the retired bracket was
    /// accidentally catching.
    ///
    /// Two gestures inside one microsecond, and a clock that steps backwards,
    /// must both still yield ids that rise. An id that repeated would merge two
    /// funnels into one — every line of two different gestures under one
    /// correlator — which is the failure this module exists to end.
    #[test]
    fn an_id_rises_even_when_the_clock_does_not() {
        let now = 1_786_060_800_000_000;
        assert_eq!(super::mint(now, now), now + 1, "two gestures in one microsecond");
        assert_eq!(super::mint(now, now + 5_000), now + 5_001, "a clock that stepped back");
        assert_eq!(
            super::mint(0, u64::MAX),
            u64::MAX,
            "and it saturates rather than wrapping to 0"
        );
    }

    /// "No gesture" and "gesture 0" are different facts, and an absent field on
    /// the wire must read as the first.
    #[test]
    fn zero_is_not_a_gesture() {
        assert_eq!(GestureId::from_raw(0), None);
        assert_eq!(GestureId::from_raw(1).map(GestureId::raw), Some(1));
        assert_eq!("".parse::<GestureId>(), Err(()));
        assert_eq!("0".parse::<GestureId>(), Err(()));
        assert_eq!(" 42 ".parse::<GestureId>().map(GestureId::raw), Ok(42));
    }
}
