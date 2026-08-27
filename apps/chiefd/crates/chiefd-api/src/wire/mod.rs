//! Frozen wire types — **M2, Track D. This module freezes cross-track
//! contracts.**
//!
//! Everything other tracks build against is here: the request and response
//! structs for every operation in plan §2, [`identity::CallerIdentity`], the
//! closed error taxonomy's wire form, and the `schemars` derivation the shim's
//! tool catalog is generated from. Changing a type here is a PR every track
//! owner reviews (plan §9).
//!
//! # The two field classes (plan §1)
//!
//! | Class | Rule | Where it shows |
//! |---|---|---|
//! | **Injected** | `requestedBy` and the caller's own identity are never in a request struct; they come from [`identity::CallerIdentity`] | [`identity::INJECTED_FIELDS`], asserted absent from every schema |
//! | **Stripped** | Fields the old CLI accepted and ignored are absent entirely, so `deny_unknown_fields` rejects them loudly | [`identity::STRIPPED_FIELDS`], asserted absent from every schema |
//!
//! A third class once existed — **attested-echo**, `personId` echoed in the
//! request and checked for equality — but its only member,
//! `readiness.receipt`, was deleted with the provider-readiness store, so the
//! class has no surface left and is gone.
//!
//! # Not-yet-frozen
//!
//! Plan §10 Q2 makes M3 (the live-traffic recording + the
//! `organization-intercom.ts` code pass) a freeze blocker for the **30
//! model-facing `org_*` tool** contracts. The types here are the *server*
//! surface, which is what M5/M6/M11/M13 compile against. Per-tool alias tables
//! and per-field tool shapes remain M3/M14's to pin, and the shim owns them by
//! design (plan §3.5) — the alias table is versioned separately from these
//! types precisely so pinning it later cannot reopen this freeze.

pub mod activity;
pub mod common;
pub mod comms;
pub mod company;
pub mod error;
pub mod identity;
pub mod maint;
pub mod org;

pub use common::{
    Accepted, Bounded, DepartmentId, IdempotencyKey, InvalidSlug, PersonId, Slug, Warning,
};
pub use error::{ErrorResponse, ReadinessTier, WireError};
pub use identity::{AttestedEcho, CallerChannel, CallerIdentity, CallerRole};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// TOMBSTONE: `VerbAuth` (`ManagerOnly` / `WorkerExempt`). It sat on every
// `OperationSpec` and was read by NOTHING outside `tests/` and a `#[cfg(test)]`
// module, so no request ever passed through it. It is deleted rather than
// wired, because `ManagerOnly`/`WorkerExempt` IS the job-title model that five
// merged PRs and three operator rulings removed in the same week: wiring it in
// would re-assert that model on the daemon's own enforcement path. A
// vocabulary tested only against another specification reads to a reviewer as
// a guarantee and is not one.
//
// The one rule it carried that was CORRECT — `maint.queue` names a TARGET, so
// a worker who could queue maintenance could queue a fresh session for anyone
// — is now a real caller check rather than a declaration. It lives at
// `session_maintenance_ops::queue` (`person_is_in_scope`, with
// `a_stranger_cannot_queue_maintenance_against_somebody_they_do_not_manage`
// and its two siblings) and at the route, which binds `requestedBy` to the
// authenticated caller. Both landed in #1084 before this husk was removed, so
// the rule was never homeless.

/// Which channels may issue an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChannelPolicy {
    /// Any authenticated channel, including a model tool call.
    Any,
    /// Shim- or human-issued only. Runtime-attested facts must never be
    /// model claims (plan §3.4) — there is no `org_ack` tool and there must
    /// not be one.
    AttestingOnly,
}

/// One operation's frozen metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationSpec {
    /// The wire operation name.
    pub op: &'static str,
    /// Which channels may issue it.
    pub channel: ChannelPolicy,
    /// Lowest startup tier at which it is served (plan §7.2). Tier-0 ops
    /// depend on nothing the recovery pass produces, and serving them during
    /// recovery is what keeps a chiefd restart from aborting 28 in-flight
    /// turns.
    pub tier: ReadinessTier,
}

/// An operation's request schema, for the shim's catalog and for the
/// cross-cutting schema tests.
#[derive(Debug, Clone)]
pub struct RequestSchema {
    /// The wire operation name.
    pub op: &'static str,
    /// The `schemars`-emitted JSON Schema of its request struct.
    pub schema: serde_json::Value,
}

macro_rules! wire_operations {
    ($(
        $op:literal => $req:ty, $channel:expr, $tier:expr;
    )*) => {
        /// Every operation in plan §2, with its frozen metadata.
        pub const OPERATIONS: &[OperationSpec] = &[
            $( OperationSpec { op: $op, channel: $channel, tier: $tier } ),*
        ];

        /// The `schemars` schema of every request struct, keyed by operation.
        ///
        /// Built at call time rather than as a const because `schemars` emits
        /// owned JSON. The shim merges the separately versioned description
        /// catalog onto these (plan §3.6).
        #[must_use]
        pub fn request_schemas() -> Vec<RequestSchema> {
            vec![
                $( RequestSchema {
                    op: $op,
                    schema: serde_json::to_value(schemars::schema_for!($req))
                        .unwrap_or(serde_json::Value::Null),
                } ),*
            ]
        }
    };
}

use activity::ActivityStatusRequest;
use comms::{
    EventEmitRequest, HealthRecordRequest, HealthResolveRequest, HealthStatusRequest,
    MsgDrainRequest, MsgSendRequest,
};
use company::{
    CompanyCreateRequest, CompanyListRequest, CompanyMaintenanceRequest, CompanyRef,
    CompanyTargetRequest,
};
use maint::{MaintFencedRequest, MaintQueueRequest, MaintStartRequest};
use org::{
    BenchRequest, ContractCloseRequest, ContractOpenRequest, DepartmentAddRequest,
    DepartmentLaunchRequest, DepartmentMoveRequest, DepartmentPauseRequest,
    DepartmentRemoveRequest, DepartmentStopRequest, HireRequest, PersonVerbRequest,
    TransferRequest,
};

use ChannelPolicy::Any;
use ReadinessTier::{Tier0, Tier1};

wire_operations! {
    // --- §2.1 registry & company lifecycle -------------------------------
    "company.create"           => CompanyCreateRequest,           Any, Tier1;
    "company.list"             => CompanyListRequest,             Any, Tier1;
    "company.show"             => CompanyRef,                     Any, Tier1;
    "company.tree"             => CompanyRef,                     Any, Tier1;
    "company.boot"             => CompanyTargetRequest,           Any, Tier1;
    // TOMBSTONE (chief-home-is-cwd §4c): `"company.ceo" =>
    // CompanyTargetRequest` was registered here — "bring this company's CEO
    // pane up". The operator client owns every pane, so no daemon verb can
    // mean that any more. `CompanyTargetRequest` survives: `company.boot`
    // still uses it.
    "company.resume"           => CompanyRef,                     Any, Tier1;
    "company.stop"             => CompanyRef,                     Any, Tier1;
    "company.compact"          => CompanyMaintenanceRequest,      Any, Tier1;
    "company.reset"            => CompanyMaintenanceRequest,      Any, Tier1;

    // --- §2.2 structure & staffing ---------------------------------------
    "org.department.add"       => DepartmentAddRequest,           Any, Tier1;
    "org.department.move"      => DepartmentMoveRequest,          Any, Tier1;
    "org.department.pause"     => DepartmentPauseRequest,         Any, Tier1;
    "org.department.resume"    => DepartmentPauseRequest,         Any, Tier1;
    "org.department.launch"    => DepartmentLaunchRequest,        Any, Tier1;
    "org.department.stop"      => DepartmentStopRequest,          Any, Tier1;
    "org.department.remove"    => DepartmentRemoveRequest,        Any, Tier1;
    "org.contract.open"        => ContractOpenRequest,            Any, Tier1;
    "org.contract.close"       => ContractCloseRequest,           Any, Tier1;
    "org.hire"                 => HireRequest,                    Any, Tier1;
    "org.bench"                => BenchRequest,                   Any, Tier1;
    "org.recall"               => PersonVerbRequest,              Any, Tier1;
    "org.transfer"             => TransferRequest,                Any, Tier1;
    "org.offboard"             => PersonVerbRequest,              Any, Tier1;
    // `org_roster` stays available to workers: it is part of the documented
    // worker recovery path (`organization-intercom.ts:3974`).
    "org.roster"               => CompanyRef,                     Any, Tier1;
    "org.list"                 => CompanyRef,                     Any, Tier1;
    "org.lifecycle_status"     => CompanyRef,                     Any, Tier1;
    "org.extension_drift"      => CompanyRef,                     Any, Tier1;

    // --- §2.4 activity ------------------------------------------------------
    "activity.status"          => ActivityStatusRequest,          Any, Tier0;

    // --- §2.5 session maintenance -----------------------------------------
    // `maint.queue` names a TARGET rather than the caller's own session, so a
    // caller who could queue maintenance could queue a fresh session for
    // anyone. That rule is real and it is ENFORCED, not declared: the route
    // binds `requestedBy` to the authenticated caller and
    // `session_maintenance_ops::queue` refuses a requester who does not manage
    // the target. This comment used to call it "manager-only", which was the
    // job-title vocabulary the tombstone at the top of this file retired —
    // twelve lines apart and contradicting each other. Authority here is the
    // subtree, like everywhere else. `auto_compact` needs no such check
    // because it is the person's own self-service path.
    "maint.queue"              => MaintQueueRequest,              Any, Tier1;
    "maint.auto_compact"       => MaintQueueRequest,              Any, Tier1;
    "maint.start"              => MaintStartRequest,              Any, Tier1;
    "maint.interrupt"          => MaintFencedRequest,             Any, Tier1;
    "maint.defer"              => MaintFencedRequest,             Any, Tier1;
    "maint.recover"            => MaintFencedRequest,             Any, Tier1;
    "maint.finish"             => MaintFencedRequest,             Any, Tier1;
    "maint.apply"              => MaintFencedRequest,             Any, Tier1;
    "maint.complete"           => MaintFencedRequest,             Any, Tier1;

    // --- §2.7 messaging, health, events ------------------------------------
    "msg.send"                 => MsgSendRequest,                 Any, Tier1;
    "msg.drain"                => MsgDrainRequest,                Any, Tier1;
    "health.status"            => HealthStatusRequest,            Any, Tier0;
    "health.record"            => HealthRecordRequest,            Any, Tier0;
    "health.resolve"           => HealthResolveRequest,           Any, Tier0;
    "events.emit"              => EventEmitRequest,               Any, Tier1;

}

/// Look up an operation's frozen metadata.
#[must_use]
pub fn operation(op: &str) -> Option<&'static OperationSpec> {
    OPERATIONS.iter().find(|spec| spec.op == op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn properties(schema: &Value) -> Vec<String> {
        schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn every_request_struct_denies_unknown_fields() {
        for entry in request_schemas() {
            assert_eq!(
                entry.schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "{} must derive #[serde(deny_unknown_fields)]",
                entry.op
            );
        }
    }

    #[test]
    fn no_request_struct_accepts_an_injected_field() {
        for entry in request_schemas() {
            for field in identity::INJECTED_FIELDS {
                assert!(
                    !properties(&entry.schema).iter().any(|name| name == field),
                    "{} declares the injected field {field}; it comes from CallerIdentity",
                    entry.op
                );
            }
        }
    }

    #[test]
    fn no_request_struct_accepts_a_stripped_field() {
        for entry in request_schemas() {
            for field in identity::STRIPPED_FIELDS {
                assert!(
                    !properties(&entry.schema).iter().any(|name| name == field),
                    "{} declares the stripped field {field}; it must be a schema error",
                    entry.op
                );
            }
        }
    }

    #[test]
    fn no_request_struct_echoes_the_callers_own_identity() {
        // The attested-echo field class was retired with `readiness.receipt`,
        // its only member. No surviving request may echo the caller's own
        // identity back — that is what `CallerIdentity` is for.
        let carriers: Vec<&str> = request_schemas()
            .iter()
            .filter(|entry| {
                let props = properties(&entry.schema);
                props.iter().any(|name| name == "callerPersonId")
            })
            .map(|entry| entry.op)
            .collect();
        assert!(
            carriers.is_empty(),
            "the attested-echo class is gone; these ops still echo identity: {carriers:?}"
        );
    }

    #[test]
    fn operation_names_are_unique() {
        let mut names: Vec<&str> = OPERATIONS.iter().map(|spec| spec.op).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate operation name in the registry");
    }

    #[test]
    fn tier0_is_exactly_the_ops_that_must_survive_a_restart_mid_turn() {
        let mut tier0: Vec<&str> = OPERATIONS
            .iter()
            .filter(|spec| spec.tier == ReadinessTier::Tier0)
            .map(|spec| spec.op)
            .collect();
        tier0.sort_unstable();
        assert_eq!(
            tier0,
            vec!["activity.status", "health.record", "health.resolve", "health.status",],
            "plan §7.2: a flat 503 gate would abort 28 in-flight turns"
        );
    }

    #[test]
    fn schemars_output_is_snapshotted_so_a_contract_change_shows_up_in_review() {
        // TESTING.md §7/M2. The snapshot is the frozen contract other tracks
        // build against: any diff here is a cross-track API change and needs
        // every track owner on the PR (plan §9).
        let registry: serde_json::Map<String, Value> = request_schemas()
            .into_iter()
            .map(|entry| (entry.op.to_owned(), entry.schema))
            .collect();
        insta::assert_json_snapshot!("request-schemas", Value::Object(registry));
    }

    #[test]
    fn operation_lookup_finds_registered_ops_and_rejects_others() {
        assert!(operation("company.create").is_some());
        assert!(operation("org_admin").is_none(), "org_admin is deleted (plan §3.3)");
    }
}
