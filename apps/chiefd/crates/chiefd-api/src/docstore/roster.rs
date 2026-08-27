//! `POST /v1/org/roster/desired` — the client-agnostic roster facts.
//!
//! # Why this route exists
//!
//! chiefd is the backend and it is client-agnostic: it publishes FACTS and does
//! not decide, or describe, how a client presents them. `chiefd-core` used to
//! violate that in one specific way: `desired_topology` answered "who should be
//! running" and then, in the same value, grouped those people into *sessions*,
//! *windows* and *panes*, named the windows, and dropped the departments that
//! would render empty. Everything after "who" is a display decision belonging to
//! the operator's terminal client, and a browser had to ignore all of it. That
//! planner is deleted (#751/P10) and this route is now the only answer.
//!
//! The route publishes only the half chiefd owns. The terminal client derives
//! its own session name (`org-<slug>`), its own windows (a department each,
//! empty ones skipped) and its own pane placement (a head sits in the parent
//! department's window) from these facts; a browser derives a list. Neither
//! derivation is chiefd's business, and neither appears here.
//!
//! # What is deliberately NOT in the response
//!
//! No session name, no window, no pane, no socket, no layout — and, the one
//! that is easy to miss, no `paneDepartmentId`. That last field looked like
//! data but was a placement DECISION: chiefd computed head-in-parent and
//! persisted its answer, so the stored value went stale the moment a
//! department was reparented. #751-P9 deleted the rule and both of its
//! columns, so there is no stored answer left to leak here. A client derives
//! placement from `isHeadOf` plus the department tree and is correct
//! immediately; `chiefd-core/src/runtime/roster/tests.rs` pins that the two
//! sides now agree, including across a reparent.
//!
//! `apps/chiefd/tests/wire-boundary` asserts that absence mechanically, over
//! the real response body. The repo's backend boundary guard scans file text
//! and cargo edges; it cannot inspect a wire shape, so the wire shape gets its
//! own assertion — and it lives outside these crates precisely because making
//! it requires naming the display technology this crate must not name.

use axum::extract::{Extension, Json};

use chiefd_core::runtime::roster::{project_desired_roster, DesiredRoster};

use super::org_slice::{failed, live, Refused, SlugRequest};
use super::router::SupervisionLiveSource;

/// Project the company's committed roster into client-agnostic facts.
///
/// The activity ledger is read, and it does not degrade into a warning: it
/// decides `desiredActive`. A response that quietly answered "nobody is
/// desired" because the ledger was unreadable would read to an actuator as a
/// mandate to stop the whole company, which is the fail-closed property
/// `trust.rs` exists to protect one layer down.
///
/// # The roster is fenced to the caller (B4)
///
/// This route names every person in the company and says which of them chiefd
/// wants running, so an unfenced answer is a full staff list. It is narrowed
/// by the same manifest narrowing the tree routes use.
///
/// THE ACTUATOR MUST STILL SEE ALL OF IT, and that is not an exception bolted
/// on here — the actuator authenticates as a SERVICE, names no person row, and
/// so has no fence. A read fence that resolved a person would have refused the
/// one caller this route exists to serve.
pub(crate) async fn org_roster_desired(
    Extension(supervision_live): Extension<Option<SupervisionLiveSource>>,
    crate::authn::middleware::Caller(caller): crate::authn::middleware::Caller,
    Json(req): Json<SlugRequest>,
) -> Result<Json<DesiredRoster>, Refused> {
    let source = live(supervision_live, &req.slug)?;
    let manifest = match source.company.org_manifest_read().await {
        Ok(Some((manifest, _))) => manifest,
        Ok(None) => {
            return Err(super::route_error::RouteError::not_found(
                "unknown-company",
                "company has no organization manifest",
            ))
        }
        Err(error) => return Err(failed(&error)),
    };
    let fence = super::disclosure_fence::disclosure_fence(&caller, &manifest)?;
    let narrowed = super::disclosure_fence::narrowed_manifest(&manifest, fence.as_deref());
    let view = narrowed.as_ref().unwrap_or(&manifest);
    let activity = source.company.activity_read().await.map_err(|error| failed(&error))?;
    Ok(Json(project_desired_roster(view, activity.as_ref().map(|(ledger, _)| ledger))))
}
