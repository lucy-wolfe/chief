//! The organization manifest rows — the sole structural authority
//! of a company.
//!
//! The record types, the manifest validator (plan D3) and the durable
//! read/write half of the manifest store all live here; the launcher's
//! TypeScript manifest types and store modules they replaced are deleted, so
//! this is the only place the manifest contract is defined.
//!
//! Nearly every other store validates itself *against* this one: activity
//! checks person placement, supervision checks manager/worker kind, launch
//! intent checks the CEO. It is also the file an operator hand-edits to unstick
//! a company, which is why two properties below are load-bearing and are
//! asserted by tests rather than assumed.
//!
//! # Unknown fields round-trip (operator safety)
//!
//! [`OrganizationManifest`], [`DepartmentRecord`] and [`PersonRecord`] each
//! carry a `#[serde(flatten)]` catch-all. The TypeScript reads this file with
//! `JSON.parse`, mutates a `structuredClone`, and writes it back, so any key it
//! does not model survives a write. A typed Rust port without a catch-all would
//! **delete** those keys on the first chiefd write — silent, permanent data
//! loss on a file humans edit. So unknown keys are preserved verbatim and
//! `manifest_json_round_trips_unknown_operator_fields` proves it.
//!
//! # Local mutation helper
//!
//! [`mutate`] is a store-local validation helper. Production changes are named
//! normalized operations, so no caller supplies or observes a global manifest
//! counter.
//!
//! # Genesis is the whole-company seed, not a manifest-only write
//!
//! `CompanyDb::org_manifest_genesis_with_models` (E7-S2, #815) is the one
//! creating mutation, and its transaction seeds five documents, not one: the
//! manifest rows [`create`] writes here, the model catalog/default rows,
//! materialization checkpoints, person operating contracts, and the
//! supervision/activity ledgers. All five commit together inside the same
//! `BEGIN IMMEDIATE`, or none of them land — there is no partial company.
//!
//! # Invariant 34 was REMOVED (operator decision, 2026-08-10)
//!
//! "Everybody should have bash... We should not remove the basic PI tools. And
//! Bash is one of them. Every agent should have a bash." `bash` is now an
//! ordinary tool for every kind: nothing refuses it, nothing strips it on
//! appointment, and nothing filters it on insert. The rule is recorded here
//! rather than deleted silently because its removal is visible in three
//! places at once and someone finding the gap in history needs to know it was
//! deliberate.
//!
//! # Polarity: `FailClosed` on read, write and clear
//!
//! Plan §5.5 assigns this store no polarity (§5.5b lists it as open, owned by
//! M12), so this milestone closes the decision:
//!
//! * *read* — there is no safe default structure. An unreadable manifest read
//!   as "empty" would mean a company with no CEO, no departments and no
//!   people; every dependent store would then "reconcile" that emptiness into
//!   its own ledger and delete real state. An error is the only honest answer.
//! * *write* — overwriting bytes chiefd could not read destroys the exact file
//!   an operator repairs a company with.
//! * *clear* — deleting the manifest is deleting the company. That happens
//!   through `company.remove` (M16, quarantine), never through a store clear.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::error::{corrupt_store, store_failure};
use crate::isotime::parse_iso_millis;
use crate::ledger::Ledgers;
use crate::polarity::{FailClosed, StoreKind};
use crate::store::CompanyContext;
use crate::ChiefdError;

/// Schema version of the manifest body.
pub const ORGANIZATION_SCHEMA_VERSION: u32 = 1;

/// The implicit root unit's id. Never derived, never configurable.
pub const ROOT_DEPARTMENT_ID: &str = "executive";

// --- refusal codes ------------------------------------------------------

/// The manifest failed a D3 structural rule.
pub const MANIFEST_INVALID: &str = "manifest-invalid";
/// A create attempted to occupy an existing organization namespace.
pub const ORGANIZATION_EXISTS: &str = "organization-exists";
/// The named company has no manifest.
pub const UNKNOWN_COMPANY: &str = "unknown-company";

/// The manifest store marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrganizationStore;

impl StoreKind for OrganizationStore {
    /// **Not** `"organization"`.
    ///
    /// The documents key is also the containment boundary: M7's
    /// `no_source_outside_a_stores_own_module_can_name_its_documents_key`
    /// greps every production source for the literal, and `"organization"` is a
    /// field name half the ported ledgers already carry (`ActivityLedger`,
    /// `SupervisionLedger` and the health monitor state all have one). A key
    /// that collides with a common field name makes that guard unenforceable —
    /// it would fire on innocent code forever, and the usual response to a
    /// guard that cries wolf is to weaken the guard. So the key is distinct
    /// from the field name on purpose.
    const NAME: &'static str = "org-manifest";
    type Body = OrganizationManifest;
}

impl FailClosed for OrganizationStore {}

// --- record types -------------------------------------------------------

/// Whether a person may be activated at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmploymentState {
    /// Available for work.
    Active,
    /// Retained but not staffed.
    Benched,
    /// Left the company.
    Departed,
}

/// A person's structural role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PersonKind {
    /// The CEO; heads the root unit.
    Executive,
    /// Heads exactly one department or contract unit.
    Head,
    /// Individual contributor; heads nothing.
    Worker,
}

impl PersonKind {
    /// Whether this kind is an organization manager, for authorization and
    /// structural rules.
    #[must_use]
    pub const fn is_manager(self) -> bool {
        !matches!(self, Self::Worker)
    }
}

/// Which flavour of unit a department record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitKind {
    /// The implicit root. Exactly one per manifest.
    Company,
    /// An ordinary department.
    Department,
    /// A transient engagement with an expiry.
    Contract,
}

/// Whether a unit is accepting work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitState {
    /// Accepting work.
    Active,
    /// Explicitly paused; ancestry-inherited by children.
    Paused,
}

/// A contract unit's engagement metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractMetadata {
    /// What the engagement is.
    pub engagement: String,
    /// When it started (ISO-8601).
    pub launched_at: String,
    /// When it ends, if it does (ISO-8601, strictly after `launched_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// One department or contract unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepartmentRecord {
    /// Hierarchical id (`parent-local`), kebab-case.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it exists to do.
    pub purpose: String,
    /// Optional **only** so schema-v1 manifests written before unit kinds stay
    /// readable; [`organization_unit_kind`] resolves the absent case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<UnitKind>,
    /// Contract metadata; present iff `kind == Contract`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient: Option<ContractMetadata>,
    /// The parent unit; absent only on the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_department_id: Option<String>,
    /// Who heads it.
    pub head_person_id: String,
    /// Active or paused.
    pub state: UnitState,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// Keys this port does not model, preserved verbatim (see module docs).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Default for [`PersonRecord::activation`] when older organization manifest
/// rows predate the field. See that field's doc comment for why `"resident"`.
fn default_activation() -> String {
    "resident".to_string()
}

/// The TS manifest model (`org-types.ts`) has NO `activation` field; it is a
/// chiefd-only column (delta #22) read/persisted internally. Omit it from the
/// serialized manifest when it holds the default so the wire round-trips
/// byte-for-byte with the launcher-authored manifest (a non-default value, which
/// TS never authors today).
fn is_default_activation(value: &str) -> bool {
    value == "resident"
}

/// One person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRecord {
    /// Kebab-case person id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Job title.
    pub title: String,
    /// What they own.
    pub mandate: String,
    /// Structural role.
    pub kind: PersonKind,
    /// Where they belong, which is also where they work.
    ///
    /// One field since the loan concept was deleted (2026-08-13): a loan was
    /// the only thing that could separate membership from placement, so the
    /// `home`/`assigned` pair that stood here recorded one fact twice.
    pub department_id: String,
    /// Employment state.
    pub employment_state: EmploymentState,
    /// Whether a pane is kept resident or spawned on demand.
    ///
    /// Found live, 2026-07-21: a company whose organization manifest rows
    /// predate this field being added can have people (e.g. hired before the field existed) with
    /// no `activation` key at all. Defaulting the missing case to
    /// `"resident"` — the value every currently-active person in the same
    /// live document already carries — rather than refusing the whole
    /// manifest deserialization over an old person record's missing field.
    #[serde(default = "default_activation", skip_serializing_if = "is_default_activation")]
    pub activation: String,
    /// Granted tools. Every kind may hold every tool, `bash` included.
    ///
    /// TOMBSTONE (chief-home-is-cwd §3/§4e): `skills`, `extensions` and
    /// `packages` stood beside this and are deleted. A tool grant is still a
    /// chief decision — it is computed here and reaches a pane as `--tools` —
    /// but Pi discovers, validates and loads an agent's skills itself through
    /// one symlink, extensions arrive as `--extension <path>` argv, and Pi
    /// packages belong to Pi's own package manager. Nothing selects a resource
    /// per person, so nothing records one.
    pub tools: Vec<String>,
    /// Project-local prompt templates under `prompts/`.
    ///
    /// Optional because schema-v1 organizations did not persist it; an omitted
    /// field means "no templates" and must never make a previously valid
    /// manifest unreadable (`org-types.ts:528-534`).
    // Serialized even when empty ([]/{}) to round-trip byte-for-byte with the TS
    // canonical (normalizeOrganizationSpec always emits these keys); `default`
    // keeps schema-v1 manifests that omit them readable.
    #[serde(default)]
    pub prompts: Vec<String>,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// Append-only staffing audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staffing_history: Option<Vec<serde_json::Value>>,
    /// Keys this port does not model, preserved verbatim (see module docs).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Launcher-owned supervision policy constants carried in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationPolicy {
    /// Supervision cycle cadence.
    pub supervision_interval_ms: i64,
    /// How long a delivered assignment may go unacknowledged.
    pub acknowledgement_timeout_ms: i64,
    /// Acknowledgement retries allowed.
    pub acknowledgement_retry_limit: u32,
    /// Generation replacements allowed per assignment.
    pub replacement_limit: u32,
}

/// The `sessionName` key the legacy `org_documents` blobs carry: `org-<slug>`.
///
/// # NOT the client's tmux session name. Do not use it as one.
///
/// It used to be both, and the two have DIVERGED on purpose. The operator
/// client's tmux session name is `chief-cli/src/placement.rs`'s
/// `session_name_for_slug`, and it now ends in a terminator character a slug
/// can never contain, because `tmux -t <name>` falls back to PREFIX matching:
/// under a bare `org-<slug>`, a probe for a stopped `acme` was answered by a
/// running `acme-corp` and `chief attach acme` moved the operator into the
/// wrong company's panes. That is a fact about tmux's target resolution, and
/// this backend has no tmux — so the fix belongs entirely to the client and
/// this key is deliberately NOT following it. Nothing here may be used to name
/// a tmux target; there is no tmux target on this side of the wire.
///
/// #751-P9 reduced six producers of the bare `format!("org-{slug}")` to this
/// ONE backend definition. AC6 finishes the job in the only direction that
/// removes the duplicate rather than centralising it: the tmux session name is
/// a DISPLAY fact the operator client derives for itself, so it is no longer a
/// field on [`OrganizationManifest`], no longer a column, and no longer
/// anywhere on chiefd's HTTP surface.
///
/// What is left is the handful of DERIVED-not-stored `sessionName` keys the
/// legacy `org_documents` blobs still carry (launch-intent, CEO boot lease,
/// goal-delivery quiesce, supervisor process state). Their row modules
/// reconstruct the key from the slug so a blob that carries it round-trips
/// zero-loss; each of those publishes REJECTS an unmodeled key, so the field
/// cannot simply vanish from the struct without refusing every historical
/// blob. Nothing else may call this, and nothing new may spell the format
/// string — `the_session_name_convention_is_derived_in_exactly_one_place`
/// fails the build if a producer does.
#[must_use]
pub fn runtime_session_for_slug(slug: &str) -> String {
    format!("org-{slug}")
}

/// The manifest: the sole structural authority for one company.
///
/// # DELETED: `runtimeSession` (AC6)
///
/// The manifest used to carry `org-<slug>` as a field, which put a tmux
/// session name on `/v1/org/manifest/read` — the single widest read on
/// chiefd's HTTP surface. It was pure derivation from `slug`, it was never a
/// column, and the one client that read it
/// (`chief-cli/src/company.rs::facts`) already carried the identical
/// derivation as its "the daemon is down" fallback. Deleted rather than
/// renamed: a second source of truth for a value both sides compute is exactly
/// what a rename preserves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationManifest {
    /// Always [`ORGANIZATION_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Always `"organization"`.
    pub kind: String,
    /// Company slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// What the company exists to do.
    pub purpose: String,
    /// Always [`ROOT_DEPARTMENT_ID`].
    pub root_department_id: String,
    /// Supervision policy.
    pub policy: OrganizationPolicy,
    /// Canonical department ordering; a bijection with `departments`.
    pub department_order: Vec<String>,
    /// Canonical person ordering; a bijection with `people`.
    pub people_order: Vec<String>,
    /// Departments by id.
    pub departments: BTreeMap<String, DepartmentRecord>,
    /// People by id.
    pub people: BTreeMap<String, PersonRecord>,
    /// ISO-8601 creation stamp.
    pub created_at: String,
    /// ISO-8601 stamp of the last write.
    pub updated_at: String,
    /// Keys this port does not model, preserved verbatim (see module docs).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl OrganizationManifest {
    /// The root unit's head — the CEO.
    ///
    /// # Errors
    /// [`MANIFEST_INVALID`] when the root unit is missing. Callers that already
    /// hold a validated manifest can rely on this succeeding.
    pub fn chief_person_id(&self) -> Result<&str, Refusal> {
        self.departments
            .get(&self.root_department_id)
            .map(|root| root.head_person_id.as_str())
            .ok_or_else(|| invalid("Organization root department is missing"))
    }

    /// The person record for `person_id`, if the manifest has one.
    #[must_use]
    pub fn person(&self, person_id: &str) -> Option<&PersonRecord> {
        self.people.get(person_id)
    }

    /// The department record for `department_id`, if the manifest has one.
    #[must_use]
    pub fn department(&self, department_id: &str) -> Option<&DepartmentRecord> {
        self.departments.get(department_id)
    }

    /// The unit `person_id` heads, if any. Manifest order, not map order, so
    /// the answer is deterministic when a damaged manifest has two.
    #[must_use]
    pub fn headed_department(&self, person_id: &str) -> Option<&DepartmentRecord> {
        self.department_order
            .iter()
            .filter_map(|id| self.departments.get(id))
            .find(|department| department.head_person_id == person_id)
    }

    /// The person `person_id` reports to, if anyone.
    ///
    /// Port of `org-activity.ts:731-736`: a head reports to the head of its
    /// unit's parent (so the CEO reports to nobody); everyone else reports to
    /// the head of the unit they are assigned to.
    #[must_use]
    pub fn manager_of(&self, person_id: &str) -> Option<&str> {
        let person = self.people.get(person_id)?;
        match self.headed_department(person_id) {
            Some(headed) => {
                let parent = headed.parent_department_id.as_deref()?;
                self.departments.get(parent).map(|unit| unit.head_person_id.as_str())
            }
            None => {
                self.departments.get(&person.department_id).map(|unit| unit.head_person_id.as_str())
            }
        }
    }

    /// Every manager, in manifest person order.
    ///
    /// "Manager" here is the *structural* definition the supervision ledger
    /// uses: someone who heads a unit. It is deliberately not `kind != worker`
    /// — a damaged manifest could disagree, and the check-in roster follows the
    /// units (`org-supervision-state.ts:284-287`).
    #[must_use]
    pub fn manager_ids(&self) -> Vec<String> {
        let heads: BTreeSet<&str> =
            self.departments.values().map(|unit| unit.head_person_id.as_str()).collect();
        self.people_order
            .iter()
            .filter(|person_id| heads.contains(person_id.as_str()))
            .cloned()
            .collect()
    }
}

/// Resolve a unit's kind, tolerating schema-v1 records that omit it.
///
/// # Errors
/// [`MANIFEST_INVALID`] when the stored kind is not one of the three.
pub fn organization_unit_kind(
    manifest: &OrganizationManifest,
    department: &DepartmentRecord,
) -> Result<UnitKind, Refusal> {
    Ok(department.kind.unwrap_or({
        if department.id == manifest.root_department_id {
            UnitKind::Company
        } else {
            UnitKind::Department
        }
    }))
}

/// The first local-or-ancestor paused unit, if any.
///
/// # Errors
/// [`MANIFEST_INVALID`] for an unknown unit or an ancestry cycle.
pub fn stopped_organization_unit_ancestor<'m>(
    manifest: &'m OrganizationManifest,
    unit_id: &str,
) -> Result<Option<&'m DepartmentRecord>, Refusal> {
    let mut cursor = manifest
        .departments
        .get(unit_id)
        .ok_or_else(|| invalid(format!("Unknown department '{unit_id}'")))?;
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    loop {
        if !visited.insert(cursor.id.as_str()) {
            return Err(invalid(format!("Department cycle includes '{}'", cursor.id)));
        }
        if cursor.state != UnitState::Active {
            return Ok(Some(cursor));
        }
        let Some(parent) = cursor.parent_department_id.as_deref() else {
            return Ok(None);
        };
        let Some(next) = manifest.departments.get(parent) else {
            return Ok(None);
        };
        cursor = next;
    }
}

/// Whether a unit and its whole ancestry are active.
///
/// Returns `false` for an unknown unit or a cycle: this is the predicate
/// activity and supervision use to decide whether to *keep* someone running,
/// and a structurally broken ancestry must never read as "keep running".
#[must_use]
pub fn organization_unit_is_active(manifest: &OrganizationManifest, unit_id: &str) -> bool {
    matches!(stopped_organization_unit_ancestor(manifest, unit_id), Ok(None))
}

// TOMBSTONE (#751-P9): `pane_department_id(manifest, person_id)` — the
// head-in-parent rule ("a head's pane lives in its parent's window, everyone
// else's in the unit they are assigned to") — is DELETED from the backend. It
// was a *display* transform: it answered which terminal window a pane is drawn
// in, which is the operator client's decision, and P5 re-established it in
// `chief-cli/src/placement.rs` from `isHeadOf` plus the department's own
// parent. Worse than a duplicated function, chiefd PERSISTED its answer
// (`person_activity.last_pane_department_id`,
// `transitions.from_pane_department_id`), so the stale copy was durable: P5's
// live test reparented a department and watched the derived answer and the
// stored column disagree, with the derivation correct.
//
// A backend caller that needs a person's department reads the manifest fact
// itself — `PersonRecord::department_id`, still projected on the activity
// ledger. Do not reintroduce a transform of it here; deriving where a pane is
// DRAWN is the client's business.

// --- D3: the full manifest validation set --------------------------------

fn invalid(message: impl Into<String>) -> Refusal {
    Refusal::new(MANIFEST_INVALID, message)
}

fn nonempty(value: &str, label: &str) -> Result<(), Refusal> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{label} is required")));
    }
    Ok(())
}

fn iso(value: &str, label: &str) -> Result<i64, Refusal> {
    parse_iso_millis(value).ok_or_else(|| invalid(format!("{label} must be an ISO-8601 timestamp")))
}

/// The complete D3 rule set (`org-types.ts:459-537`).
///
/// Run after **every** manifest mutation and before commit. The order below
/// mirrors the TypeScript exactly, because several later rules assume earlier
/// ones already held (the ancestry walk assumes parents resolve; the per-person
/// pass assumes `headedPeople` is complete).
///
/// # Errors
/// [`MANIFEST_INVALID`] naming the first rule that failed.
#[allow(clippy::too_many_lines)] // One rule per statement; splitting hides the order.
pub fn validate_organization_manifest(manifest: &OrganizationManifest) -> Result<(), Refusal> {
    if manifest.schema_version != ORGANIZATION_SCHEMA_VERSION || manifest.kind != "organization" {
        return Err(invalid("Unsupported organization manifest"));
    }
    if manifest.root_department_id != ROOT_DEPARTMENT_ID {
        return Err(invalid("Organization root department id is not the reserved root"));
    }
    if !manifest.departments.contains_key(&manifest.root_department_id) {
        return Err(invalid("Organization root department is missing"));
    }
    nonempty(&manifest.slug, "Organization slug")?;
    nonempty(&manifest.name, "Organization name")?;

    // Order bijectivity, both directions. This is a `validate()` rule and not a
    // SQL constraint for the reason plan §5.1 gives: bijectivity between a JSON
    // array and a JSON object is not expressible as a CHECK.
    let department_ids: BTreeSet<&String> = manifest.department_order.iter().collect();
    if department_ids.len() != manifest.department_order.len()
        || manifest.department_order.iter().any(|id| !manifest.departments.contains_key(id))
    {
        return Err(invalid("Organization department order is invalid"));
    }
    let person_ids: BTreeSet<&String> = manifest.people_order.iter().collect();
    if person_ids.len() != manifest.people_order.len()
        || manifest.people_order.iter().any(|id| !manifest.people.contains_key(id))
    {
        return Err(invalid("Organization people order is invalid"));
    }
    if manifest.department_order.len() != manifest.departments.len()
        || manifest.people_order.len() != manifest.people.len()
    {
        return Err(invalid("Organization order must include every department and person"));
    }

    let root = manifest
        .departments
        .get(&manifest.root_department_id)
        .ok_or_else(|| invalid("Organization root department is missing"))?;
    if root.parent_department_id.is_some() {
        return Err(invalid("Organization root department cannot have a parent"));
    }

    let mut headed_people: BTreeSet<&str> = BTreeSet::new();
    for id in &manifest.department_order {
        let department = &manifest.departments[id];
        if department.id != *id {
            return Err(invalid(format!("Department '{id}' has a mismatched id")));
        }
        let unit_kind = organization_unit_kind(manifest, department)?;
        if department.id == manifest.root_department_id && unit_kind != UnitKind::Company {
            return Err(invalid("Organization root unit must have kind 'company'"));
        }
        if department.id != manifest.root_department_id && unit_kind == UnitKind::Company {
            return Err(invalid("Only the organization root may have kind 'company'"));
        }
        if unit_kind == UnitKind::Contract {
            let transient = department.transient.as_ref().ok_or_else(|| {
                invalid(format!(
                    "Contract unit '{}' requires transient engagement and launchedAt metadata",
                    department.id
                ))
            })?;
            if transient.engagement.trim().is_empty() || transient.launched_at.trim().is_empty() {
                return Err(invalid(format!(
                    "Contract unit '{}' requires transient engagement and launchedAt metadata",
                    department.id
                )));
            }
            let launched = iso(
                &transient.launched_at,
                &format!("Contract unit '{}'.transient.launchedAt", department.id),
            )?;
            if let Some(expires) = transient.expires_at.as_deref() {
                let expires = iso(
                    expires,
                    &format!("Contract unit '{}'.transient.expiresAt", department.id),
                )?;
                if expires <= launched {
                    return Err(invalid(format!(
                        "Contract unit '{}'.transient.expiresAt must be later than launchedAt",
                        department.id
                    )));
                }
            }
        } else if department.transient.is_some() {
            return Err(invalid(format!(
                "Non-contract unit '{}' cannot retain transient metadata",
                department.id
            )));
        }
        if let Some(parent) = department.parent_department_id.as_deref() {
            if !manifest.departments.contains_key(parent) {
                return Err(invalid(format!(
                    "Department '{}' has unknown parent '{parent}'",
                    department.id
                )));
            }
        }
        let head = manifest.people.get(&department.head_person_id).ok_or_else(|| {
            invalid(format!(
                "Department '{}' has unknown head '{}'",
                department.id, department.head_person_id
            ))
        })?;
        // ONE head-placement rule. It was two — "must belong to" and "must work
        // in", checked a dozen lines apart against the two placement columns —
        // and with one column they are the same sentence checked twice, so the
        // second is deleted rather than kept as a duplicate refusal with a
        // different message for the same fact.
        if head.department_id != department.id {
            return Err(invalid(format!(
                "Head '{}' must belong to department '{}'",
                head.id, department.id
            )));
        }
        if !headed_people.insert(head.id.as_str()) {
            return Err(invalid(format!(
                "Person '{}' cannot head more than one department",
                head.id
            )));
        }
        let expected_kind = if department.id == manifest.root_department_id {
            PersonKind::Executive
        } else {
            PersonKind::Head
        };
        if head.kind != expected_kind {
            let label = if expected_kind == PersonKind::Executive { "executive" } else { "head" };
            return Err(invalid(format!("Department '{}' requires a {label} head", department.id)));
        }
        if head.employment_state == EmploymentState::Departed {
            return Err(invalid(format!(
                "Head '{}' cannot depart while heading department '{}'",
                head.id, department.id
            )));
        }
    }

    // Every unit must reach the root, and no ancestry may cycle.
    for id in &manifest.department_order {
        let mut cursor = &manifest.departments[id];
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        while let Some(parent) = cursor.parent_department_id.as_deref() {
            if !visited.insert(cursor.id.as_str()) {
                return Err(invalid(format!("Department cycle includes '{}'", cursor.id)));
            }
            let Some(next) = manifest.departments.get(parent) else { break };
            cursor = next;
        }
        if cursor.id != manifest.root_department_id {
            return Err(invalid(format!(
                "Department '{id}' is disconnected from the organization root"
            )));
        }
    }

    for id in &manifest.people_order {
        let person = &manifest.people[id];
        if person.id != *id {
            return Err(invalid(format!("Person '{id}' has a mismatched id")));
        }
        if !manifest.departments.contains_key(&person.department_id) {
            return Err(invalid(format!(
                "Person '{}' has unknown department '{}'",
                person.id, person.department_id
            )));
        }
        // TOMBSTONE (#1081): a rule refusing a person whose ASSIGNED unit
        // differed from their HOME unit stood here, and one of the two
        // unknown-department checks above was its twin. Both are deleted rather
        // than kept, because a manifest can no longer EXPRESS the state they
        // refused: there is one placement column, so "works somewhere they do
        // not belong" is not a manifest a caller can build and then be told is
        // invalid. A validator rule against an unrepresentable state is not a
        // safety net, it is a claim that the state exists.
        let heads_a_unit = headed_people.contains(person.id.as_str());
        if person.kind.is_manager() && !heads_a_unit {
            return Err(invalid(format!(
                "Leader '{}' must head exactly one department",
                person.id
            )));
        }
        if !person.kind.is_manager() && heads_a_unit {
            return Err(invalid(format!("Worker '{}' cannot head a department", person.id)));
        }
        if person.tools.iter().any(|tool| tool.trim().is_empty()) {
            return Err(invalid(format!("Person '{}' tools must be a string array", person.id)));
        }
        for (index, prompt) in person.prompts.iter().enumerate() {
            prompt_template_reference(prompt, &format!("Person '{}'.prompts[{index}]", person.id))?;
        }
        iso(&person.created_at, &format!("Person '{}'.createdAt", person.id))?;
    }

    iso(&manifest.created_at, "Organization createdAt")?;
    iso(&manifest.updated_at, "Organization updatedAt")?;
    Ok(())
}

/// Prompt templates are source-controlled project resources, never arbitrary
/// host files (`org-types.ts:185-200`).
///
/// # Errors
/// [`MANIFEST_INVALID`] for an absolute path, a backslash, any traversal
/// segment, a non-`prompts/` root, or a non-Markdown suffix.
pub fn prompt_template_reference(value: &str, label: &str) -> Result<(), Refusal> {
    if value.is_empty() || value.trim() != value {
        return Err(invalid(format!("{label} must be a non-empty prompt template path")));
    }
    if value.contains('\\') || value.starts_with('/') || value.chars().nth(1) == Some(':') {
        return Err(invalid(format!("{label} must be a relative path under prompts/")));
    }
    let traversal =
        value.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..");
    if traversal || !value.starts_with("prompts/") || !value.ends_with(".md") {
        return Err(invalid(format!(
            "{label} must name a Markdown template under prompts/ without traversal"
        )));
    }
    Ok(())
}

// --- durable read / write ------------------------------------------------

/// Whether `store` names the organization-manifest documents key. The router's
/// #442 write-interception gate uses this exactly as #440's supervision path
/// uses `supervision::is_supervision_store`.
#[must_use]
pub fn is_organization_store(store: &str) -> bool {
    store == OrganizationStore::NAME
}

/// #127: insert-if-absent for the manifest, as a typed store operation.
///
/// The launcher's `/v1/docs/insert-if-absent` for THIS process's own company
/// lands here instead of planting a second authority in `org_documents`. It
/// goes through [`create`] rather than dropping the body in, so a manifest that
/// does not VALIDATE is refused at the door — the store's fail-closed polarity
/// says an unreadable manifest must never become a company's structural
/// authority.
///
/// Returns whether this call created it. Presence check and creation share the
/// caller's one transaction, so a concurrent creator cannot slip between them.
///
/// # Errors
/// `Corrupt` when the body does not decode; [`MANIFEST_INVALID`] when it does
/// not validate.
pub fn create_if_absent(ledgers: &mut Ledgers, body: &str) -> Result<bool, ChiefdError> {
    if exists(ledgers) {
        return Ok(false);
    }
    let manifest = serde_json::from_str::<OrganizationManifest>(body)
        .map_err(|e| corrupt_store(OrganizationStore::NAME, e))?;
    create(ledgers, &manifest)?;
    Ok(true)
}

/// Read the manifest.
///
/// # Errors
/// [`UNKNOWN_COMPANY`] when no manifest row exists — deliberately a `Refused`
/// and not `Corrupt`: "this company was never created" is a caller mistake,
/// while `Corrupt` means "the company exists and its authority is unreadable",
/// and an operator runbook branches on exactly that difference.
/// `Corrupt{store:"org-manifest"}` when the row exists and does not decode;
/// `StoreFailure{store:"org-manifest"}` when it decodes and then fails its own
/// invariants.
pub fn read(ledgers: &Ledgers) -> Result<OrganizationManifest, ChiefdError> {
    let Some(body) = ledgers.document_body(OrganizationStore::NAME) else {
        return Err(ChiefdError::refused(
            UNKNOWN_COMPANY,
            "this company has no organization manifest",
        ));
    };
    // The two failures are NOT the same event. Bytes that will not decode are
    // damage and say so; a manifest that decodes and then fails its own
    // invariants is a store failure, and calling that "corrupt" is what sent
    // operators hunting for damaged bytes that parse perfectly well.
    let manifest = serde_json::from_str::<OrganizationManifest>(body)
        .map_err(|e| corrupt_store(OrganizationStore::NAME, e))?;
    validate_organization_manifest(&manifest)
        .map_err(|refusal| store_failure(OrganizationStore::NAME, &refusal))?;
    Ok(manifest)
}

/// Whether this company has a manifest at all. Never errors: a caller deciding
/// *whether to create* must not have to distinguish absent from corrupt.
#[must_use]
pub fn exists(ledgers: &Ledgers) -> bool {
    ledgers.document_body(OrganizationStore::NAME).is_some()
}

/// Write the initial manifest for a new company.
///
/// # Errors
/// [`MANIFEST_INVALID`] when `manifest` fails D3; [`ORGANIZATION_EXISTS`] when a
/// manifest already exists (create-once: the replay of a create is a claim
/// clash, plan §2.1, not a silent overwrite of a live company).
pub fn create(
    ledgers: &mut Ledgers,
    manifest: &OrganizationManifest,
) -> Result<OrganizationManifest, ChiefdError> {
    if exists(ledgers) {
        return Err(ChiefdError::refused(
            ORGANIZATION_EXISTS,
            format!("organization '{}' already exists", manifest.slug),
        ));
    }
    validate_organization_manifest(manifest)?;
    put(ledgers, manifest)?;
    Ok(manifest.clone())
}

/// Apply a direct in-memory mutation. The normalized SQL operations are the
/// production mutation authority; this helper is retained for store-local
/// validation and harness setup only.
pub fn mutate<T>(
    ledgers: &mut Ledgers,
    f: impl FnOnce(&mut OrganizationManifest) -> Result<T, Refusal>,
) -> Result<(T, OrganizationManifest), ChiefdError> {
    let current = read(ledgers)?;
    let at = crate::isotime::iso_millis(ledgers.now().0);
    let mut draft = current.clone();
    let result = f(&mut draft)?;
    draft.updated_at = at;
    validate_organization_manifest(&draft)?;
    put(ledgers, &draft)?;
    Ok((result, draft))
}

/// Remove the manifest, returning whether a row was present.
///
/// # Errors
/// `Corrupt{store:"org-manifest"}` when the stored bytes are unreadable — the
/// clear path is fail-closed like the rest of the store, so a company whose
/// authority chiefd cannot parse is never silently erased.
pub fn clear(ledgers: &mut Ledgers) -> Result<bool, ChiefdError> {
    if exists(ledgers) {
        read(ledgers)?;
    }
    Ok(ledgers.remove_document(OrganizationStore::NAME))
}

fn put(ledgers: &mut Ledgers, manifest: &OrganizationManifest) -> Result<(), Refusal> {
    let encoded = serde_json::to_string(manifest).map_err(|error| {
        Refusal::new(MANIFEST_INVALID, format!("cannot encode the organization manifest: {error}"))
    })?;
    ledgers.put_document(OrganizationStore::NAME, encoded);
    Ok(())
}

/// The [`CompanyContext`] every other store validates itself against.
///
/// Built from the manifest rather than assembled by callers, so the four facts
/// cannot drift from the authority that owns them.
///
/// # Errors
/// [`MANIFEST_INVALID`] when the root unit is missing.
pub fn company_context(manifest: &OrganizationManifest) -> Result<CompanyContext, Refusal> {
    // No session name. The context carried one solely because `launch_intent`
    // wrote it into a legacy blob key; that key is retired on every row model
    // that had it, so there is no longer a caller, and a context field nobody
    // reads is a third place for `org-<slug>` to be defined.
    Ok(CompanyContext::new(
        manifest.slug.clone(),
        manifest.chief_person_id()?.to_string(),
        manifest.people_order.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::WallMillis;
    use crate::test_support::northstar_manifest;

    const EPOCH: i64 = 1_784_116_800_000;

    fn ledgers() -> Ledgers {
        Ledgers::empty(WallMillis(EPOCH))
    }

    fn seeded() -> (Ledgers, OrganizationManifest) {
        let mut l = ledgers();
        let manifest = northstar_manifest(EPOCH);
        create(&mut l, &manifest).expect("the template manifest is valid");
        (l, manifest)
    }

    #[test]
    fn manifest_writes_do_not_mint_a_document_generation_counter() {
        let (mut ledgers, _) = seeded();
        let retired = "manifest_doc_generation";
        assert!(
            !ledgers.counters().any(|(name, _)| name == retired),
            "manifest creation must not persist a retired write counter",
        );
        mutate(&mut ledgers, |draft| {
            draft.purpose = "current semantic state only".to_string();
            Ok(())
        })
        .expect("manifest mutation");
        assert!(
            !ledgers.counters().any(|(name, _)| name == retired),
            "manifest mutation must not recreate the retired counter",
        );
    }

    #[test]
    fn the_northstar_template_is_a_valid_manifest() {
        let manifest = northstar_manifest(EPOCH);
        validate_organization_manifest(&manifest).expect("template validates");
        assert_eq!(manifest.slug, "northstar-conformance");
        assert_eq!(
            manifest.people_order,
            vec!["chief", "quant-head", "signal-researcher", "it-head"]
        );
        assert_eq!(manifest.department_order, vec!["executive", "quant", "it"]);
        assert_eq!(manifest.chief_person_id().expect("a ceo"), "chief");
    }

    /// Invariant 34 was REMOVED by operator decision, 2026-08-10: "Everybody
    /// should have bash... Every agent should have a bash."
    ///
    /// This test previously asserted the OPPOSITE — that granting `bash` to
    /// `quant-head` / `it-head` was refused with `manager-bash-forbidden`. It
    /// is inverted rather than deleted so the new contract stays pinned: a
    /// department head may hold `bash`, and it must survive the durable write
    /// path, not merely the pure validator. A "fast path" that skipped
    /// validation would pass a validator-only test and fail this one.
    #[test]
    fn a_department_head_may_now_hold_bash() {
        let (mut l, _) = seeded();
        for manager in ["quant-head", "it-head"] {
            mutate(&mut l, |draft| {
                draft.people.get_mut(manager).expect("a manager").tools.push("bash".to_string());
                Ok(())
            })
            .unwrap_or_else(|error| {
                panic!("a head holding bash is now allowed ({manager}): {error:?}")
            });
        }
        // And it is DURABLE: the write path no longer strips or filters it.
        let stored = read(&l).expect("manifest still readable");
        for manager in ["quant-head", "it-head"] {
            assert!(
                stored.people[manager].tools.iter().any(|tool| tool == "bash"),
                "{manager} must keep the bash it was granted"
            );
        }
    }

    /// Formerly the CEO's carve-out from invariant 34 (owner ruling
    /// 2026-08-02). The carve-out is now the general rule, but the executive
    /// case stays covered so a future narrowing has to fail something.
    #[test]
    fn the_executive_may_hold_bash() {
        let (mut l, _) = seeded();
        let (_, manifest) = mutate(&mut l, |draft| {
            draft.people.get_mut("chief").expect("the executive").tools.push("bash".to_string());
            Ok(())
        })
        .expect("the executive may hold bash");
        assert!(manifest.people["chief"].tools.iter().any(|tool| tool == "bash"));
    }

    #[test]
    fn a_worker_may_hold_bash() {
        let (mut l, _) = seeded();
        let (_, manifest) = mutate(&mut l, |draft| {
            draft
                .people
                .get_mut("signal-researcher")
                .expect("the worker")
                .tools
                .push("bash".to_string());
            Ok(())
        })
        .expect("a worker holding bash is the normal case");
        assert!(manifest.people["signal-researcher"].tools.iter().any(|tool| tool == "bash"));
    }

    /// The operator-safety property from the module docs, as a test: chiefd
    /// must not silently delete keys a human or a future version put in
    /// the organization manifest rows.
    #[test]
    fn manifest_json_round_trips_unknown_operator_fields() {
        let manifest = northstar_manifest(EPOCH);
        let mut value = serde_json::to_value(&manifest).expect("serializable");
        value["operatorNote"] = serde_json::json!("do not remove: incident 2026-07-19");
        value["people"]["chief"]["operatorPin"] = serde_json::json!(true);
        value["departments"]["quant"]["operatorPin"] = serde_json::json!(["a"]);
        let text = serde_json::to_string(&value).expect("encodable");

        let mut l = ledgers();
        l.put_document(OrganizationStore::NAME, text);
        let decoded = read(&l).expect("unknown keys are not corruption");
        assert_eq!(
            decoded.extra.get("operatorNote").and_then(serde_json::Value::as_str),
            Some("do not remove: incident 2026-07-19")
        );

        // And a chiefd write preserves them.
        let (_, written) = mutate(&mut l, |draft| {
            draft.purpose = "edited by chiefd".to_string();
            Ok(())
        })
        .expect("mutation");
        assert_eq!(
            written.extra.get("operatorNote").and_then(serde_json::Value::as_str),
            Some("do not remove: incident 2026-07-19")
        );
        assert_eq!(
            written.people["chief"].extra.get("operatorPin"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            written.departments["quant"].extra.get("operatorPin"),
            Some(&serde_json::json!(["a"]))
        );
    }

    #[test]
    fn unreadable_bytes_are_corrupt_and_an_absent_row_is_not() {
        let mut l = ledgers();
        let absent = read(&l).expect_err("no manifest");
        assert_eq!(absent.kind(), "Refused");
        assert_eq!(absent.code(), Some(UNKNOWN_COMPANY));

        l.put_document(OrganizationStore::NAME, "{\"schemaVersion\":1}");
        let corrupt = read(&l).expect_err("half a manifest is not a manifest");
        assert_eq!(corrupt.kind(), "Corrupt");
        match corrupt {
            ChiefdError::Corrupt { store, cause } => {
                assert_eq!(store, "org-manifest");
                assert!(!cause.is_empty(), "a decode failure must say what did not decode");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn clear_refuses_over_unreadable_bytes() {
        let mut l = ledgers();
        l.put_document(OrganizationStore::NAME, "not json");
        let err = clear(&mut l).expect_err("clear is fail-closed");
        assert_eq!(err.kind(), "Corrupt");
        assert!(exists(&l), "the refusal published nothing");
    }

    #[test]
    fn the_session_name_convention_is_derived_in_exactly_one_place() {
        // #751-P9. `org-<slug>` was written as a bare format string in six
        // backend producers — two manifest mints plus four document
        // projections — each free to disagree with the operator client, which
        // derives the same name for itself. There is one definition now, and
        // no producer may spell it out again. Five, not six, since
        // `supervisor_process_state_rows.rs` was DELETED with the detached
        // org-supervisor's state document (#825); a scan row naming a file that
        // does not exist would not compile, which is the correct outcome — the
        // row goes with the file.
        //
        // This is the legacy blob KEY, not a tmux session name — the two
        // diverged when the client's convention grew a prefix-collision
        // terminator (`chief-cli/src/placement.rs::session_name_for_slug`), and
        // this value must NOT follow it: it is what historical `org_documents`
        // blobs carry and what `organization-intercom.ts` validates them
        // against.
        assert_eq!(runtime_session_for_slug("northstar"), "org-northstar");
        let raw = format!("{}{}", r#"format!("org-"#, "{");
        for (label, source) in [
            ("organization_rows", include_str!("organization_rows.rs")),
            ("organization_spec", include_str!("organization_spec.rs")),
            ("launch_intent_rows", include_str!("launch_intent_rows.rs")),
            // `boot_lease_rows` was the fourth entry; it derived the session
            // name the same way, and it is deleted whole with the CEO boot
            // lease (chief-home-is-cwd §4c).
            ("goal_delivery_quiesce_rows", include_str!("goal_delivery_quiesce_rows.rs")),
        ] {
            assert!(
                !source.contains(&raw),
                "{label} must derive the session name through runtime_session_for_slug"
            );
        }
    }

    #[test]
    fn the_published_manifest_carries_no_session_name_at_all() {
        // AC6. The serialized manifest is the exact body
        // `/v1/org/manifest/read` hands a client, so neither the retired key
        // nor its value may appear in it: `org-<slug>` is a tmux session name
        // and the client derives its own. The BACKWARD direction — a body that
        // still carries the key — is
        // `organization_rows::a_manifest_still_carrying_the_retired_session_key_is_refused_loudly`,
        // which proves the key is refused rather than preserved verbatim.
        let manifest = northstar_manifest(EPOCH);
        let encoded = serde_json::to_string(&manifest).expect("encode");
        assert!(!encoded.contains("runtimeSession"), "{encoded}");
        assert!(!encoded.contains("org-northstar-conformance"), "{encoded}");
    }

    #[test]
    fn the_store_defines_no_head_in_parent_placement_rule() {
        // #751-P9. `pane_ownership_follows_the_disk_model` stood here and
        // asserted `pane_department_id`'s three cases. The rule is a DISPLAY
        // transform and lives only in the operator client now, so the test that
        // pinned it in the store is deleted rather than weakened — and replaced
        // by the assertion that actually matters going forward: the name must
        // not come back. (Same shape as `schema.rs`'s retired-registry guard.)
        let source = include_str!("organization.rs");
        let retired = format!("fn pane{}", "_department_id");
        assert!(
            !source.contains(&retired),
            "head-in-parent is the client's rule; the store must not re-derive it"
        );
        // The un-transformed facts a backend caller should read instead are
        // still here, and still the person's own.
        let manifest = northstar_manifest(EPOCH);
        assert_eq!(manifest.people["quant-head"].department_id, "quant");
        assert_eq!(manifest.people["signal-researcher"].department_id, "quant");
    }

    #[test]
    fn management_chains_follow_unit_ancestry() {
        let manifest = northstar_manifest(EPOCH);
        assert_eq!(manifest.manager_of("signal-researcher"), Some("quant-head"));
        assert_eq!(manifest.manager_of("quant-head"), Some("chief"));
        assert_eq!(manifest.manager_of("chief"), None, "the CEO reports to nobody");
        assert_eq!(manifest.manager_ids(), vec!["chief", "quant-head", "it-head"]);
    }

    #[test]
    fn a_paused_ancestor_deactivates_the_whole_subtree() {
        let mut manifest = northstar_manifest(EPOCH);
        assert!(organization_unit_is_active(&manifest, "quant"));
        manifest.departments.get_mut("executive").expect("root").state = UnitState::Paused;
        assert!(!organization_unit_is_active(&manifest, "quant"));
        assert_eq!(
            stopped_organization_unit_ancestor(&manifest, "quant")
                .expect("resolvable")
                .map(|unit| unit.id.clone()),
            Some("executive".to_string())
        );
    }

    #[test]
    fn d3_rejects_a_cycle_and_a_head_who_lives_elsewhere() {
        let mut orphan = northstar_manifest(EPOCH);
        orphan.departments.get_mut("quant").expect("quant").parent_department_id =
            Some("it".to_string());
        orphan.departments.get_mut("it").expect("it").parent_department_id =
            Some("quant".to_string());
        let err = validate_organization_manifest(&orphan).expect_err("a cycle is invalid");
        assert_eq!(err.code, MANIFEST_INVALID);

        // Reassigning a unit's head to somebody who lives elsewhere is the
        // rule that fires *before* the one-head-one-unit rule can — which is
        // why "person heads two units" is unreachable rather than merely
        // refused, in both implementations.
        let mut moved = northstar_manifest(EPOCH);
        moved.departments.get_mut("it").expect("it").head_person_id = "quant-head".to_string();
        let err = validate_organization_manifest(&moved).expect_err("a head lives in its unit");
        assert!(err.message.contains("must belong to department"), "{}", err.message);
    }

    /// A person's placement must name a department this company has.
    ///
    /// TOMBSTONE (#1081): this test used to prove a SECOND thing — that a
    /// person assigned somewhere other than their home unit was refused. It
    /// cannot be written any more, and that is the point: with one placement
    /// column there is no manifest a caller can build that says a person works
    /// where they do not belong, so the rule that refused it was deleted rather
    /// than kept. What survives is the half that still has a failure mode — a
    /// placement naming a department that is not there — and it is asserted
    /// here in full, including that the refusal never says "assignment" (#375:
    /// a noun this product reserves for neither a goal nor a task).
    #[test]
    fn d3_rejects_a_placement_naming_a_department_the_company_does_not_have() {
        let mut manifest = northstar_manifest(EPOCH);
        let worker = manifest.people.get_mut("signal-researcher").expect("worker");
        worker.department_id = "no-such-unit".to_string();
        let err = validate_organization_manifest(&manifest).expect_err("placed nowhere real");
        assert!(err.message.contains("signal-researcher"), "{}", err.message);
        assert!(err.message.contains("no-such-unit"), "{}", err.message);
        assert!(err.message.contains("unknown department"), "{}", err.message);
        assert!(!err.message.contains("assignment"), "{}", err.message);

        // And it passes once the placement names a real one again.
        manifest.people.get_mut("signal-researcher").expect("worker").department_id =
            "quant".to_string();
        validate_organization_manifest(&manifest).expect("a real department is the legal shape");
    }

    #[test]
    fn d3_rejects_contract_metadata_on_the_wrong_unit_kind() {
        let mut manifest = northstar_manifest(EPOCH);
        manifest.departments.get_mut("quant").expect("quant").transient = Some(ContractMetadata {
            engagement: "e".to_string(),
            launched_at: "2026-07-15T12:00:00.000Z".to_string(),
            expires_at: None,
        });
        let err =
            validate_organization_manifest(&manifest).expect_err("departments are not contracts");
        assert!(err.message.contains("cannot retain transient metadata"), "{}", err.message);
    }

    #[test]
    fn d3_rejects_a_contract_expiring_before_it_launched() {
        let mut manifest = northstar_manifest(EPOCH);
        let unit = manifest.departments.get_mut("it").expect("it");
        unit.kind = Some(UnitKind::Contract);
        unit.transient = Some(ContractMetadata {
            engagement: "ship the thing".to_string(),
            launched_at: "2026-07-15T12:00:00.000Z".to_string(),
            expires_at: Some("2026-07-14T12:00:00.000Z".to_string()),
        });
        let err = validate_organization_manifest(&manifest).expect_err("expiry precedes launch");
        assert!(err.message.contains("later than launchedAt"), "{}", err.message);
    }

    #[test]
    fn prompt_templates_cannot_escape_the_prompts_directory() {
        for bad in [
            "/etc/passwd",
            "prompts/../../secret.md",
            "notprompts/a.md",
            "prompts/a.txt",
            "prompts\\a.md",
            "",
        ] {
            assert!(prompt_template_reference(bad, "p").is_err(), "'{bad}' must be rejected");
        }
        prompt_template_reference("prompts/onboarding.md", "p").expect("a normal template");
    }

    #[test]
    fn schema_v1_people_without_prompts_stay_readable() {
        // `org-types.ts:528-534` interprets an omitted `prompts` as "none" so a
        // previously valid durable organization never becomes unreadable.
        let manifest = northstar_manifest(EPOCH);
        let mut value = serde_json::to_value(&manifest).expect("serializable");
        for person in value["people"].as_object_mut().expect("people").values_mut() {
            person.as_object_mut().expect("person").remove("prompts");
        }
        let decoded: OrganizationManifest =
            serde_json::from_value(value).expect("an omitted prompts field is legal");
        validate_organization_manifest(&decoded).expect("still valid");
        assert!(decoded.people["chief"].prompts.is_empty());
    }

    #[test]
    fn the_company_context_comes_from_the_manifest() {
        let manifest = northstar_manifest(EPOCH);
        let ctx = company_context(&manifest).expect("context");
        assert_eq!(ctx.slug(), "northstar-conformance");
        assert_eq!(ctx.chief_person_id(), "chief");
        assert!(ctx.knows_person("signal-researcher"));
        assert!(!ctx.knows_person("nobody"));
    }

    #[test]
    fn create_is_once_only() {
        let (mut l, manifest) = seeded();
        let err = create(&mut l, &manifest).expect_err("create is once-only");
        assert_eq!(err.code(), Some(ORGANIZATION_EXISTS));
    }
}
