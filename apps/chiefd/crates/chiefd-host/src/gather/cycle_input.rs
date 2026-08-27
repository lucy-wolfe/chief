//! Duty #3 — the gatherer for the D9 supervision cycle.
//!
//! Assembles a [`CycleInput`] for
//! [`cycle`](chiefd_core::store::supervision::cycle) from chiefd's OWN durable
//! facts. It reads no host fact and can no longer acquire one.
//!
//! # TOMBSTONE: where the observation came from
//!
//! This gatherer once called `HostExecutor::audit_session` and looked at a
//! display. #751/P8 stopped that and replaced it with a READ of the report the
//! operator client committed through `POST /v1/org/runtime/observed`. Both are
//! now gone, and the second is the one worth explaining: reading a report
//! instead of taking a look was a real improvement — chiefd stopped touching
//! tmux — but it left the SHAPE intact, a durable decision conditioned on a
//! host fact. That shape is what produced the defect this branch exists to
//! remove.
//!
//! The old fail-closed rule deserves recording because it was RIGHT.
//! `Observation` was an enum, so "untrusted, and here are zero people" could
//! not be written down; an absent report and an untrusted one both exited as an
//! error, never as an empty-and-healthy `CycleInput`. It was defeated one call
//! away, by a caller that passed `Some(observed_person_ids)` unconditionally.
//! Hence the removal of the input rather than a repair of the reader.
//!
//! # Which fields this gathers, and which it is handed
//!
//! [`CycleInput`] has four fields. Identity is a durable fact. The other three
//! have no host source and use their safe values.
//!
//! | Field | Source |
//! |---|---|
//! | `identity` | durable runtime-ownership doc, keyed by socket (given) |
//! | `suppressed` | ALWAYS `false` — see the tombstone below |
//! | `audit` | EMPTY, and nothing may be derived from its emptiness |
//! | `unhealthy` | EMPTY, for the same reason |
//!
//! # The two verdicts chiefd is no longer entitled to
//!
//! `adopted` and `unhealthy` were both derived from the live set. Neither can
//! be derived from its absence, and this is the single most important rule in
//! this file: an empty `live` is NOT "everybody is dead". Fast health used to
//! be a projection diff — desired-active minus live, plus anyone at a stale
//! generation — and run against an empty live set it would mark EVERY DESIRED
//! PERSON UNHEALTHY. That is the original defect reintroduced by a mechanical
//! edit, so the loop is deleted rather than fed an empty input.
//!
//! Staleness moved rather than vanished: the actuator compares its pane's
//! `@organization_launch_hash` tag against the hash chiefd published, which is
//! the same question asked by the only process that can see both sides.
//!
//! # The inert gate short-circuits everything
//!
//! A company the ownership doc says is held by another chiefd is one this
//! daemon must not act on — the pure cycle stops at that gate, so the gatherer
//! returns immediately and the fields the short-circuited cycle never reads are
//! left at their defaults.
//!
//! # TOMBSTONE (chief-home-is-cwd §4c): the fleet-suppression gate
//!
//! There were TWO inert gates, and the first was `suppressed`, read from the
//! CEO boot lease (`ReconcilerFactsStore::identity_and_suppression`). The lease
//! was the exclusivity window an attended CEO-only boot held so this duty could
//! not project the fleet during its slow pre-converge phase. The daemon boots no
//! pane now, so no writer can take the lease, and the gate could only ever read
//! `false`. `CycleGatherContext` loses the field; `CycleInput::suppressed` and
//! `Stage::Suppression` stay because they belong to the pure supervision cycle's
//! FROZEN wire contract (`chiefd_api::wire::SupervisionStage`) — chiefd simply
//! has no source for the input any more, and passes the value the pure cycle
//! treats as "carry on".

use std::sync::Arc;

use chiefd_core::runtime::duty_hooks::{BoxFuture, CycleInputGatherer, DutyContext, DutyError};
use chiefd_core::store::organization;
use chiefd_core::store::supervision::{CycleInput, IdentityObservation};

use crate::executor::HostErr;
use crate::gather::reconciler_facts::ReconcilerFactsStore;

/// Everything the D9 gatherer needs.
///
/// One durable fact chiefd already holds, and nothing else.
#[derive(Debug, Clone)]
pub struct CycleGatherContext {
    /// Runtime-ownership verdict, read from the durable runtime-owner doc
    /// (`Owned` when its active socket is ours, `Foreign` otherwise).
    pub identity: IdentityObservation,
}

/// Gather a [`CycleInput`] from chiefd's OWN durable facts and nothing else.
///
/// It cannot fail on a host fact any more, because it reads none. The `Result`
/// is kept because the two inert gates and the caller's error channel are
/// shared, not because there is an observation left to refuse.
///
/// # Errors
/// Infallible today; the signature is the caller's, not this function's.
pub fn gather_cycle_input(ctx: &CycleGatherContext) -> Result<CycleInput, HostErr> {
    // The inert gate: a company another chiefd owns is one this cycle writes
    // nothing for and never reads the observation of, so we do not look at it.
    if matches!(ctx.identity, IdentityObservation::Foreign { .. }) {
        return Ok(CycleInput {
            suppressed: false,
            identity: ctx.identity.clone(),
            ..CycleInput::default()
        });
    }

    // TOMBSTONE: the observation-derived live set.
    //
    // This read the actuator's report, refused a lapsed lease, refused an
    // untrusted observation, and derived `live` from the people it vouched for
    // ALIVE. Its own comment named the property exactly right -- "unproven must
    // never reach the cycle as an empty session, that reads as everybody is
    // dead and drives a teardown" -- and it was correct here. The conflation
    // happened elsewhere, which is the argument for removing the input rather
    // than auditing each reader of it.
    //
    // chiefd has no live set now. The health audit below reports what chiefd
    // DESIRES and nothing about what is running: an empty `live` here would be
    // the "everybody is dead" reading this code refused, so nothing may be
    // derived from its absence either. Every desired person is therefore
    // neither adopted nor unhealthy -- chiefd is not entitled to either verdict.
    // NOTHING IS DERIVED FROM THE ABSENCE, and the loop that used to sit here is
    // deleted rather than fed an empty set.
    //
    // It walked `expected_active` and put every person with no matching live
    // entry into `unhealthy`. Handed an empty live set it would have marked
    // EVERY DESIRED PERSON UNHEALTHY -- the `Some(EMPTY)` conflation this branch
    // exists to kill, reintroduced by a mechanical edit rather than a decision.
    // Leaving the loop and emptying its input is precisely the shape of the
    // original defect: an absence of evidence rendered as evidence of absence.
    //
    // chiefd is entitled to NEITHER verdict now. It knows what it desires, not
    // what is running, so a person is neither adopted nor unhealthy here.
    Ok(CycleInput { identity: ctx.identity.clone(), ..CycleInput::default() })
}

/// The real [`CycleInputGatherer`] — `chiefd run`'s production wiring
/// (od-host-gatherers-completion).
///
/// One source feeds one [`CycleInput`]:
///
/// * the shared company `org.sqlite` (when `facts` is `Some`, i.e.
///   `CHIEFD_STORE_DB_PATH` was set at boot) supplies the runtime-ownership
///   identity and the CEO-boot-lease suppression gate — the two durable facts
///   that genuinely have no chiefd-native store yet (`store/mod.rs`'s
///   inventory does not carry them).
///
/// There is no second source. The `runtime_actuation` row that used to supply
/// the attached operator client's observation is deleted, and with it the only
/// path by which a host fact reached this cycle.
///
/// When `facts` is `None` the gatherer reports the company [`IdentityObservation::Foreign`]
/// — the same safe "never act" default the inert scaffold this replaces used,
/// and for the identical reason: fabricating an Owned/not-suppressed default
/// risks a wrongful actuation, which is strictly worse than a company that
/// stays idle until the shared store is configured.
pub struct HostCycleInputGatherer {
    // No `CompanyDb`. It was held to read the actuation record -- the
    // actuator's committed report -- and there is no report.
    facts: Option<ReconcilerFactsStore>,
    /// The runtime socket THIS chiefd daemon actuates against — compared against
    /// a runtime-owner claim to decide Owned vs Foreign.
    our_socket_name: String,
    /// The CompanyDb label used by the normalized runtime-owner row.  A shared
    /// `org.sqlite` is multiplexed by `documentKey(slug, dataRoot)`, while the
    /// manifest intentionally retains the human-facing bare slug; conflating
    /// the two makes every live owner claim look absent.
    runtime_owner_row_key: String,
}

impl HostCycleInputGatherer {
    /// Build the gatherer. `facts` is `None` when the shared facts store is
    /// not configured for this boot (see the type docs).
    #[must_use]
    pub fn new(
        facts: Option<ReconcilerFactsStore>,
        our_socket_name: impl Into<String>,
        runtime_owner_row_key: impl Into<String>,
    ) -> Self {
        Self {
            facts,
            our_socket_name: our_socket_name.into(),
            runtime_owner_row_key: runtime_owner_row_key.into(),
        }
    }
}

impl CycleInputGatherer for HostCycleInputGatherer {
    fn gather_cycle_input(
        &self,
        ctx: &DutyContext,
    ) -> BoxFuture<'_, Result<CycleInput, DutyError>> {
        // Capture owned/cheaply-cloned data up front (matching
        // `MailboxDeliverySink::deliver`'s pattern): the future must be
        // `'static`-capturing even though it is bound to `&self`'s lifetime,
        // and `rusqlite::Connection` (inside `facts`'s calls) is not `Sync`,
        // so nothing here is held across the `.await` boundary either.
        let snapshot = Arc::clone(&ctx.snapshot);
        let facts = self.facts.clone();
        let our_socket_name = self.our_socket_name.clone();
        let runtime_owner_row_key = self.runtime_owner_row_key.clone();

        Box::pin(async move {
            let ledgers = snapshot.ledgers();
            let manifest =
                organization::read(ledgers).map_err(|error| DutyError::new(error.to_string()))?;
            let identity = match &facts {
                Some(facts) => facts
                    .identity_observation(&runtime_owner_row_key, &manifest.slug, &our_socket_name)
                    .map_err(DutyError::new)?,
                None => {
                    IdentityObservation::Foreign { holder: "no-facts-store-configured".to_string() }
                }
            };

            let gather_ctx = CycleGatherContext { identity };
            gather_cycle_input(&gather_ctx).map_err(|error| DutyError::new(error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TOMBSTONE: this module's whole subject was the observation -- trusted vs
    // untrusted reports, lapsed leases, stale records, and the live/adopted/
    // unhealthy split derived from them. chiefd has no observation, so those
    // tests have no subject rather than a changed answer.
    //
    // What survives is the property they collectively defended, and it is now
    // assertable directly: chiefd must never render "I have no host facts" as
    // "I looked and found nothing".

    #[test]
    fn a_cycle_input_claims_nothing_about_what_is_running() {
        let ctx = CycleGatherContext { identity: IdentityObservation::Owned };
        let input = gather_cycle_input(&ctx).expect("gathering never depends on a host fact");
        assert!(
            input.audit.adopted.is_empty(),
            "chiefd may not call a process adopted; it cannot see one"
        );
        assert!(
            input.unhealthy.is_empty(),
            "nor unhealthy: with two people desired and no host facts at all, an empty live \
             set must NOT read as everybody is dead"
        );
        assert!(input.audit.live.is_empty(), "chiefd sees nobody, so it names nobody live");
    }
}
