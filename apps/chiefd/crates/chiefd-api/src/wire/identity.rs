//! `CallerIdentity` and the field classes (plan §1, §6).
//!
//! # The field classes
//!
//! 1. **Injected** — `requestedBy` and friends. *Never* in a request struct;
//!    supplied from [`CallerIdentity`]. A grep-style test in
//!    [`crate::wire`] asserts no request schema carries one.
//! 2. **Stripped** — fields the old CLI accepted and ignored. Absent
//!    entirely; `deny_unknown_fields` turns a sender that still includes one
//!    into a schema error rather than a silent no-op.
//! 3. **Attested-echo** — `personId` echoed in the
//!    request and validated for *equality* against [`CallerIdentity`] (the
//!    `src/cli.ts:650-668` matrix); mismatch is
//!    `Refused{code:"identity-echo-mismatch"}`. This preserves the cross-check
//!    that a Pi materialized against a stale checkout or the wrong data root
//!    cannot write runtime-attested facts for the company it merely *names*.
//!    Its one wire member, `readiness.receipt`, was deleted with the
//!    provider-readiness store, so no request struct currently carries the pair
//!    — but [`AttestedEcho`] and [`CallerIdentity::verify_echo`] are kept: the
//!    check is auth machinery that outlives the single verb, and the
//!    conformance corpus shows other echo ops.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chiefd_core::Refusal;

use super::common::{PersonId, Slug};

/// Refusal code for an attested-echo mismatch (plan §1).
pub const IDENTITY_ECHO_MISMATCH: &str = "identity-echo-mismatch";

/// Refusal code for the disk-authority check (plan §1, §6.4).
pub const ORG_DIR_MISMATCH: &str = "org-dir-mismatch";

/// Refusal code for a worker attempting a manager-only verb (plan §3.2).
pub const MANAGER_ONLY: &str = "manager-only";

/// Refusal code for a model-issued call on a shim-attested-only verb
/// (plan §3.4: acknowledgement is a launcher-attested fact, never a model
/// claim).
pub const SHIM_ATTESTED_ONLY: &str = "shim-attested-only";

/// Field names that are **injected**, never accepted on the wire. The
/// cross-cutting schema test asserts no request struct declares one.
pub const INJECTED_FIELDS: &[&str] = &["requestedBy", "callerPersonId", "uid"];

/// Field names the old CLI accepted and ignored — **stripped**. They are
/// absent from every request struct, so `deny_unknown_fields` rejects them
/// loudly instead of silently doing nothing.
pub const STRIPPED_FIELDS: &[&str] = &["organizationSlug", "dryRun", "verbose", "json", "quiet"];

/// Which authorization class the caller holds for verb-level checks. Ported
/// from `commandRequiresOrganizationManager`
/// (`src/organization/org-caller-auth.ts:52-70`).
///
/// Note (plan §3.2): this is **not** derived from the registered toolset.
/// Everyone is registered the union of tools and the server checks the verb
/// per call against a freshly loaded manifest — which is exactly why
/// `org.transfer`/promotion/demotion take effect on the very next call, and
/// why a demoted manager does not keep manager authority until fresh-session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CallerRole {
    /// A worker. Limited to the worker-exempt verb list (plan §3.2).
    Worker,
    /// A manager or executive. May issue structural and executive verbs.
    Manager,
}

impl CallerRole {
    /// Whether this role satisfies a manager-only verb.
    #[must_use]
    pub fn is_manager(self) -> bool {
        matches!(self, Self::Manager)
    }
}

/// How the call reached chiefd. Load-bearing: acknowledgement and progress are
/// runtime-attested facts the shim issues, and must never be reachable as a
/// model tool (plan §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CallerChannel {
    /// A model tool call, relayed by the shim on the model's behalf.
    Model,
    /// The shim itself, attesting a runtime fact.
    Shim,
    /// `chiefctl` — humans and CI. Not reachable from managed agents after
    /// Phase 3 (plan §3).
    Human,
}

impl CallerChannel {
    /// Whether this channel may issue runtime-attested verbs.
    #[must_use]
    pub fn is_attesting(self) -> bool {
        matches!(self, Self::Shim | Self::Human)
    }
}

/// Everything chiefd knows about the caller *before* it looks at the request
/// body. Assembled server-side from `SO_PEERCRED`, the bearer token, the
/// freshly re-read pane pid, and the registry — never from the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallerIdentity {
    /// The person this call is made as.
    pub person_id: PersonId,
    /// `SO_PEERCRED` uid of the connecting process.
    #[serde(default)]
    pub uid: u32,
    /// `SO_PEERCRED` pid of the connecting process. Ancestry is walked from
    /// here to the pane process, whose pid is re-read from the runtime at auth time
    /// (plan §6.2) — never a pid recorded at spawn.
    #[serde(default)]
    pub pid: i32,
    // TOMBSTONE (#751-P9): `pane: Option<PaneRef>` sat here — the terminal
    // socket, session and `%17` pane id a caller was bound to. Identity is
    // WHO is calling; a pane address is where their terminal happens to draw
    // them, and an HTTP caller with no terminal at all had to send `null`.
    /// The company the caller is materialized against — its display name.
    pub slug: Slug,
    // TOMBSTONE: `data_root: PathBuf`. It sat beside `org_dir` and named the
    // tree the company was said to live under. Nothing read it — not one
    // caller in this crate, and no fixture ever set it — so it was a second
    // path on an identity struct that already carries the only one that
    // decides anything. With the company IN the directory there is no tree
    // above it to name.
    /// The company DIRECTORY, which is the caller's company.
    ///
    /// **The slug never identifies a company for auth purposes** — two
    /// directories may hold companies with the same name — so every
    /// activity-class and readiness op checks this against the directory the
    /// daemon was actually started in.
    #[serde(default)]
    pub org_dir: PathBuf,
    /// Verb-level authorization class, loaded fresh per call.
    #[serde(default = "default_role")]
    pub role: CallerRole,
    /// How the call arrived.
    #[serde(default = "default_channel")]
    pub channel: CallerChannel,
}

fn default_role() -> CallerRole {
    CallerRole::Worker
}

fn default_channel() -> CallerChannel {
    CallerChannel::Model
}

impl CallerIdentity {
    /// Validate an attested echo for **equality** (plan §1).
    ///
    /// # Errors
    /// `Refused{code:"identity-echo-mismatch"}` when the echoed person differs
    /// from the authenticated identity.
    pub fn verify_echo(&self, echo: &AttestedEcho) -> Result<(), Refusal> {
        if echo.person_id == self.person_id {
            return Ok(());
        }
        Err(Refusal::new(
            IDENTITY_ECHO_MISMATCH,
            format!(
                "echoed identity {} does not match the authenticated caller {}",
                echo.person_id, self.person_id
            ),
        )
        .with_routes(["re-read your identity from the runtime header and retry".to_owned()]))
    }

    /// Disk authority: the caller's org dir must equal the registry's resolved
    /// location for the named slug (plan §1, §6.4).
    ///
    /// # Errors
    /// `Refused{code:"org-dir-mismatch"}` when they differ, even if the token,
    /// peercred and ancestry all check out.
    pub fn verify_disk_authority(&self, registry_org_dir: &Path) -> Result<(), Refusal> {
        if self.org_dir == registry_org_dir {
            return Ok(());
        }
        Err(Refusal::new(
            ORG_DIR_MISMATCH,
            format!(
                "caller org dir {} is not the registry location for {}",
                self.org_dir.display(),
                self.slug
            ),
        )
        .with_routes(["re-materialize against the registry's data root".to_owned()]))
    }

    /// Verb-level manager check.
    ///
    /// # Errors
    /// `Refused{code:"manager-only"}` for a worker.
    pub fn require_manager(&self, verb: &str) -> Result<(), Refusal> {
        if self.role.is_manager() {
            return Ok(());
        }
        Err(Refusal::new(MANAGER_ONLY, format!("{verb} requires organization manager authority"))
            .with_routes(["org.roster".to_owned(), "msg.send".to_owned()]))
    }

    /// Runtime-attested verbs (`progress`, `result`) may only arrive on an
    /// attesting channel — exposing them to the model would convert a
    /// launcher-attested fact into a model claim (plan §3.4).
    ///
    /// # Errors
    /// `Refused{code:"shim-attested-only"}` for a model-issued call.
    pub fn require_attesting_channel(&self, verb: &str) -> Result<(), Refusal> {
        if self.channel.is_attesting() {
            return Ok(());
        }
        Err(Refusal::new(
            SHIM_ATTESTED_ONLY,
            format!("{verb} is attested by the runtime and is not a model-callable tool"),
        )
        .with_routes(["org_send with completeAssignment:true".to_owned()]))
    }
}

/// The attested-echo identity (field class 3). Checked for equality against
/// [`CallerIdentity`] by [`CallerIdentity::verify_echo`].
///
/// This type exists exactly once so the echo field cannot be spelled by hand
/// on a new op without an explicit decision. Its one wire member,
/// `readiness.receipt`, was deleted with the provider-readiness store, so no
/// request struct currently declares it — the type and its check are kept
/// as auth machinery for the next echo op the conformance corpus already shows.
///
/// It is *not* `#[serde(flatten)]`ed into request structs: `flatten` and
/// `deny_unknown_fields` are mutually exclusive in serde, and
/// `deny_unknown_fields` is the more important of the two here. A request that
/// echoes identity declares the field directly and exposes it via a
/// `fn echo(&self) -> AttestedEcho`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttestedEcho {
    /// Echoed person id; must equal the authenticated caller's.
    pub person_id: PersonId,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn caller() -> CallerIdentity {
        CallerIdentity {
            person_id: PersonId("p-worker-1".to_owned()),
            uid: 1000,
            pid: 4242,
            slug: Slug::parse("cobalt").unwrap(),
            org_dir: PathBuf::from("/work/cobalt"),
            role: CallerRole::Worker,
            channel: CallerChannel::Shim,
        }
    }

    #[test]
    fn matching_echo_is_accepted() {
        let identity = caller();
        let echo = AttestedEcho { person_id: PersonId("p-worker-1".to_owned()) };
        assert!(identity.verify_echo(&echo).is_ok());
    }

    #[test]
    fn echo_with_a_different_person_is_refused_with_the_documented_code() {
        let identity = caller();
        let echo = AttestedEcho { person_id: PersonId("p-other".to_owned()) };
        let refusal = identity.verify_echo(&echo).unwrap_err();
        assert_eq!(refusal.code, IDENTITY_ECHO_MISMATCH);
        assert!(!refusal.legal_routes.is_empty());
    }

    #[test]
    fn disk_authority_refuses_a_caller_in_another_directory_even_when_the_slug_matches() {
        let identity = caller();
        assert!(identity.verify_disk_authority(Path::new("/work/cobalt")).is_ok());
        // Same name, different directory — which is now an ordinary thing for
        // an operator to have, not a shared-orgs-root curiosity. It is a
        // different company and the slug says nothing about that.
        let refusal = identity.verify_disk_authority(Path::new("/elsewhere/cobalt")).unwrap_err();
        assert_eq!(refusal.code, ORG_DIR_MISMATCH);
    }

    #[test]
    fn worker_cannot_reach_a_manager_only_verb() {
        let identity = caller();
        assert_eq!(identity.require_manager("org.department.add").unwrap_err().code, MANAGER_ONLY);
        let manager = CallerIdentity { role: CallerRole::Manager, ..caller() };
        assert!(manager.require_manager("org.department.add").is_ok());
    }

    #[test]
    fn model_channel_cannot_issue_a_runtime_attested_verb() {
        let model = CallerIdentity { channel: CallerChannel::Model, ..caller() };
        assert_eq!(
            model.require_attesting_channel("assignment.acknowledge").unwrap_err().code,
            SHIM_ATTESTED_ONLY
        );
        assert!(caller().require_attesting_channel("assignment.result").is_ok());
    }

    #[test]
    fn attested_echo_rejects_extra_fields() {
        let parsed = serde_json::from_str::<AttestedEcho>(r#"{"personId":"p","requestedBy":"p"}"#);
        assert!(parsed.is_err(), "requestedBy is injected and must never be accepted");
    }
}
