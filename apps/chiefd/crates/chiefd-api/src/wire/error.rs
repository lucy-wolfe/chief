//! The closed error taxonomy, wire half (plan §1).
//!
//! `chiefd_core::ChiefdError` is the semantic taxonomy the store layer
//! produces; it deliberately derives no `Serialize`. This module is its
//! explicit projection, so the two cannot drift by accident — a new core
//! variant fails to compile here until someone decides its wire shape.
//!
//! Rules encoded here:
//!
//! * **Closed.** Adding a variant is an API-freeze change. Every error the
//!   wire can carry is one of these six.
//! * **Never 200-with-failure.** A non-accepted result is an error status;
//!   [`WireError::http_status`] is the single mapping.
//! * **SQL `CHECK` violations never surface.** Limits like `ack_retry <= 1`
//!   are enforced in `validate(&Ledger)` and returned as
//!   `Refused{code:"ack-retry-exhausted", legalRoutes:[…]}` (plan §1). There
//!   is no `Internal`/`Sql`/`Other` variant to leak one into.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chiefd_core::{ChiefdError, Refusal};

/// Startup readiness tier (plan §7.2). Tier 0 is served the moment a
/// company's lane/receipt tables open; tier 1 after its recovery pass.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ReadinessTier {
    /// `activity.status`, `health`.
    Tier0,
    /// Everything else for that company.
    Tier1,
}

/// The six-variant closed taxonomy as it appears on the wire.
///
/// Serialized externally tagged by a `type` discriminant so the conformance
/// corpus can match on `{type, code}` alone (TESTING.md §2) while message copy
/// is snapshot-tested separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum WireError {
    /// chiefd deliberately declined, naming the routes that would have worked.
    #[serde(rename_all = "camelCase")]
    Refused {
        /// Stable machine code, e.g. `identity-echo-mismatch`.
        code: String,
        /// Human-facing sentence; snapshot-tested, never corpus-matched.
        message: String,
        /// What the caller could do instead. Always present, possibly empty.
        legal_routes: Vec<String>,
    },

    /// A fence lost: a runtime generation or lease token did not match. Both sides are
    /// reported so the caller can re-read and retry.
    #[serde(rename_all = "camelCase")]
    Conflict {
        /// Stable machine code, e.g. `claim-mismatch`. The conformance corpus
        /// matches conflicts on `{type, code}` exactly as it matches refusals
        /// (`conformance/FORMAT.md`), so a conflict without a code could not be
        /// replayed against Rust at all.
        code: String,
        /// What the caller presented.
        expected: String,
        /// What chiefd holds.
        actual: String,
    },

    /// chiefd waited the documented ladder or queue deadline and still could
    /// not proceed. Never minted without waiting — the core type's payload has
    /// a crate-private constructor (plan §5.2 item 2).
    #[serde(rename_all = "camelCase")]
    Busy {
        /// How long chiefd actually waited, in milliseconds. This is the
        /// wire-visible half of the proof: a `Busy` with `waitedMs: 0` would
        /// be the fail-fast regression, and is assertable by tests.
        waited_ms: u64,
        /// Which wait produced it (`mutation-queue`, `lease:<scope>`).
        site: String,
    },

    /// A fail-closed store could not be read (plan §5.5): the query, the lock,
    /// the transaction or the disk failed, and the data is NOT known to be
    /// damaged. Per-company isolation: one unreadable `chief.db` reports this
    /// for that company and never wedges the host (plan §7.2).
    #[serde(rename_all = "camelCase")]
    StoreFailure {
        /// The store name.
        store: String,
        /// Why it failed, rendered — the `rusqlite::Error` with its result
        /// codes, or the `Refusal` naming the invariant that broke.
        ///
        /// This field is the point of the kind. Without it the caller receives
        /// a store name and nothing else, the reason exists only on the
        /// daemon's stderr, and every operator sighting of this error is
        /// indistinguishable from every other one.
        cause: String,
    },

    /// A stored body did not decode. The narrow kind: the bytes are present
    /// and are not the value they must be, which is the only case where
    /// telling an operator their data is damaged is true.
    #[serde(rename_all = "camelCase")]
    Corrupt {
        /// The store name.
        store: String,
        /// Why the bytes did not decode, rendered.
        cause: String,
    },

    /// Not currently serving: chiefd is still in a startup tier, or the
    /// company's writer actor is quiescing for removal. In-flight `mutate`
    /// futures resolve to this rather than hanging (plan §2.1, §7.2).
    #[serde(rename_all = "camelCase")]
    Unavailable {
        /// Short, stable reason string (`starting`, `removing`, …).
        reason: String,
        /// Which tier is up, when the reason is `starting`. This carries the
        /// information plan §7.2 describes as `{status:"starting", tier}`
        /// without opening a seventh top-level error shape — §1's "every error
        /// the wire can carry is in this taxonomy" wins.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        tier: Option<ReadinessTier>,
    },
}

/// The stable discriminant strings, exposed so cross-track code (and the
/// corpus runner) never spells them as literals.
pub mod kind {
    /// [`super::WireError::Refused`].
    pub const REFUSED: &str = "Refused";
    /// [`super::WireError::Conflict`].
    pub const CONFLICT: &str = "Conflict";
    /// [`super::WireError::Busy`].
    pub const BUSY: &str = "Busy";
    /// [`super::WireError::StoreFailure`].
    pub const STORE_FAILURE: &str = "StoreFailure";
    /// [`super::WireError::Corrupt`].
    pub const CORRUPT: &str = "Corrupt";
    /// [`super::WireError::Unavailable`].
    pub const UNAVAILABLE: &str = "Unavailable";
}

impl WireError {
    /// Build a refusal.
    pub fn refused(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Refused { code: code.into(), message: message.into(), legal_routes: Vec::new() }
    }

    /// Builder form: attach the legal routes.
    #[must_use]
    pub fn with_routes(mut self, routes: impl IntoIterator<Item = String>) -> Self {
        if let Self::Refused { legal_routes, .. } = &mut self {
            *legal_routes = routes.into_iter().collect();
        }
        self
    }

    /// The tier-gated startup refusal (plan §7.2).
    #[must_use]
    pub fn starting(tier: ReadinessTier) -> Self {
        Self::Unavailable { reason: "starting".to_owned(), tier: Some(tier) }
    }

    /// The wire discriminant. Matches [`kind`].
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Refused { .. } => kind::REFUSED,
            Self::Conflict { .. } => kind::CONFLICT,
            Self::Busy { .. } => kind::BUSY,
            Self::StoreFailure { .. } => kind::STORE_FAILURE,
            Self::Corrupt { .. } => kind::CORRUPT,
            Self::Unavailable { .. } => kind::UNAVAILABLE,
        }
    }

    /// Why a store error happened, when this is one.
    #[must_use]
    pub fn cause(&self) -> Option<&str> {
        match self {
            Self::StoreFailure { cause, .. } | Self::Corrupt { cause, .. } => Some(cause),
            _ => None,
        }
    }

    /// The refusal code, when this is a refusal.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused { code, .. } => Some(code),
            _ => None,
        }
    }

    /// HTTP status. The single mapping in the system: a non-accepted result is
    /// an error status, never 200-with-failure (plan §1).
    ///
    /// * `Refused` → 422: the request was well-formed and chiefd declined.
    /// * `Conflict` → 409: re-read the fence and retry.
    /// * `Busy` → 429: chiefd waited; the caller may retry (idempotency keys
    ///   make that safe — plan §11-A1).
    /// * `StoreFailure` → 500: a fail-closed store could not be read; an
    ///   operator, not a retry, is the route.
    /// * `Corrupt` → 500: the same route, with the stronger claim that a
    ///   stored body did not decode.
    /// * `Unavailable` → 503: shims retry with the standard ladder.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Refused { .. } => 422,
            Self::Conflict { .. } => 409,
            Self::Busy { .. } => 429,
            Self::StoreFailure { .. } => 500,
            Self::Corrupt { .. } => 500,
            Self::Unavailable { .. } => 503,
        }
    }
}

impl From<&Refusal> for WireError {
    fn from(refusal: &Refusal) -> Self {
        Self::Refused {
            code: refusal.code.to_owned(),
            message: refusal.message.clone(),
            legal_routes: refusal.legal_routes.clone(),
        }
    }
}

impl From<Refusal> for WireError {
    fn from(refusal: Refusal) -> Self {
        Self::from(&refusal)
    }
}

impl From<&ChiefdError> for WireError {
    fn from(error: &ChiefdError) -> Self {
        // Exhaustive on purpose: a new core variant must be given a wire shape
        // here before it can compile, which is what keeps the taxonomy closed.
        match error {
            ChiefdError::Refused(refusal) => Self::from(refusal),
            ChiefdError::Conflict { code, expected, actual } => Self::Conflict {
                code: (*code).to_string(),
                expected: expected.clone(),
                actual: actual.clone(),
            },
            ChiefdError::Busy(proof) => Self::Busy {
                waited_ms: u64::try_from(proof.waited().as_millis()).unwrap_or(u64::MAX),
                site: proof.site().to_owned(),
            },
            ChiefdError::StoreFailure { store, cause } => {
                Self::StoreFailure { store: (*store).to_owned(), cause: cause.clone() }
            }
            ChiefdError::Corrupt { store, cause } => {
                Self::Corrupt { store: (*store).to_owned(), cause: cause.clone() }
            }
            // #105: absence is NOT corruption, and it is not a server fault —
            // the store was simply never written. It maps onto the existing
            // `Unavailable` shape with a stable reason rather than opening a
            // sixth top-level wire shape, keeping §1's closed taxonomy (and the
            // recorded request-schema snapshots) intact. If a caller ever needs
            // to distinguish it on the wire, that is a deliberate taxonomy
            // change with a snapshot regen, not a side effect of this fix.
            ChiefdError::Absent { store } => {
                Self::Unavailable { reason: (*store).to_owned(), tier: None }
            }
            ChiefdError::Unavailable { reason } => {
                Self::Unavailable { reason: (*reason).to_owned(), tier: None }
            }
        }
    }
}

impl From<ChiefdError> for WireError {
    fn from(error: ChiefdError) -> Self {
        Self::from(&error)
    }
}

/// The response body chiefd returns for any non-accepted result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    /// The taxonomy value.
    pub error: WireError,
}

impl From<WireError> for ErrorResponse {
    fn from(error: WireError) -> Self {
        Self { error }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refused_serializes_to_its_documented_shape() {
        let value = serde_json::to_value(
            WireError::refused("ack-retry-exhausted", "already retried once")
                .with_routes(["assignment.result".to_owned()]),
        );
        assert_eq!(
            value.ok(),
            Some(json!({
                "type": "Refused",
                "code": "ack-retry-exhausted",
                "message": "already retried once",
                "legalRoutes": ["assignment.result"],
            }))
        );
    }

    #[test]
    fn conflict_serializes_to_its_documented_shape() {
        let value = serde_json::to_value(WireError::Conflict {
            code: "lease-conflict".to_owned(),
            expected: "12".to_owned(),
            actual: "13".to_owned(),
        });
        assert_eq!(
            value.ok(),
            Some(json!({
                "type": "Conflict",
                "code": "lease-conflict",
                "expected": "12",
                "actual": "13"
            }))
        );
    }

    #[test]
    fn busy_serializes_with_the_wait_it_is_proof_of() {
        let value = serde_json::to_value(WireError::Busy {
            waited_ms: 4250,
            site: "lease:runtime".to_owned(),
        });
        assert_eq!(
            value.ok(),
            Some(json!({"type": "Busy", "waitedMs": 4250, "site": "lease:runtime"}))
        );
    }

    #[test]
    fn store_failure_serializes_to_its_documented_shape() {
        let value = serde_json::to_value(WireError::StoreFailure {
            store: "launch-intent".to_owned(),
            cause: "DatabaseBusy".to_owned(),
        });
        assert_eq!(
            value.ok(),
            Some(json!({
                "type": "StoreFailure",
                "store": "launch-intent",
                "cause": "DatabaseBusy",
            }))
        );
    }

    #[test]
    fn corrupt_serializes_to_its_documented_shape() {
        let value = serde_json::to_value(WireError::Corrupt {
            store: "launch-intent".to_owned(),
            cause: "expected value at line 1 column 1".to_owned(),
        });
        assert_eq!(
            value.ok(),
            Some(json!({
                "type": "Corrupt",
                "store": "launch-intent",
                "cause": "expected value at line 1 column 1",
            }))
        );
    }

    /// **The cause must survive the projection.** A store error whose `cause`
    /// arrives empty is the defect this field exists to remove, wearing a
    /// schema that looks fixed. Asserting the field EXISTS would pass against
    /// an empty string, so this asserts the actual sentence arrives.
    #[test]
    fn the_cause_travels_from_the_core_type_onto_the_wire() {
        let core = ChiefdError::StoreFailure {
            store: "activity",
            cause: "Refusal { code: \"unknown-person\" }".to_owned(),
        };
        let wire = WireError::from(&core);
        assert_eq!(wire.cause(), core.cause());
        assert_eq!(wire.cause(), Some("Refusal { code: \"unknown-person\" }"));

        let body = serde_json::to_value(&wire).expect("the wire error serializes");
        assert_eq!(
            body.get("cause").and_then(serde_json::Value::as_str),
            Some("Refusal { code: \"unknown-person\" }"),
            "the reason must reach the caller, not just the daemon's stderr"
        );
    }

    /// The two store kinds must stay distinguishable ON THE WIRE. They share a
    /// payload and a status, so a projection that collapsed one into the other
    /// would still deserialize, still return 500, and still be the defect: a
    /// lock timeout telling an operator their bytes are damaged.
    #[test]
    fn the_two_store_kinds_are_distinct_on_the_wire() {
        let generic = WireError::StoreFailure {
            store: "activity".to_owned(),
            cause: "DatabaseBusy".to_owned(),
        };
        let decoded =
            WireError::Corrupt { store: "activity".to_owned(), cause: "DatabaseBusy".to_owned() };
        assert_ne!(generic, decoded);
        assert_ne!(generic.kind(), decoded.kind());
        assert_ne!(serde_json::to_value(&generic).ok(), serde_json::to_value(&decoded).ok());
        assert_eq!(generic.http_status(), decoded.http_status());
    }

    #[test]
    fn unavailable_omits_tier_unless_starting() {
        let plain = serde_json::to_value(WireError::Unavailable {
            reason: "removing".to_owned(),
            tier: None,
        });
        assert_eq!(plain.ok(), Some(json!({"type": "Unavailable", "reason": "removing"})));

        let starting = serde_json::to_value(WireError::starting(ReadinessTier::Tier0));
        assert_eq!(
            starting.ok(),
            Some(json!({"type": "Unavailable", "reason": "starting", "tier": "tier0"}))
        );
    }

    #[test]
    fn every_taxonomy_variant_maps_to_an_error_status_never_200() {
        let all = [
            WireError::refused("x", "y"),
            WireError::Conflict { code: "c".into(), expected: "a".into(), actual: "b".into() },
            WireError::Busy { waited_ms: 1, site: "mutation-queue".into() },
            WireError::StoreFailure { store: "registry".into(), cause: "locked".into() },
            WireError::Corrupt { store: "registry".into(), cause: "bad json".into() },
            WireError::Unavailable { reason: "starting".into(), tier: None },
        ];
        for error in &all {
            assert!(error.http_status() >= 400, "{} must be an error status", error.kind());
        }
    }

    #[test]
    fn core_errors_project_onto_the_wire_taxonomy_without_loss() {
        let core = ChiefdError::refused("already-exists", "taken");
        let wire = WireError::from(&core);
        assert_eq!(wire.kind(), core.kind());
        assert_eq!(wire.code(), Some("already-exists"));

        let core =
            ChiefdError::StoreFailure { store: "removal-journal", cause: "locked".to_owned() };
        assert_eq!(WireError::from(core).kind(), kind::STORE_FAILURE);

        let core = ChiefdError::Corrupt { store: "removal-journal", cause: "bad json".to_owned() };
        assert_eq!(WireError::from(core).kind(), kind::CORRUPT);
    }

    #[test]
    fn error_response_wraps_the_taxonomy_under_a_single_key() {
        let body = serde_json::to_value(ErrorResponse::from(WireError::Conflict {
            code: "lease-conflict".into(),
            expected: "1".into(),
            actual: "2".into(),
        }));
        assert_eq!(
            body.ok(),
            Some(json!({
                "error": {
                    "type": "Conflict",
                    "code": "lease-conflict",
                    "expected": "1",
                    "actual": "2"
                }
            }))
        );
    }
}
