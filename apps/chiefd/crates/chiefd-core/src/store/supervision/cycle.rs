//! The D9 supervision cycle — **M15**.
//!
//! This module runs the cycle onto M12's supervision store. It carries what a
//! now-deleted TypeScript supervisor and its supervision module used to own, so
//! the comments below name the behaviour rather than the old files.
//!
//! # The order is the contract
//!
//! [`Stage`] records each remaining durable step in execution order. ChiefD no
//! longer observes client-owned tmux state, so this cycle does not compare a
//! desired projection with a runtime observation.
//!
//! # Compute-then-apply
//!
//! Every stage computes into a [`CycleReport`] and a draft; the draft is
//! published by [`super::mutate`] in one commit (inv 14), and a refusal from
//! any stage publishes nothing. There is no interleaving in which half a cycle
//! is durable.

use std::collections::BTreeSet;

use super::mutate;
use crate::ledger::Ledgers;
use crate::store::organization::OrganizationManifest;
use crate::ChiefdError;

/// The D9 stages, in the order they run.
///
/// Mirrors `chiefd_api::wire::SupervisionStage`. It is duplicated rather than
/// imported because `chiefd-core` does not depend on `chiefd-api` — the wire
/// crate depends on this one. `the_core_stage_list_matches_the_frozen_wire_\
/// contract` in `chiefd-api` asserts the two cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Fleet-suppression check; a suppressed company goes inert here.
    Suppression,
    /// Identity/ownership check.
    Identity,
    /// Fast health sample.
    FastHealth,
    /// runtime audit.
    RuntimeAudit,
    /// Reconcile: compute-then-apply.
    Reconcile,
}

impl Stage {
    /// The stage name as it appears on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Suppression => "suppression",
            Self::Identity => "identity",
            Self::FastHealth => "fastHealth",
            Self::RuntimeAudit => "runtimeAudit",
            Self::Reconcile => "reconcile",
        }
    }

    /// Every stage, in D9 order. The single source of the sequence.
    #[must_use]
    pub const fn d9_order() -> [Self; 5] {
        [Self::Suppression, Self::Identity, Self::FastHealth, Self::RuntimeAudit, Self::Reconcile]
    }
}

/// Who the identity check says owns this company right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IdentityObservation {
    /// This chiefd owns the company.
    #[default]
    Owned,
    /// Somebody else holds it. The cycle stops rather than acting on a
    /// company it does not own.
    Foreign {
        /// Who was observed holding it, for the warning.
        holder: String,
    },
}

/// What the runtime audit saw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeAuditObservation {
    /// People whose pane was adopted by this audit.
    pub adopted: Vec<String>,
    /// People observed to have a live pane.
    pub live: BTreeSet<String>,
    /// The audit could not run; its stage still records, and the cycle does
    /// not claim recovery it cannot prove.
    pub unavailable: bool,
}

/// Everything only the host can know, gathered before the cycle runs.
///
/// A data struct rather than a trait, following `activity::reconcile`: the
/// core may not touch runtime or the filesystem (clippy.toml enforces the seam),
/// and injected observations make the order test deterministic with no mock
/// framework and no `#[cfg(test)]` seam in production code.
#[derive(Debug, Clone, Default)]
pub struct CycleInput {
    /// Fleet-suppression verdict. `true` ⇒ the cycle goes inert.
    pub suppressed: bool,
    /// Who owns the company.
    pub identity: IdentityObservation,
    /// People the fast-health sample found unhealthy.
    pub unhealthy: Vec<String>,
    /// What the runtime audit saw.
    pub audit: RuntimeAuditObservation,
}

/// What one cycle did. Bounded — a report, never the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CycleReport {
    /// Stages that actually ran, in execution order.
    pub stages: Vec<Stage>,
    /// Panes adopted during the audit.
    pub adopted: Vec<String>,
    /// Non-fatal observations.
    pub warnings: Vec<String>,
}

impl CycleReport {
    /// Whether this cycle went INERT — it took one of the two early returns
    /// (`suppressed`, or an identity claim held by another chiefd) and so never
    /// reached the stages that advance durable state.
    ///
    /// od:idle-cpu #437 / #63 / #64: an inert cycle is a `Ok(report)` that
    /// wrote nothing, and it used to be indistinguishable in the log from a
    /// healthy converged pass — 4911 consecutive inert passes logged "cycle
    /// committed" while nothing converged. It is a WARN now.
    ///
    /// Keyed on the reconcile stage rather than on the input, so it stays true
    /// for any future early return that stops short of it: if the cycle did not
    /// reach reconcile, nothing moved, full stop.
    #[must_use]
    pub fn is_inert(&self) -> bool {
        !self.stages.contains(&Stage::Reconcile)
    }
}

/// Run one D9 cycle.
///
/// # Errors
/// Whatever the store refuses. A refusal publishes nothing: the draft is
/// dropped, so a failed cycle leaves durable state byte-identical.
pub fn cycle(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
    input: &CycleInput,
) -> Result<CycleReport, ChiefdError> {
    // Suppression is evaluated before any mutation is even opened. A
    // suppressed company must not take the writer's mutation path at all —
    // `mutate` reconciles the protected roster on entry, so merely opening one
    // would be a durable write by a company that is meant to be inert.
    if input.suppressed {
        return Ok(CycleReport {
            stages: vec![Stage::Suppression],
            warnings: vec![
                "fleet suppression is active: supervision went inert and wrote nothing".to_string()
            ],
            ..CycleReport::default()
        });
    }
    if let IdentityObservation::Foreign { holder } = &input.identity {
        return Ok(CycleReport {
            stages: vec![Stage::Suppression, Stage::Identity],
            warnings: vec![format!(
                "company is held by '{holder}', not this chiefd: supervision wrote nothing"
            )],
            ..CycleReport::default()
        });
    }

    mutate(ledgers, manifest, |_draft, _at| {
        let mut report = CycleReport {
            stages: vec![Stage::Suppression, Stage::Identity],
            ..CycleReport::default()
        };

        report.stages.push(Stage::FastHealth);
        for person_id in &input.unhealthy {
            report.warnings.push(format!("fast health: '{person_id}' is unhealthy"));
        }

        report.stages.push(Stage::RuntimeAudit);
        if input.audit.unavailable {
            report.warnings.push(
                "runtime audit was unavailable: no adoption decisions were made this cycle"
                    .to_string(),
            );
        }
        report.adopted.clone_from(&input.audit.adopted);

        // --- reconcile ----------------------------------------------------
        // `mutate` has already run `reconcile_protected` on entry (the ported
        // publish rule), so the roster is current here. The stage is recorded
        // because it genuinely ran, not as decoration.
        report.stages.push(Stage::Reconcile);

        Ok(report)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::WallMillis;
    use crate::store::{organization, supervision};
    use crate::test_support::northstar_manifest;

    const EPOCH: i64 = 1_784_116_800_000;

    #[test]
    fn no_runtime_observation_does_not_create_a_projection_warning() {
        let mut ledgers = Ledgers::empty(WallMillis(EPOCH));
        let manifest = northstar_manifest(EPOCH);
        organization::create(&mut ledgers, &manifest).expect("manifest");
        supervision::seed(&mut ledgers, &manifest).expect("supervision");

        let input = CycleInput {
            unhealthy: vec!["chief".to_owned()],
            audit: RuntimeAuditObservation { unavailable: true, ..Default::default() },
            ..Default::default()
        };
        let report = cycle(&mut ledgers, &manifest, &input).expect("cycle");

        assert_eq!(
            report.warnings,
            [
                "fast health: 'chief' is unhealthy",
                "runtime audit was unavailable: no adoption decisions were made this cycle",
            ],
            "real warnings remain, but missing client-owned runtime data is not a projection mismatch",
        );
        assert_eq!(
            report.stages,
            [
                Stage::Suppression,
                Stage::Identity,
                Stage::FastHealth,
                Stage::RuntimeAudit,
                Stage::Reconcile,
            ]
        );
    }
}
