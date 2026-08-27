//! `chiefd-api` — the wire surface.
//!
//! HTTP/1.1 over a unix domain socket (`~/.local/share/tribe-launcher/chiefd.sock`,
//! override `$CHIEFD_SOCKET`), plus the TCP typed docstore on `127.0.0.1:8792`.
//! This crate owns the request/response types, their `schemars` derivation,
//! and the axum router.
//!
//! Module ownership (plan §9, Track D):
//!
//! | Module | Owns | Milestone |
//! |---|---|---|
//! | [`socket`] | Socket/session resolution, the `--socket`/`--session` pair rule (D17) | M1 |
//! | [`wire`] | Frozen request/response types, `CallerIdentity`, the error taxonomy's wire form | **M2 — landed** |
//! | [`server`] | The axum router, tiered readiness gating, `GET /v1/health` | M6+ |
//! | [`docstore`] | The typed `org_documents` surface (one-daemon lane od-store) — the ONLY durable store post-Phase-B | one-daemon |
//! | [`auth`] | Caller identity assembly, disk authority, echo checks, the verb table | **M13 — landed** |
//!
//! # Contract freeze
//!
//! M2 froze the cross-track contracts in [`wire`]. Changing a type there is now
//! a PR that every track owner reviews (plan §9), and the `schemars` snapshot
//! test makes any such change visible in the diff. Three field classes exist
//! and the type definitions are what distinguish them: **injected**
//! (`requestedBy`, never in a request struct) and **stripped** (fields the old
//! CLI accepted and ignored — absent). A third class, **attested-echo**
//! (`personId` echoed and checked for equality against
//! `CallerIdentity`), had one wire member — `readiness.receipt` — which was
//! deleted with the provider-readiness store, so no request carries it now; the
//! `verify_echo` check that enforced it is retained for future echo ops.
//!
//! One M2 acceptance criterion is **not** met by this crate and cannot be:
//! TESTING.md §7/M2 requires M3's sign-off that the D12 tool contracts are
//! pinned from recorded live traffic before M2 is "done". These types are the
//! server surface the other tracks compile against; the per-tool shapes and the
//! shim's alias table are versioned separately (plan §3.5) so pinning them
//! later does not reopen this freeze.

#![forbid(unsafe_code)]

pub mod authn;
pub mod docstore;
pub mod server;
pub mod socket;
pub mod wire;
