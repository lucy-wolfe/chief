//! The axum router and tiered readiness — **owned by M6 onward (Track D)**.
//!
//! chiefd serves HTTP/1.1 over a unix domain socket, plus TCP
//! `127.0.0.1:8791` during migration only (plan §0, §8 Phase 1).
//!
//! What lands here:
//!
//! * TOMBSTONE (plan §7.1): `GET /v1/health` was to answer
//!   `{status, pid, startedAt, schemaVersion, buildHash, launcherRoot,
//!   companies:[{slug, state, tier}]}`, so `chiefctl daemon ensure` could
//!   treat a build-hash mismatch as a hard error. It was never wired: no
//!   route in this crate ever served it and no client ever read it, and the
//!   `DaemonHealthResponse`/`CompanyHealth`/`DaemonStatus` types that spelled
//!   it are deleted with this note. Health as it actually exists is
//!   `GET /v1/docs/health` (`docstore::router`), which `chief ls` probes; the
//!   tiered-readiness rules below are live and unaffected.
//! * **Tiered readiness** (plan §7.2). A flat 503 gate would abort the
//!   in-flight turn of all 28 panes on every restart. So chiefd binds
//!   immediately and serves in tiers:
//!   - **Tier 0**, as soon as a company's lane/receipt tables open and before
//!     any runtime audit: `activity.status`, `health` (#748 retired the
//!     provider-admission pool from the tier-0 set).
//!   - **Tier 1**, after that company's recovery pass: everything else.
//!
//!   Not-yet-ready ops answer `503 {status:"starting", tier}`.
//! * **Per-company isolation**: one company failing `integrity_check` reports
//!   a store error for itself and the rest keep serving. A corrupt database
//!   never wedges the host.
//! * Verb-level authorization applied per call against freshly loaded state —
//!   never per-role tool registration, which would delay promotions until
//!   fresh-session and leave demoted managers holding manager tools
//!   (plan §3.2).
