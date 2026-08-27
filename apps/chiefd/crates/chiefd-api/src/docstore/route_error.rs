//! **The one place that decides what an HTTP status means.**
//!
//! # The defect this module exists to remove
//!
//! An agent told *"unavailable"* retries. An agent told *"not terminal"* acts.
//! Before this module the product said "unavailable" for both, because every
//! route family picked its own status and its own body:
//!
//! * `runtime_routes.rs` ran every `chiefd_host::runtime_lifecycle::*` result
//!   through a local `internal()` helper, so a
//!   `RuntimeLifecycleError::Store(ChiefdError::Refused(..))` — an unknown
//!   person, an unjustified thinking elevation — was answered **HTTP 500** and
//!   was indistinguishable from a dead daemon. An agent retrying against a
//!   refusal that will never open retries forever.
//! * `company_error` answered every `Refused` with **400 and a plain-text
//!   body**, so the refusal's machine code — the half a caller can branch on —
//!   was dropped on the floor before it left the process. Sibling mappers
//!   existed for no reason other than to hand-list codes that deserved a 422
//!   back.
//!
//! # The taxonomy
//!
//! **4xx is a refusal the caller can act on. 5xx is a genuine fault: chiefd
//! could not answer.** Nothing else. The status is chosen from the semantic
//! taxonomy (`chiefd_core::ChiefdError`), never from the route.
//!
//! | Status | Meaning | What the caller does |
//! |---|---|---|
//! | 400 | The request itself is malformed (unparseable field, bad query) | fix the request |
//! | 401 | The caller proved no identity | present a token, then ask again |
//! | 403 | The caller is authenticated and may not do this | stop asking |
//! | 404 | The named thing is not here (no live company, unknown task) | ask elsewhere / create it |
//! | 409 | A fence or a CAS was lost — chiefd's view moved under you | re-read, then retry |
//! | 422 | Well-formed, healthy daemon, and a **product rule said no** | act on the rule; NEVER retry |
//! | 429 | chiefd waited the documented ladder and could not proceed | back off and retry |
//! | 500 | chiefd faulted (store failure, corrupt bytes, dead writer) | an operator, not a retry |
//! | 503 | chiefd is not currently serving (starting, no host capability) | retry later |
//!
//! [`REFUSAL_STATUSES`] is the closed set of statuses that carry an actionable
//! `{code, detail}` refusal body. `scripts/test/refusal-taxonomy.test.mjs`
//! asserts that `packages/chiefing/src/resources/OrgRoutes.ts` accepts exactly
//! this set, so the two halves of the contract cannot drift.
//!
//! # The body
//!
//! Always `{"code": "...", "detail": "..."}` — the shape the shared TypeScript
//! decoder (`decodeRefusal`) already reads, and the shape most of `router.rs`
//! was already emitting by hand. `code` is the stable branch point; `detail` is
//! the human half. A plain-text error body is not producible from this module:
//! [`RouteError`] has no constructor that omits a code.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use chiefd_core::error::ChiefdError;

use crate::wire::WireError;

/// The statuses whose body is an actionable `{code, detail}` refusal rather
/// than a report that chiefd could not answer.
///
/// Mirrored verbatim by `REFUSAL_STATUSES` in
/// `packages/chiefing/src/resources/OrgRoutes.ts`; the guard
/// `scripts/test/refusal-taxonomy.test.mjs` fails if the two ever differ.
///
/// 429 is deliberately NOT here: `Busy` is chiefd reporting that it waited and
/// could not proceed, which is a retry instruction, not a product rule. 503 and
/// 500 are not here for the same reason — they are the two "chiefd could not
/// answer" statuses this whole module exists to keep distinct from a refusal.
///
/// 401 IS here (#751/P7). An unauthenticated caller that reads "chiefd is
/// unavailable" is told nothing about the one thing it must fix, and a retry
/// ladder hands it the identical refusal every time.
pub const REFUSAL_STATUSES: [u16; 6] = [400, 401, 403, 404, 409, 422];

/// The liveness probe's two answers.
///
/// `GET /v1/docs/health` is not classifying a failure it was handed — it is
/// reporting whether chiefd is serving at all — but "not serving" is a
/// taxonomy word, and it is answered with the taxonomy's own 503 rather than
/// with a status that route picked for itself. Nothing else in the docstore
/// surface names a status outside this module.
pub const HEALTH_SERVING: StatusCode = StatusCode::OK;
/// See [`HEALTH_SERVING`].
pub const HEALTH_NOT_SERVING: StatusCode = StatusCode::SERVICE_UNAVAILABLE;

/// One non-accepted result, on its way out of a route.
///
/// Construct it through one of the named constructors below — each one names
/// the CLASS of outcome, so a route can never quietly decide that a product
/// rule is a server fault. There is deliberately no
/// `RouteError::new(status, ..)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteError {
    status: StatusCode,
    code: String,
    detail: String,
}

impl RouteError {
    /// The request was well formed, chiefd is healthy, and a **product rule
    /// declined**. The caller must act on `code`, never retry.
    pub fn refused(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::UNPROCESSABLE_ENTITY, code, detail)
    }

    /// A fence or compare-and-set was lost: chiefd's committed view moved under
    /// the caller. Re-read, then retry.
    pub fn conflict(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::CONFLICT, code, detail)
    }

    /// The named thing is not served here: no live company for this slug, an
    /// unknown task id, a person this process does not run.
    pub fn not_found(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::NOT_FOUND, code, detail)
    }

    /// The request itself could not be understood — an unparseable embedded
    /// document, a filter chiefd cannot compile. Not a rule; a malformed ask.
    pub fn malformed(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::BAD_REQUEST, code, detail)
    }

    /// The caller proved no identity at all. It must present a credential and
    /// ask again — which is an ACTION, and why this is a refusal rather than an
    /// outage: a retry ladder against a missing token refuses identically every
    /// time and never says why.
    pub fn unauthenticated(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::UNAUTHORIZED, code, detail)
    }

    /// The caller is who they say they are and still may not do this. Distinct
    /// from [`Self::refused`] only in that the bar is the caller's IDENTITY
    /// rather than the state of the thing being asked about.
    pub fn forbidden(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::FORBIDDEN, code, detail)
    }

    /// chiefd waited the documented ladder or queue deadline and still could
    /// not proceed. Back off and retry.
    pub fn busy(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::TOO_MANY_REQUESTS, code, detail)
    }

    /// chiefd is not currently serving this: a startup tier, a quiescing
    /// writer, a capability this process was not assembled with. Retry later.
    pub fn unavailable(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::SERVICE_UNAVAILABLE, code, detail)
    }

    /// chiefd faulted. A store failure, a corrupt body, a dead writer —
    /// something an operator has to look at, and never a retry.
    pub fn fault(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::at(StatusCode::INTERNAL_SERVER_ERROR, code, detail)
    }

    /// The whole `ChiefdError` taxonomy in one projection.
    ///
    /// Status and code come from [`WireError`], which is the crate's declared
    /// single mapping (`wire/error.rs`), so the docstore surface and the wire
    /// surface cannot disagree about what a `Refused` is worth.
    #[must_use]
    pub fn from_chiefd(error: &ChiefdError) -> Self {
        let wire = WireError::from(error);
        let status =
            StatusCode::from_u16(wire.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // Both halves come off the WIRE projection, never off `ChiefdError`'s
        // `Display`. `Display` for a refusal renders `refused: <code>: <msg>`,
        // and putting that in `detail` next to `code` produced the doubled
        // sentence an agent actually read:
        //   `org row refused: launcher-root-unusable: refused:
        //    launcher-root-unusable: The launcher root …`
        let (code, detail) = match &wire {
            WireError::Refused { code, message, .. } => (code.clone(), message.clone()),
            WireError::Conflict { code, expected, actual } => {
                (code.clone(), format!("expected {expected}, actual {actual}"))
            }
            WireError::Busy { waited_ms, site } => {
                ("busy".to_owned(), format!("waited {waited_ms}ms at {site}"))
            }
            // `detail` is the only half of the body a human reads, and for a
            // store error it used to be the store name alone — the operator
            // read `corrupt store: activity` and had nowhere to go next. The
            // carried cause goes here for the same reason a refusal's `message`
            // does: this is the field that says what happened.
            WireError::StoreFailure { store, cause } => {
                ("store-failure".to_owned(), format!("store failure: {store}: {cause}"))
            }
            WireError::Corrupt { store, cause } => {
                ("corrupt-store".to_owned(), format!("corrupt store: {store}: {cause}"))
            }
            WireError::Unavailable { reason, .. } => ("unavailable".to_owned(), reason.clone()),
        };
        Self { status, code, detail }
    }

    /// The HTTP status this error will be answered with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable machine code the caller branches on.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The human half.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// True when this carries an actionable refusal rather than a report that
    /// chiefd could not answer. See [`REFUSAL_STATUSES`].
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        REFUSAL_STATUSES.contains(&self.status.as_u16())
    }

    fn at(status: StatusCode, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { status, code: code.into(), detail: detail.into() }
    }
}

impl IntoResponse for RouteError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "code": self.code, "detail": self.detail })))
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_product_rule_is_a_refusal_and_never_a_fault() {
        let error = RouteError::from_chiefd(&ChiefdError::refused(
            "head-needs-successor",
            "a department head cannot be offboarded without a successor",
        ));
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code(), "head-needs-successor");
        // The detail is the refusal's own sentence, NOT `ChiefdError`'s
        // `Display`, which would repeat the code inside it.
        assert_eq!(error.detail(), "a department head cannot be offboarded without a successor");
        assert!(error.is_refusal());
    }

    #[test]
    fn a_corrupt_store_is_a_fault_and_never_a_refusal() {
        let error = RouteError::from_chiefd(&ChiefdError::Corrupt {
            store: "supervision",
            cause: "expected value at line 1 column 1".to_owned(),
        });
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!error.is_refusal());
    }

    /// **The route body must say WHY.** `detail` is the only half a human
    /// reads, and a store error whose detail is the store name alone is the
    /// #1031 sighting: same sentence every time, no cause, nothing to act on.
    /// This asserts the reason's own words arrive, not merely that the field
    /// is populated — an assertion on presence passes against an empty cause.
    #[test]
    fn a_store_faults_route_body_carries_the_reason_it_failed() {
        let error = RouteError::from_chiefd(&ChiefdError::StoreFailure {
            store: "activity",
            cause: "Refusal { code: \"unknown-person\", message: \"Unknown activity person \
                    'ghost-person'\" }"
                .to_owned(),
        });
        assert_eq!(error.code(), "store-failure");
        assert!(error.detail().contains("activity"), "{}", error.detail());
        assert!(error.detail().contains("unknown-person"), "{}", error.detail());
        assert!(error.detail().contains("ghost-person"), "{}", error.detail());
    }

    #[test]
    fn a_lost_fence_is_a_conflict_the_caller_re_reads() {
        let error = RouteError::from_chiefd(&ChiefdError::conflict("claim-mismatch", "12", "13"));
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert!(error.is_refusal());
    }

    #[test]
    fn an_unavailable_daemon_is_not_a_refusal() {
        let error = RouteError::from_chiefd(&ChiefdError::Unavailable { reason: "starting" });
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!error.is_refusal());
    }

    /// The two halves of the taxonomy, stated as the property rather than as a
    /// list: every refusal status is 4xx, and no 5xx is ever a refusal.
    #[test]
    fn every_refusal_status_is_a_4xx_and_no_fault_status_is_a_refusal() {
        for status in REFUSAL_STATUSES {
            assert!((400..500).contains(&status), "{status} is not a 4xx");
        }
        for error in [
            RouteError::fault("x", "y"),
            RouteError::unavailable("x", "y"),
            RouteError::busy("x", "y"),
        ] {
            assert!(!error.is_refusal(), "{} must not read as a refusal", error.status());
        }
    }

    #[test]
    fn the_body_always_carries_the_code_and_the_detail() {
        let response = RouteError::refused("not-terminal", "task 'ledger-1' is not terminal yet");
        assert_eq!(response.code(), "not-terminal");
        assert_eq!(response.detail(), "task 'ledger-1' is not terminal yet");
    }
}
