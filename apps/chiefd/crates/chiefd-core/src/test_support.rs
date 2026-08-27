//! Test-only helpers, gated behind the `test-support` cargo feature.
//!
//! Why a feature and not `#[cfg(test)]`: integration tests, the conformance
//! runner and the e2e harness are separate crates, and `cfg(test)` items are
//! invisible to them. Why gated at all: [`ManualClock`] turns every
//! documented wait into
//! no wait. In a live company that is not a faster chiefd, it is the
//! fail-fast-with-no-retry bug this project has shipped three times.
//!
//! CI asserts no release build enables the feature — see
//! `apps/chiefd/README.md`, "test-support policy".

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::clock::{Clock, Monotonic, Sleep, WallMillis};
use crate::isotime::iso_millis;
use crate::store::organization::{
    DepartmentRecord, EmploymentState, OrganizationManifest, OrganizationPolicy, PersonKind,
    PersonRecord, UnitKind, UnitState, ORGANIZATION_SCHEMA_VERSION, ROOT_DEPARTMENT_ID,
};

/// The frozen instant every conformance fixture is recorded against.
///
/// **One definition, because four had to agree.** This lived in
/// `conformance/lib/world.ts` and was copied into `conformance_common/mod.rs`
/// and privately into each of the three chiefd-api replay runners — four Rust
/// constants plus a TypeScript one, none of them checked against another. The
/// TypeScript harness is deleted (#1046 gave the corpus a recorder that is the
/// replay runner itself, and ruled the TypeScript half out of the corpus), so
/// this is now the only statement of it and every runner reads it.
///
/// It is not a preference. Every `createdAt`, `nextDueAt` and derived deadline
/// in `conformance/fixtures/` is a literal computed from this instant, so
/// changing it does not re-time the suite — it invalidates the corpus. A runner
/// parks its `ManualClock` here precisely so a fixture stays a statement about
/// behaviour rather than a snapshot of the day it was recorded.
pub const CONFORMANCE_EPOCH: i64 = 1_784_116_800_000;

/// [`CONFORMANCE_EPOCH`] as the corpus writes it. The pair is asserted equal in
/// this module's tests, so the two spellings cannot drift apart.
pub const CONFORMANCE_EPOCH_ISO: &str = "2026-07-15T12:00:00.000Z";

/// A clock that only moves when a test moves it.
///
/// Expiry, renewal and backoff tests advance this explicitly; no test sleeps
/// to wait for a timeout (TESTING.md §4.2).
///
/// [`Clock::sleep`] is part of that: a wait taken against this clock resolves
/// when — and only when — a test calls [`ManualClock::advance`] past its
/// deadline. That is what makes "the holder released while the acquirer was
/// mid-backoff" a *decision the test makes* rather than a window it hopes to
/// hit. A backoff that is never advanced past stays pending forever, so a test
/// asserting "this call is still waiting" cannot pass by accident.
#[derive(Debug)]
pub struct ManualClock {
    monotonic_ms: AtomicU64,
    wall_ms: AtomicI64,
    sleepers: Mutex<Vec<(u64, oneshot::Sender<()>)>>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::starting_at(0, 1_700_000_000_000)
    }
}

impl ManualClock {
    /// A clock parked at the given monotonic and wall readings.
    #[must_use]
    pub fn starting_at(monotonic_ms: u64, wall_ms: i64) -> Self {
        Self {
            monotonic_ms: AtomicU64::new(monotonic_ms),
            wall_ms: AtomicI64::new(wall_ms),
            sleepers: Mutex::new(Vec::new()),
        }
    }

    /// Move both readings forward by `d`, then resolve every wait whose
    /// deadline that just passed.
    pub fn advance(&self, d: Duration) {
        let millis = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        self.monotonic_ms.fetch_add(millis, Ordering::SeqCst);
        self.wall_ms.fetch_add(i64::try_from(millis).unwrap_or(i64::MAX), Ordering::SeqCst);
        self.fire_due();
    }

    /// How many waits are currently parked on this clock.
    ///
    /// A test asserting "the acquirer is backing off right now" reads this,
    /// which is a state, not a timing guess.
    #[must_use]
    pub fn pending_sleeps(&self) -> usize {
        self.sleepers.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    fn fire_due(&self) {
        let now = self.monotonic_ms.load(Ordering::SeqCst);
        let mut sleepers = self.sleepers.lock().unwrap_or_else(|p| p.into_inner());
        let mut still_waiting = Vec::with_capacity(sleepers.len());
        for (due, tx) in sleepers.drain(..) {
            if due <= now {
                // The receiver is gone if the wait lost a `select!`; that is a
                // normal outcome, not a failure.
                let _ = tx.send(());
            } else {
                still_waiting.push((due, tx));
            }
        }
        *sleepers = still_waiting;
    }
}

impl Clock for ManualClock {
    fn monotonic(&self) -> Monotonic {
        Monotonic(self.monotonic_ms.load(Ordering::SeqCst))
    }

    fn wall(&self) -> WallMillis {
        WallMillis(self.wall_ms.load(Ordering::SeqCst))
    }

    fn sleep(&self, d: Duration) -> Sleep {
        let now = self.monotonic_ms.load(Ordering::SeqCst);
        let due = now.saturating_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        if due <= now {
            return Box::pin(std::future::ready(()));
        }
        let (tx, rx) = oneshot::channel();
        self.sleepers.lock().unwrap_or_else(|p| p.into_inner()).push((due, tx));
        Box::pin(async move {
            let _ = rx.await;
        })
    }
}

// --- the conformance company ------------------------------------------------

/// The provider every template person carries.
///
/// One template person's fields, as a value.
struct TemplatePerson<'a> {
    id: &'a str,
    name: &'a str,
    title: &'a str,
    mandate: &'a str,
    kind: PersonKind,
    department_id: &'a str,
    tools: &'a [&'a str],
}

fn template_person(spec: &TemplatePerson<'_>, created_at: &str) -> PersonRecord {
    let &TemplatePerson { id, name, title, mandate, kind, department_id, tools } = spec;
    PersonRecord {
        id: id.to_string(),
        name: name.to_string(),
        title: title.to_string(),
        mandate: mandate.to_string(),
        kind,
        department_id: department_id.to_string(),
        employment_state: EmploymentState::Active,
        activation: if kind == PersonKind::Worker { "on-demand" } else { "resident" }.to_string(),
        tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
        prompts: Vec::new(),
        created_at: created_at.to_string(),
        staffing_history: None,
        extra: BTreeMap::new(),
    }
}

fn template_unit(
    id: &str,
    name: &str,
    purpose: &str,
    kind: UnitKind,
    parent: Option<&str>,
    head: &str,
    created_at: &str,
) -> DepartmentRecord {
    DepartmentRecord {
        id: id.to_string(),
        name: name.to_string(),
        purpose: purpose.to_string(),
        kind: Some(kind),
        transient: None,
        parent_department_id: parent.map(ToString::to_string),
        head_person_id: head.to_string(),
        state: UnitState::Active,
        created_at: created_at.to_string(),
        extra: BTreeMap::new(),
    }
}

/// The `northstar` company the conformance corpus is recorded against.
///
/// This is now the ONLY statement of that world. `conformance/lib/world.ts`
/// used to own the spec and `createOrganization` derived the manifest from it;
/// that file is deleted, so what a fixture was recorded against is whatever
/// this function returns. The fixtures themselves are the check —
/// `the_northstar_template_matches_what_the_fixtures_were_recorded_against` in
/// each runner fails if this drifts from them. This function produces the
/// derived manifest directly,
/// because spec normalization needs the model catalog (see [`TEMPLATE_MODEL`])
/// and `org.hire` (plan §2.2), neither of which is M12's. Everything the corpus
/// actually observes — ids, kinds, ancestry, placement, employment state,
/// ordering, revision, runtime session — is derived from the same rules
/// `normalizeOrganizationSpec` applies, and
/// `the_northstar_template_matches_what_the_fixtures_were_recorded_against` in
/// each runner pins it against the fixture corpus itself.
///
/// Structure: a CEO over two departments; Quant has one worker, IT has none.
/// Enough for manager/worker fences, department ancestry, and a non-managing
/// worker.
#[must_use]
pub fn northstar_manifest(created_at_millis: i64) -> OrganizationManifest {
    let at = iso_millis(created_at_millis);
    let manager_tools = ["read", "bash", "write", "grep", "find", "ls"];
    // `defaultTools(worker, "research")`. Since the operator removed invariant
    // 34 (2026-08-10) the manager grant carries `bash` too; the two differ now
    // only in ordering, not in shell access.
    let research_tools = ["read", "bash", "write", "grep", "find", "ls"];

    let people = [
        TemplatePerson {
            id: "chief",
            name: "Avery",
            title: "Chief",
            mandate: "Set direction for Northstar Conformance, delegate through department heads, and make final organization decisions.",
            kind: PersonKind::Executive,
            department_id: ROOT_DEPARTMENT_ID,
            tools: &manager_tools,
        },
        TemplatePerson {
            id: "quant-head",
            name: "Quinn",
            title: "Head of Quant",
            mandate: "Delegate Quant work, supervise delivery, and report decision-ready results to the parent head.",
            kind: PersonKind::Head,
            department_id: "quant",
            tools: &manager_tools,
        },
        TemplatePerson {
            id: "signal-researcher",
            name: "Signal Researcher",
            title: "Signal Researcher",
            mandate: "Own assigned Quant work and return a concise, verified result to the department head.",
            kind: PersonKind::Worker,
            department_id: "quant",
            tools: &research_tools,
        },
        TemplatePerson {
            id: "it-head",
            name: "Ira",
            title: "Head of IT",
            mandate: "Delegate IT work, supervise delivery, and report decision-ready results to the parent head.",
            kind: PersonKind::Head,
            department_id: "it",
            tools: &manager_tools,
        },
    ]
    .map(|spec| template_person(&spec, &at));
    let units = [
        template_unit(
            ROOT_DEPARTMENT_ID,
            "Northstar Conformance",
            "Freeze durable store behaviour as language-neutral fixtures.",
            UnitKind::Company,
            None,
            "chief",
            &at,
        ),
        template_unit(
            "quant",
            "Quant",
            "Own systematic research.",
            UnitKind::Department,
            Some(ROOT_DEPARTMENT_ID),
            "quant-head",
            &at,
        ),
        template_unit(
            "it",
            "IT",
            "Ship products.",
            UnitKind::Department,
            Some(ROOT_DEPARTMENT_ID),
            "it-head",
            &at,
        ),
    ];

    OrganizationManifest {
        schema_version: ORGANIZATION_SCHEMA_VERSION,
        kind: "organization".to_string(),
        slug: "northstar-conformance".to_string(),
        name: "Northstar Conformance".to_string(),
        purpose: "Freeze durable store behaviour as language-neutral fixtures.".to_string(),
        root_department_id: ROOT_DEPARTMENT_ID.to_string(),
        policy: OrganizationPolicy {
            supervision_interval_ms: 15 * 60 * 1_000,
            acknowledgement_timeout_ms: 90 * 1_000,
            acknowledgement_retry_limit: 1,
            replacement_limit: 1,
        },
        // Insertion order, exactly as `normalizeOrganizationSpec` builds it:
        // the root first, then each department in spec order; people are the
        // CEO, then per department its head followed by its staff.
        department_order: units.iter().map(|unit| unit.id.clone()).collect(),
        people_order: people.iter().map(|person| person.id.clone()).collect(),
        departments: units.into_iter().map(|unit| (unit.id.clone(), unit)).collect(),
        people: people.into_iter().map(|person| (person.id.clone(), person)).collect(),
        created_at: at.clone(),
        updated_at: at,
        extra: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two spellings of the conformance epoch are the same instant.
    ///
    /// They are a pair because the corpus writes ISO strings and a `ManualClock`
    /// takes millis, and a pair is exactly the shape that drifts. Before this
    /// module owned it there were five copies across two languages and nothing
    /// compared any of them; changing one and not the others would have moved
    /// every fixture literal without a single test noticing.
    #[test]
    fn the_conformance_epochs_two_spellings_are_the_same_instant() {
        assert_eq!(
            crate::isotime::parse_iso_millis(CONFORMANCE_EPOCH_ISO),
            Some(CONFORMANCE_EPOCH),
            "CONFORMANCE_EPOCH_ISO and CONFORMANCE_EPOCH name different instants",
        );
        assert_eq!(iso_millis(CONFORMANCE_EPOCH), CONFORMANCE_EPOCH_ISO);
    }

    #[test]
    fn manual_clock_does_not_move_on_its_own() {
        let clock = ManualClock::default();
        let first = clock.monotonic();
        for _ in 0..1000 {
            assert_eq!(clock.monotonic(), first);
        }
    }

    #[test]
    fn advance_moves_monotonic_and_wall_together() {
        let clock = ManualClock::starting_at(0, 1_000);
        clock.advance(Duration::from_secs(31));
        assert_eq!(clock.monotonic(), Monotonic(31_000));
        assert_eq!(clock.wall(), WallMillis(32_000));
    }

    #[tokio::test]
    async fn a_wait_on_the_manual_clock_resolves_only_when_a_test_advances_past_it() {
        let clock = ManualClock::default();
        let mut wait = clock.sleep(Duration::from_millis(250));
        assert_eq!(clock.pending_sleeps(), 1);

        // One millisecond short: still parked. `now_or_never`-style polling
        // without the dependency — a ready future would resolve here.
        clock.advance(Duration::from_millis(249));
        assert_eq!(
            futures_poll_once(&mut wait).await,
            None,
            "a wait must not resolve early: that is the fail-fast bug in miniature"
        );

        clock.advance(Duration::from_millis(1));
        assert_eq!(futures_poll_once(&mut wait).await, Some(()));
        assert_eq!(clock.pending_sleeps(), 0);
    }

    #[tokio::test]
    async fn a_zero_wait_is_already_resolved() {
        let clock = ManualClock::default();
        let mut wait = clock.sleep(Duration::ZERO);
        assert_eq!(futures_poll_once(&mut wait).await, Some(()));
        assert_eq!(clock.pending_sleeps(), 0);
    }

    /// Poll a wait exactly once, without a dependency and without `unsafe`.
    ///
    /// `biased` makes the arm order the poll order, and the second arm is
    /// always ready, so this resolves in one pass: `Some(())` iff the wait was
    /// already done.
    async fn futures_poll_once<F: std::future::Future<Output = ()> + Unpin>(
        f: &mut F,
    ) -> Option<()> {
        tokio::select! {
            biased;
            () = f => Some(()),
            () = std::future::ready(()) => None,
        }
    }
}
