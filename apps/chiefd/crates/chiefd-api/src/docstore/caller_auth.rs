//! Bind a request's DECLARED staffing requester to the CRYPTOGRAPHICALLY
//! AUTHENTICATED caller.
//!
//! # The gap this closes
//!
//! Every staffing route reads its requester out of the request BODY
//! (`requester: { kind, personId }`) and then checks that body against itself —
//! that `hiringManagerPersonId` equals the declared requester, that an operator
//! carries no person id, and so on. Those are consistency checks, not
//! authentication: nothing tied the claim to whoever was actually calling.
//!
//! The attestation that made the claim trustworthy lived entirely in
//! TypeScript, in `apps/cli`'s `resolveAtomicStaffingRequester` — it compared
//! `--requester-person` against chiefd's own `ORG_LAUNCHER_PERSON` pane
//! injection and refused a mismatch. That gate is real, and it is also
//! bypassable by construction: it runs in ONE client. `apps/api`, the intercom
//! extension, and plain `curl` all reach the same routes without passing
//! through it, so any caller who could reach the daemon could name any manager
//! as the requester of a hire. A rule enforced in one of several clients is not
//! enforced.
//!
//! Mandate 3 puts the decision here, where the data is.
//!
//! # What "authenticated" means at this seam
//!
//! There is no such thing as an absent caller here any more. The middleware's
//! pass-through branch is gone and the `Caller` extractor is what a handler
//! takes, so reaching this function at all means enforcement ran AND passed:
//! a bad or missing credential is answered with 401/403 before a handler
//! body ever starts. That is why the parameter is `&Identity` and not
//! `Option<&Identity>` — the optional form used to carry a rollout rule
//! ("absence is local trust") that no longer describes any deployment, and an
//! unreachable arm that still reads as policy is worse than no arm.
//!
//! That is the whole point: this cannot be weakened by omitting a header,
//! because omitting the header is what the middleware rejects.

use super::route_error::RouteError;

use chiefd_core::store::identities::{Identity, IdentityKind};

/// Refuse when an authenticated caller's declared staffing requester is not
/// its own identity.
///
/// `declared_person_id` is the requester the body claims, already parsed:
/// `Some(person)` for a person requester, `None` for a direct operator.
///
/// # Errors
/// `403 requester-identity-mismatch` when a person-authenticated caller claims
/// a different person, or claims operator; `403 operator-requester-forbidden`
/// when a person-authenticated caller claims the operator route.
pub(crate) fn bind_requester_to_caller(
    caller: &Identity,
    declared_person_id: Option<&str>,
    company_slug: &str,
) -> Result<(), RouteError> {
    match caller.kind {
        IdentityKind::Person => {
            // A person may act only as itself. `principal` is the field
            // authorization keys on.
            let Some(declared) = declared_person_id else {
                return Err(refuse(
                    "operator-requester-forbidden",
                    format!(
                        "caller '{}' is a person identity and cannot act as the direct operator",
                        caller.principal
                    ),
                ));
            };
            if declared != caller.principal {
                return Err(refuse(
                    "requester-identity-mismatch",
                    format!(
                        "caller '{}' declared requester '{declared}'; a person may act only as itself",
                        caller.principal
                    ),
                ));
            }
            // A person identity is company-scoped, and a person from one
            // company must never staff another.
            if caller.company_slug.as_deref().is_some_and(|slug| slug != company_slug) {
                return Err(refuse(
                    "requester-company-mismatch",
                    format!(
                        "caller '{}' belongs to a different company than '{company_slug}'",
                        caller.principal
                    ),
                ));
            }
            Ok(())
        }
        // Operator / service / channel identities are daemon-scoped
        // and may act as the operator. They may NOT impersonate a person: a
        // manager-attributed hire inherits that manager's model route and is
        // recorded against them in the audit trail.
        IdentityKind::Operator | IdentityKind::Service | IdentityKind::Channel => {
            match declared_person_id {
                None => Ok(()),
                Some(declared) => Err(refuse(
                    "requester-identity-mismatch",
                    format!(
                        "caller '{}' is not a person identity and cannot act as person '{declared}'",
                        caller.principal
                    ),
                )),
            }
        }
    }
}

/// Refuse when an authenticated caller writes into a company that is not its
/// own.
///
/// The binding for a route that names NO requester and NO person target — the
/// company-wide ledger writes. `/v1/org/event-journal/{insert-if-absent,prune}`
/// are the two: they are DocStore-direct on the shared `org.sqlite` with no
/// live-company gate at all (that is deliberate — an exactly-once marker is a
/// cross-producer primitive written before any company is "live"), so the
/// `slug` in the body is the ONLY thing that decides which company's journal is
/// touched, and nothing ever compared it to the caller.
///
/// There is nothing to `bind_requester_to_caller` here: neither request carries
/// a requester, and ADDING a body field so one could be bound is exactly the
/// second `requested_by` §2 of the design record warns about — a value
/// that reads as bound and is supplied by the caller it claims to authenticate.
/// What the caller does carry is a company, and a company is a real fence.
///
/// A person identity is company-scoped, so its `company_slug` must match.
/// Operator, service and channel identities are DAEMON-scoped and carry no
/// company; a daemon-scoped credential is already trusted across the companies
/// this process serves, so there is nothing to compare and nothing to refuse.
/// `company_slug` is the COMPOSITE document key the routes take in `slug`, not
/// the display slug — the same value `bind_requester_to_caller` is given.
///
/// # Errors
/// `403 caller-company-mismatch` when a person-authenticated caller names a
/// different company than its own.
pub(crate) fn bind_caller_to_company(
    caller: &Identity,
    company_slug: &str,
) -> Result<(), RouteError> {
    if caller.company_slug.as_deref().is_some_and(|slug| slug != company_slug) {
        return Err(refuse(
            "caller-company-mismatch",
            format!(
                "caller '{}' belongs to a different company than '{company_slug}'",
                caller.principal
            ),
        ));
    }
    Ok(())
}

fn refuse(code: &str, detail: String) -> RouteError {
    RouteError::forbidden(code, detail)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    fn identity(kind: IdentityKind, principal: &str, company: Option<&str>) -> Identity {
        Identity {
            identity_id: format!("id-{principal}"),
            principal: principal.to_owned(),
            kind,
            company_slug: company.map(str::to_owned),
            pubkey: Some("spki".to_owned()),
            fingerprint: "fp".to_owned(),
            active: true,
            enrolled_at: 0,
            enrolled_by: Some("test".to_owned()),
            revoked_at: None,
        }
    }

    #[test]
    fn a_person_may_act_as_itself() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));
        assert!(bind_requester_to_caller(&caller, Some("engineering-head"), "cobalt").is_ok());
    }

    /// THE defect. Attestation lived in one TypeScript client, so any other
    /// caller — apps/api, the intercom, curl — could name any manager as the
    /// requester of a hire and inherit that manager's authority.
    #[test]
    fn a_person_cannot_hire_as_a_different_manager() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));

        let refused = bind_requester_to_caller(&caller, Some("research-head"), "cobalt")
            .expect_err("impersonating another manager must be refused");

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(refused.code(), "requester-identity-mismatch");
    }

    /// The operator route carries different rules (explicit provider/model, no
    /// inherited manager route), so a person taking it is an escalation.
    #[test]
    fn a_person_cannot_claim_the_operator_route() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));

        let refused = bind_requester_to_caller(&caller, None, "cobalt")
            .expect_err("a person claiming operator must be refused");

        assert_eq!(refused.code(), "operator-requester-forbidden");
    }

    /// A person identity is company-scoped; staffing a different company is a
    /// cross-tenant write.
    #[test]
    fn a_person_cannot_staff_another_company() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));

        let refused = bind_requester_to_caller(&caller, Some("engineering-head"), "northstar")
            .expect_err("cross-company staffing must be refused");

        assert_eq!(refused.code(), "requester-company-mismatch");
    }

    /// THE POSITIVE CASE for the company binding, and it is the one that keeps
    /// the refusal below honest: a guard that refused everybody would satisfy
    /// the negative on its own, and the event-journal marker the intercom
    /// writes on every organization event travels this path.
    #[test]
    fn a_person_may_write_its_own_companys_journal() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));
        assert!(bind_caller_to_company(&caller, "cobalt").is_ok());
    }

    /// The event-journal routes are DocStore-direct on the shared `org.sqlite`
    /// with NO live-company gate, so the body's `slug` is the only thing that
    /// chose the company — and until now nothing compared it to the caller.
    #[test]
    fn a_person_may_not_write_another_companys_journal() {
        let caller = identity(IdentityKind::Person, "engineering-head", Some("cobalt"));

        let refused = bind_caller_to_company(&caller, "northstar")
            .expect_err("a cross-company journal write must be refused");

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(refused.code(), "caller-company-mismatch");
    }

    /// Operator, service and channel identities are DAEMON-scoped: they carry
    /// no company, so there is nothing to compare. The resident actuator holds
    /// one of these, and a fence that refused it would take the daemon's own
    /// bookkeeping offline.
    #[test]
    fn a_daemon_scoped_identity_carries_no_company_and_is_not_fenced() {
        for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
            let caller = identity(kind, "operator", None);
            assert!(
                bind_caller_to_company(&caller, "cobalt").is_ok(),
                "{kind:?} is daemon-scoped and names no company"
            );
        }
    }

    #[test]
    fn a_daemon_identity_may_act_as_the_operator_but_never_as_a_person() {
        for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
            let caller = identity(kind, "operator", None);
            assert!(
                bind_requester_to_caller(&caller, None, "cobalt").is_ok(),
                "{kind:?} must be allowed the operator route"
            );
            let refused = bind_requester_to_caller(&caller, Some("engineering-head"), "cobalt")
                .expect_err("a daemon identity must not impersonate a person");
            assert_eq!(refused.code(), "requester-identity-mismatch");
        }
    }
}
