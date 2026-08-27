//! The org-manifest ROW implementation (org-data-normalization P0, N2).
//!
//! Reconstruct an [`OrganizationManifest`] from the normalized tables, and
//! read the normalized rows for materialization and direct operation preflight.
//! The direct-operation helpers run entirely inside chiefd-core because the
//! normalized tables live in `chief.db` (applied by `open_company_db` /
//! `COMPANY_SCHEMA_SQL`); the raw `&Transaction` comes from
//! `CompanyDb::in_transaction`.
//!
//! Identity that is DERIVED, never stored (B2): `slug`/`runtime_session` from the
//! process's own company slug; `schema_version`/`kind` are constants;
//! `name`/`purpose`/`created_at` from the root (kind='company') department;
//! `updated_at` = `MAX(org_events.at)`.
//!
//! Item D (Fable #6): a normalized manifest carries NO unmodeled keys. Publish
//! REJECTS any `extra` with [`UNMODELED_KEYS`] (+ the offending key paths) —
//! never silently drops.

use rusqlite::{params, Transaction};

use crate::error::Refusal;
use crate::store::organization::{
    validate_organization_manifest, ContractMetadata, DepartmentRecord, EmploymentState,
    OrganizationManifest, OrganizationPolicy, PersonKind, PersonRecord, UnitKind, UnitState,
    MANIFEST_INVALID, ORGANIZATION_SCHEMA_VERSION, ROOT_DEPARTMENT_ID,
};
use crate::store::rows_txn::EventTouch;
use crate::ChiefdError;

/// A publish carried a key the row model does not represent (item D). The detail
/// lists the offending dotted paths so the caller fixes the exact field.
pub const UNMODELED_KEYS: &str = "unmodeled-keys";

/// This store's own documents key (`OrganizationStore::NAME`, organization.rs),
/// named HERE too so `persist_dispatch`/`load_ledgers` can dispatch/reconstruct
/// without reaching across module boundaries for it -- this module is a
/// registered co-owner of the org-manifest key (fence_containment.rs
/// `allowed_files`), the columnar persistence half of the same store.
pub const ORGANIZATION_MANIFEST_STORE: &str = "org-manifest";

/// A SQL failure reading/writing the normalized rows is a store failure, not a
/// caller error and not corruption. Greppable single mapping point for every
/// `.map_err`; the real `rusqlite::Error` travels inside the value.
fn store_failure(e: rusqlite::Error) -> ChiefdError {
    crate::error::store_failure("org-manifest-rows", e)
}

/// `rusqlite::Error` lifts into the store error adapter used by row helpers. A
/// local newtype keeps the direct-operation boundary explicit.
impl From<rusqlite::Error> for RowsSqlError {
    fn from(e: rusqlite::Error) -> Self {
        RowsSqlError(store_failure(e))
    }
}
/// Wrapper giving `ChiefdError` a `From<rusqlite::Error>` at the scaffold
/// boundary without a blanket impl on `ChiefdError` (which other crates rely on
/// NOT existing). Unwrapped immediately by [`publish`].
pub struct RowsSqlError(pub ChiefdError);
impl From<ChiefdError> for RowsSqlError {
    fn from(e: ChiefdError) -> Self {
        RowsSqlError(e)
    }
}

// ---- enum <-> column text -------------------------------------------------

fn person_kind_text(k: PersonKind) -> &'static str {
    match k {
        PersonKind::Executive => "executive",
        PersonKind::Head => "head",
        PersonKind::Worker => "worker",
    }
}
fn person_kind_of(s: &str) -> Result<PersonKind, ChiefdError> {
    Ok(match s {
        "executive" => PersonKind::Executive,
        "head" => PersonKind::Head,
        "worker" => PersonKind::Worker,
        other => return Err(invalid(format!("unknown person kind '{other}'"))),
    })
}
fn employment_text(e: EmploymentState) -> &'static str {
    match e {
        EmploymentState::Active => "active",
        EmploymentState::Benched => "benched",
        EmploymentState::Departed => "departed",
    }
}
fn employment_of(s: &str) -> Result<EmploymentState, ChiefdError> {
    Ok(match s {
        "active" => EmploymentState::Active,
        "benched" => EmploymentState::Benched,
        "departed" => EmploymentState::Departed,
        other => return Err(invalid(format!("unknown employment_state '{other}'"))),
    })
}
fn unit_kind_text(k: UnitKind) -> &'static str {
    match k {
        UnitKind::Company => "company",
        UnitKind::Department => "department",
        UnitKind::Contract => "contract",
    }
}
fn unit_kind_of(s: &str) -> Result<UnitKind, ChiefdError> {
    Ok(match s {
        "company" => UnitKind::Company,
        "department" => UnitKind::Department,
        "contract" => UnitKind::Contract,
        other => return Err(invalid(format!("unknown unit kind '{other}'"))),
    })
}
fn unit_state_text(s: UnitState) -> &'static str {
    match s {
        UnitState::Active => "active",
        UnitState::Paused => "paused",
    }
}
fn unit_state_of(s: &str) -> Result<UnitState, ChiefdError> {
    Ok(match s {
        "active" => UnitState::Active,
        "paused" => UnitState::Paused,
        other => return Err(invalid(format!("unknown unit state '{other}'"))),
    })
}

fn invalid(message: impl Into<String>) -> ChiefdError {
    ChiefdError::from(Refusal::new(MANIFEST_INVALID, message))
}

/// One department row of [`department_structure`]:
/// `(id, parent_id, head_person_id, state)`.
pub type DepartmentStructureRow = (String, Option<String>, String, String);

/// One person row of [`person_structure`]:
/// `(id, kind, employment_state, department_id)`.
pub type PersonStructureRow = (String, String, String, String);

/// The STRUCTURAL columns of every department, for the pure eligibility view
/// (`org_projection::OrgView`): `(id, parent_id, head_person_id, state)`.
///
/// Deliberately narrower than [`reconstruct`]. Eligibility depends on the
/// shape of the tree and nothing else, and a partially-populated people row —
/// which the whole-manifest read rejects outright — must not be able to make a
/// refusal check fail.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_structure(
    tx: &Transaction<'_>,
    slug: &str,
) -> rusqlite::Result<Vec<DepartmentStructureRow>> {
    let mut stmt = tx.prepare(
        "SELECT id, parent_id, head_person_id, state FROM departments \
         WHERE slug = ?1 ORDER BY ordinal",
    )?;
    let rows = stmt
        .query_map(params![slug], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
    rows.collect()
}

/// The STRUCTURAL columns of every person, for the pure eligibility view:
/// `(id, kind, employment_state, department_id)`.
/// The narrow twin of [`department_structure`]; see it for why.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_structure(
    tx: &Transaction<'_>,
    slug: &str,
) -> rusqlite::Result<Vec<PersonStructureRow>> {
    let mut stmt = tx.prepare(
        "SELECT id, kind, employment_state, department_id FROM people WHERE slug = ?1 ORDER BY ordinal",
    )?;
    let rows = stmt
        .query_map(params![slug], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
    rows.collect()
}

// ---- reconstruct (read path) ---------------------------------------------

/// Reconstruct the manifest from the rows keyed by `row_slug` (the company's
/// identity), or `None` when the company has no `org_settings` row (never
/// created / already removed).
///
/// # Why the display name is READ and no longer derived
///
/// This used to take the caller's own label as a second parameter and strip an
/// `@<rootHash>` suffix off it, because the identity was the composite
/// `<display>@<rootHash>` and the name rode inside the key. The identity is a
/// directory hash now and carries no name, so that derivation produced a
/// company called `c84afac7d8ad` — and every cross-store validator that checks
/// `ledger.organization == manifest.slug` then refused a correctly seeded
/// ledger, which made genesis itself refuse. The name is a stored column.
///
/// # Errors
/// [`ChiefdError::StoreFailure`] on a SQL failure; [`ChiefdError::Corrupt`] on a row
/// that cannot map to the
/// typed model.
pub fn reconstruct(
    tx: &Transaction<'_>,
    row_slug: &str,
) -> Result<Option<OrganizationManifest>, ChiefdError> {
    let (policy, company_slug) = match read_policy(tx, row_slug)? {
        Some(found) => found,
        None => return Ok(None),
    };

    let departments = read_departments(tx, row_slug)?;
    let people = read_people(tx, row_slug)?;
    let department_order = preorder_departments(&departments);
    let people_order = people.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();

    let root = departments
        .iter()
        .find(|(_, d)| d.parent_department_id.is_none())
        .map(|(_, d)| d.clone())
        .ok_or_else(|| invalid("reconstructed manifest has no root department"))?;

    // The manifest's updatedAt is the latest MANIFEST-entity event time, not the
    // whole shared feed's: org_events is shared across every store (activity,
    // materialization, supervision, …) and each stamps its own `at`, so an
    // unfiltered MAX would report a sibling store's write time. The manifest
    // publish touches only 'org'/'person'/'department', so filter to those — the
    // publish stamps their `at` with the incoming manifest.updatedAt.
    let updated_at: Option<String> = tx
        .query_row(
            "SELECT MAX(at) FROM org_events WHERE slug = ?1 \
             AND entity IN ('org', 'person', 'department')",
            params![row_slug],
            |r| r.get(0),
        )
        .map_err(store_failure)?;

    let manifest = OrganizationManifest {
        schema_version: ORGANIZATION_SCHEMA_VERSION,
        kind: "organization".to_string(),
        slug: company_slug.to_string(),
        name: root.name.clone(),
        purpose: root.purpose.clone(),
        root_department_id: ROOT_DEPARTMENT_ID.to_string(),
        policy,
        department_order,
        people_order,
        departments: departments.into_iter().collect(),
        people: people.into_iter().collect(),
        created_at: root.created_at.clone(),
        updated_at: updated_at.unwrap_or_else(|| root.created_at.clone()),
        extra: Default::default(),
    };
    Ok(Some(manifest))
}

/// Read the policy singleton and the company's display name from
/// `org_settings`.
///
/// Both in ONE read because both are the same row and the caller needs both to
/// build a manifest — a second statement for the name would be a second chance
/// for the two to disagree about whether the company exists.
fn read_policy(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<Option<(OrganizationPolicy, String)>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT supervision_interval_ms, acknowledgement_timeout_ms, \
             acknowledgement_retry_limit, replacement_limit, display_slug \
             FROM org_settings WHERE slug = ?1",
        )
        .map_err(store_failure)?;
    let mut rows = stmt.query(params![slug]).map_err(store_failure)?;
    let Some(row) = rows.next().map_err(store_failure)? else {
        return Ok(None);
    };
    Ok(Some((
        OrganizationPolicy {
            supervision_interval_ms: row.get(0).map_err(store_failure)?,
            acknowledgement_timeout_ms: row.get(1).map_err(store_failure)?,
            acknowledgement_retry_limit: row.get::<_, i64>(2).map_err(store_failure)? as u32,
            replacement_limit: row.get::<_, i64>(3).map_err(store_failure)? as u32,
        },
        row.get(4).map_err(store_failure)?,
    )))
}

fn read_departments(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<Vec<(String, DepartmentRecord)>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, parent_id, name, purpose, kind, state, head_person_id, created_at, \
             contract_engagement, contract_launched_at, contract_expires_at, ordinal \
             FROM departments WHERE slug = ?1 ORDER BY ordinal",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug], |row| {
            Ok((
                row.get::<_, String>(0)?,          // id
                row.get::<_, Option<String>>(1)?,  // parent_id
                row.get::<_, String>(2)?,          // name
                row.get::<_, String>(3)?,          // purpose
                row.get::<_, String>(4)?,          // kind
                row.get::<_, String>(5)?,          // state
                row.get::<_, String>(6)?,          // head_person_id
                row.get::<_, String>(7)?,          // created_at
                row.get::<_, Option<String>>(8)?,  // contract_engagement
                row.get::<_, Option<String>>(9)?,  // contract_launched_at
                row.get::<_, Option<String>>(10)?, // contract_expires_at
            ))
        })
        .map_err(store_failure)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, parent, name, purpose, kind, state, head, created, eng, launched, expires) =
            row.map_err(store_failure)?;
        let kind = unit_kind_of(&kind)?;
        let transient = if kind == UnitKind::Contract {
            Some(ContractMetadata {
                engagement: eng.ok_or_else(|| invalid("contract unit missing engagement"))?,
                launched_at: launched
                    .ok_or_else(|| invalid("contract unit missing launched_at"))?,
                expires_at: expires,
            })
        } else {
            None
        };
        out.push((
            id.clone(),
            DepartmentRecord {
                id,
                name,
                purpose,
                kind: Some(kind),
                transient,
                parent_department_id: parent,
                head_person_id: head,
                state: unit_state_of(&state)?,
                created_at: created,
                extra: Default::default(),
            },
        ));
    }
    Ok(out)
}

fn read_people(
    tx: &Transaction<'_>,
    slug: &str,
) -> Result<Vec<(String, PersonRecord)>, ChiefdError> {
    let mut out = Vec::new();
    // Read scalar rows in ordinal order into owned structs FIRST, so the
    // prepared statement's borrow of `tx` is released before the per-person
    // child-table reads below (which re-borrow `tx`).
    let mut full = tx
        .prepare(
            "SELECT id, name, title, mandate, kind, employment_state, department_id, created_at, activation FROM people WHERE slug = ?1 ORDER BY ordinal",
        )
        .map_err(store_failure)?;
    let people_rows = full
        .query_map(params![slug], |row| {
            Ok(PersonScalars {
                id: row.get(0)?,
                name: row.get(1)?,
                title: row.get(2)?,
                mandate: row.get(3)?,
                kind: row.get(4)?,
                employment_state: row.get(5)?,
                department_id: row.get(6)?,
                created_at: row.get(7)?,
                activation: row.get(8)?,
            })
        })
        .map_err(store_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_failure)?;
    for s in people_rows {
        let tools = read_str_list(
            tx,
            "SELECT tool FROM person_tools WHERE slug=?1 AND person_id=?2 ORDER BY ordinal",
            slug,
            &s.id,
        )?;
        let prompts = read_str_list(
            tx,
            "SELECT template FROM person_prompts WHERE slug=?1 AND person_id=?2 ORDER BY ordinal",
            slug,
            &s.id,
        )?;
        let staffing_history = read_staffing_history(tx, slug, &s.id)?;
        out.push((
            s.id.clone(),
            PersonRecord {
                id: s.id,
                name: s.name,
                title: s.title,
                mandate: s.mandate,
                kind: person_kind_of(&s.kind)?,
                department_id: s.department_id,
                employment_state: employment_of(&s.employment_state)?,
                // people.activation (delta #22) is authoritative — read it, never
                // inject a default (the stale "Item A held" comment is retired).
                activation: s.activation,
                tools,
                prompts,
                created_at: s.created_at,
                staffing_history,
                extra: Default::default(),
            },
        ));
    }
    Ok(out)
}

struct PersonScalars {
    id: String,
    name: String,
    title: String,
    mandate: String,
    kind: String,
    employment_state: String,
    department_id: String,
    created_at: String,
    activation: String,
}

/// Reconstruct one person's append-only staffing audit from the `staffing_history`
/// rows (ordered by the per-slug seq), as the manifest's opaque JSON events
/// ({action, at, fromDepartmentId?, toDepartmentId?, reason?} — the camelCase
/// shape org-types.ts declares). `None` when the person has no history, so the
/// field is omitted (matching the optional TS field). The manifest carried this
/// inline on the blob path; the row model keeps it in its own table.
fn read_staffing_history(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> Result<Option<Vec<serde_json::Value>>, ChiefdError> {
    let mut stmt = tx
        .prepare(
            "SELECT action, from_department_id, to_department_id, reason, at \
             FROM staffing_history WHERE slug=?1 AND person_id=?2 ORDER BY seq",
        )
        .map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug, person_id], |row| {
            let action: String = row.get(0)?;
            let from: Option<String> = row.get(1)?;
            let to: Option<String> = row.get(2)?;
            let reason: String = row.get(3)?;
            let at: String = row.get(4)?;
            let mut obj = serde_json::Map::new();
            obj.insert("action".to_string(), serde_json::Value::String(action));
            obj.insert("at".to_string(), serde_json::Value::String(at));
            if let Some(f) = from {
                obj.insert("fromDepartmentId".to_string(), serde_json::Value::String(f));
            }
            if let Some(t) = to {
                obj.insert("toDepartmentId".to_string(), serde_json::Value::String(t));
            }
            if !reason.is_empty() {
                obj.insert("reason".to_string(), serde_json::Value::String(reason));
            }
            Ok(serde_json::Value::Object(obj))
        })
        .map_err(store_failure)?;
    let events = rows.collect::<Result<Vec<_>, _>>().map_err(store_failure)?;
    Ok(if events.is_empty() { None } else { Some(events) })
}

fn read_str_list(
    tx: &Transaction<'_>,
    sql: &str,
    slug: &str,
    person_id: &str,
) -> Result<Vec<String>, ChiefdError> {
    let mut stmt = tx.prepare(sql).map_err(store_failure)?;
    let rows = stmt
        .query_map(params![slug, person_id], |row| row.get::<_, String>(0))
        .map_err(store_failure)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_failure)
}

// TOMBSTONE (chief-home-is-cwd §4e): `read_resources` — the `person_resources`
// reader that split rows back into a person's skills/extensions/packages. The
// table is dropped and `PersonRecord` carries no resource field, so a
// reconstructed person has nothing to read back. `read_str_list` above still
// serves `person_tools` and `person_prompts`, which are unaffected.

/// Preorder department ids: root first, then each node's children by ordinal.
fn preorder_departments(departments: &[(String, DepartmentRecord)]) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    // Preserve the SELECT ordinal ordering: `departments` is already ordinal-sorted.
    for (id, d) in departments {
        children.entry(d.parent_department_id.clone()).or_default().push(id.clone());
    }
    let mut order = Vec::new();
    let mut stack: Vec<String> = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    while let Some(id) = stack.pop() {
        order.push(id.clone());
        if let Some(kids) = children.get(&Some(id)) {
            for kid in kids.iter().rev() {
                stack.push(kid.clone());
            }
        }
    }
    order
}

// ---- first-write-only genesis (diff/write path) --------------------------

/// Result of a first-write-only normalized organization genesis.
///
/// A caller either creates an absent organization atomically or is told that a
/// company already owns the namespace. There is no stale-version retry key.
pub enum ManifestGenesisOutcome {
    /// The normalized organization rows were created atomically.
    Created,
    /// A normalized organization already exists for this storage key.
    AlreadyExists,
}

/// Create normalized organization rows exactly once.
///
/// The transaction writes the normalized rows directly and atomically. It is
/// intentionally first-write-only: mutable organization changes must use a
/// named normalized operation rather than replacing an aggregate snapshot.
pub fn genesis(
    tx: &Transaction<'_>,
    row_slug: &str,
    incoming: &OrganizationManifest,
) -> Result<ManifestGenesisOutcome, ChiefdError> {
    reject_unmodeled_keys(incoming)?;
    validate_organization_manifest(incoming)?;

    if reconstruct(tx, row_slug)?.is_some() {
        return Ok(ManifestGenesisOutcome::AlreadyExists);
    }
    let at = incoming.updated_at.clone();
    // Append the direct row writes' events in the same transaction. `apply_and_emit`
    // needs `E: From<rusqlite::Error>`; `ChiefdError` has none, so the closure runs
    // in the local `RowsSqlError` wrapper and is unwrapped here.
    crate::store::rows_txn::apply_and_emit::<RowsSqlError, _>(tx, row_slug, &at, "", |tx| {
        // Defer FK checks to COMMIT: the diff writes departments and people in a
        // fixed order, but a subtree removal deletes a department before the people
        // that still reference it (people.department_id → departments), and an
        // add inserts a person before... the reverse. The END state is always
        // FK-consistent (it's a whole validated manifest), so deferring to commit
        // makes the intermediate write order irrelevant while still catching a
        // genuinely inconsistent result.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        let mut touches = Vec::new();
        write_org_settings(tx, row_slug, incoming, None, &mut touches)?;
        diff_departments(tx, row_slug, incoming, None, &mut touches)?;
        diff_people(tx, row_slug, incoming, None, &mut touches)?;
        Ok(touches)
    })
    .map(|_seq| ManifestGenesisOutcome::Created)
    .map_err(|RowsSqlError(e)| e)
}

/// Reject any `extra` (serde-flatten) key on the manifest, a department, or a
/// person — a normalized manifest carries none (item D). NEVER silently drops.
fn reject_unmodeled_keys(m: &OrganizationManifest) -> Result<(), ChiefdError> {
    let mut paths = Vec::new();
    for key in m.extra.keys() {
        paths.push(format!("extra.{key}"));
    }
    for (id, d) in &m.departments {
        for key in d.extra.keys() {
            paths.push(format!("departments.{id}.extra.{key}"));
        }
    }
    for (id, p) in &m.people {
        for key in p.extra.keys() {
            paths.push(format!("people.{id}.extra.{key}"));
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    paths.sort();
    Err(ChiefdError::from(Refusal::new(
        UNMODELED_KEYS,
        format!("manifest carries unmodeled keys the row model cannot store: {}", paths.join(", ")),
    )))
}

fn write_org_settings(
    tx: &Transaction<'_>,
    slug: &str,
    m: &OrganizationManifest,
    current: Option<&OrganizationManifest>,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    // The display name is compared too, not just the policy. It cannot change
    // today — there is no rename verb — but an early return that skipped the
    // ONE column carrying a company's name would be a silent no-op the moment
    // one existed, and the row is written once at genesis either way.
    if current.map(|c| c.policy == m.policy && c.slug == m.slug).unwrap_or(false) {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, \
         acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) \
         VALUES(?1,?2,?3,?4,?5,?6) \
         ON CONFLICT(slug) DO UPDATE SET display_slug=?2, supervision_interval_ms=?3, \
         acknowledgement_timeout_ms=?4, acknowledgement_retry_limit=?5, replacement_limit=?6",
        params![
            slug,
            m.slug,
            m.policy.supervision_interval_ms,
            m.policy.acknowledgement_timeout_ms,
            m.policy.acknowledgement_retry_limit as i64,
            m.policy.replacement_limit as i64,
        ],
    )
    .map_err(store_failure)?;
    touches.push(EventTouch::new("org", &m.slug, "upsert", "org_settings", slug));
    Ok(())
}

fn diff_departments(
    tx: &Transaction<'_>,
    slug: &str,
    m: &OrganizationManifest,
    current: Option<&OrganizationManifest>,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    // Removals first (children before parents can't be guaranteed via FK, so
    // rely on the incoming set being complete; delete any current id absent now).
    if let Some(cur) = current {
        for id in cur.departments.keys() {
            if !m.departments.contains_key(id) {
                tx.execute("DELETE FROM departments WHERE slug=?1 AND id=?2", params![slug, id])
                    .map_err(store_failure)?;
                touches.push(EventTouch::new("department", id, "delete", "departments", slug));
            }
        }
    }
    // Vacate every surviving department's ordinal BEFORE the final assignment:
    // `departments` has a UNIQUE(slug, parent_id, ordinal) index, so a sibling
    // reorder or a reparent that assigns one department an (parent, ordinal) pair
    // another still holds would transiently violate it. `-1 - ordinal` maps the
    // current ordinals onto distinct negatives (disjoint from the final
    // non-negative range) so neither the vacate nor the reassignment collides.
    if current.is_some() {
        tx.execute("UPDATE departments SET ordinal = -1 - ordinal WHERE slug = ?1", params![slug])
            .map_err(store_failure)?;
    }
    for (ordinal, id) in m.department_order.iter().enumerate() {
        let d = m
            .departments
            .get(id)
            .ok_or_else(|| invalid(format!("department_order names unknown '{id}'")))?;
        let unchanged = current
            .and_then(|c| c.departments.get(id))
            .map(|prev| prev == d && c_ordinal(current, id) == Some(ordinal as i64))
            .unwrap_or(false);
        if unchanged {
            // Content AND final ordinal unchanged — restore the ordinal the vacate
            // moved, with no event (no-churn).
            tx.execute(
                "UPDATE departments SET ordinal = ?1 WHERE slug = ?2 AND id = ?3",
                params![ordinal as i64, slug, id],
            )
            .map_err(store_failure)?;
            continue;
        }
        let kind = d.kind.unwrap_or(if d.parent_department_id.is_none() {
            UnitKind::Company
        } else {
            UnitKind::Department
        });
        let (eng, launched, expires) = match &d.transient {
            Some(c) => {
                (Some(c.engagement.clone()), Some(c.launched_at.clone()), c.expires_at.clone())
            }
            None => (None, None, None),
        };
        tx.execute(
            "INSERT INTO departments(slug, id, parent_id, name, purpose, kind, state, \
             head_person_id, contract_engagement, contract_launched_at, contract_expires_at, \
             ordinal, created_at, updated_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) \
             ON CONFLICT(slug,id) DO UPDATE SET parent_id=?3, name=?4, purpose=?5, kind=?6, \
             state=?7, head_person_id=?8, contract_engagement=?9, contract_launched_at=?10, \
             contract_expires_at=?11, ordinal=?12, updated_at=?14",
            params![
                slug,
                id,
                d.parent_department_id,
                d.name,
                d.purpose,
                unit_kind_text(kind),
                unit_state_text(d.state),
                d.head_person_id,
                eng,
                launched,
                expires,
                ordinal as i64,
                d.created_at,
                m.updated_at,
            ],
        )
        .map_err(store_failure)?;
        touches.push(EventTouch::new("department", id, "upsert", "departments", slug));
    }
    Ok(())
}

fn diff_people(
    tx: &Transaction<'_>,
    slug: &str,
    m: &OrganizationManifest,
    current: Option<&OrganizationManifest>,
    touches: &mut Vec<EventTouch>,
) -> Result<(), ChiefdError> {
    if let Some(cur) = current {
        for id in cur.people.keys() {
            if !m.people.contains_key(id) {
                delete_person_children(tx, slug, id)?;
                tx.execute("DELETE FROM people WHERE slug=?1 AND id=?2", params![slug, id])
                    .map_err(store_failure)?;
                touches.push(EventTouch::new("person", id, "delete", "people", slug));
            }
        }
    }
    // Vacate every surviving person's ordinal into a distinct temp range BEFORE
    // assigning the final 0..N order below. `people` has a UNIQUE(slug, ordinal)
    // index, so a reorder/insert that assigns one person an ordinal another still
    // holds would transiently violate it mid-statement. `-1 - ordinal` maps the
    // current distinct 0..M onto distinct negatives (disjoint from the final
    // non-negative range), so neither the vacate nor the reassignment collides.
    if current.is_some() {
        tx.execute("UPDATE people SET ordinal = -1 - ordinal WHERE slug = ?1", params![slug])
            .map_err(store_failure)?;
    }
    for (ordinal, id) in m.people_order.iter().enumerate() {
        let p = m
            .people
            .get(id)
            .ok_or_else(|| invalid(format!("people_order names unknown '{id}'")))?;
        let unchanged = current
            .and_then(|c| c.people.get(id))
            .map(|prev| prev == p && p_ordinal(current, id) == Some(ordinal as i64))
            .unwrap_or(false);
        if unchanged {
            // Content AND final ordinal unchanged — just restore the ordinal the
            // vacate above moved, with no event or child rewrite (no-churn).
            tx.execute(
                "UPDATE people SET ordinal = ?1 WHERE slug = ?2 AND id = ?3",
                params![ordinal as i64, slug, id],
            )
            .map_err(store_failure)?;
            continue;
        }
        tx.execute(
            "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, \
             department_id, ordinal, created_at, updated_at, activation) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
             ON CONFLICT(slug,id) DO UPDATE SET name=?3, title=?4, mandate=?5, kind=?6, \
             employment_state=?7, department_id=?8, \
             ordinal=?9, updated_at=?11, activation=?12",
            params![
                slug,
                id,
                p.name,
                p.title,
                p.mandate,
                person_kind_text(p.kind),
                employment_text(p.employment_state),
                p.department_id,
                ordinal as i64,
                p.created_at,
                m.updated_at,
                p.activation,
            ],
        )
        .map_err(store_failure)?;
        // Child sets are small; rewrite them wholesale for a touched person.
        rewrite_person_children(tx, slug, p)?;
        // The manifest carries the append-only staffingHistory; persist any events
        // it gained (TS staffing ops append to the manifest, then publish) into the
        // append-only table beyond what it already holds.
        sync_staffing_history(tx, slug, id, &p.staffing_history)?;
        touches.push(EventTouch::new("person", id, "upsert", "people", slug));
    }
    Ok(())
}

/// Append to the `staffing_history` table any events the manifest's per-person
/// `staffingHistory` gained beyond what the table already holds. TS staffing ops
/// append to the manifest (a stable prefix, reconstruct builds it back in seq
/// order), so the new events are exactly the tail past the current row count.
/// Atomic org-ops write the table directly; a later manifest publish sees them via
/// reconstruct and never re-appends (the prefix already matches).
fn sync_staffing_history(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    history: &Option<Vec<serde_json::Value>>,
) -> Result<(), ChiefdError> {
    let events = match history {
        Some(events) => events,
        None => return Ok(()),
    };
    let existing: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM staffing_history WHERE slug=?1 AND person_id=?2",
            params![slug, person_id],
            |r| r.get(0),
        )
        .map_err(store_failure)?;
    for event in events.iter().skip(existing.max(0) as usize) {
        let obj =
            event.as_object().ok_or_else(|| invalid("staffingHistory event must be an object"))?;
        let action = obj
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("staffingHistory event missing 'action'"))?;
        let at = obj
            .get("at")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("staffingHistory event missing 'at'"))?;
        let from = obj.get("fromDepartmentId").and_then(serde_json::Value::as_str);
        let to = obj.get("toDepartmentId").and_then(serde_json::Value::as_str);
        let reason = obj.get("reason").and_then(serde_json::Value::as_str).unwrap_or("");
        append_staffing_history(tx, slug, person_id, action, from, to, reason, at)
            .map_err(store_failure)?;
    }
    Ok(())
}

fn delete_person_children(tx: &Transaction<'_>, slug: &str, id: &str) -> Result<(), ChiefdError> {
    for table in ["person_tools", "person_prompts"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE slug=?1 AND person_id=?2"),
            params![slug, id],
        )
        .map_err(store_failure)?;
    }
    Ok(())
}

fn rewrite_person_children(
    tx: &Transaction<'_>,
    slug: &str,
    p: &PersonRecord,
) -> Result<(), ChiefdError> {
    delete_person_children(tx, slug, &p.id)?;
    for (ordinal, tool) in p.tools.iter().enumerate() {
        tx.execute(
            "INSERT INTO person_tools(slug, person_id, ordinal, tool) VALUES(?1,?2,?3,?4)",
            params![slug, p.id, ordinal as i64, tool],
        )
        .map_err(store_failure)?;
    }
    for (ordinal, template) in p.prompts.iter().enumerate() {
        tx.execute(
            "INSERT INTO person_prompts(slug, person_id, ordinal, template) VALUES(?1,?2,?3,?4)",
            params![slug, p.id, ordinal as i64, template],
        )
        .map_err(store_failure)?;
    }
    Ok(())
}

fn c_ordinal(current: Option<&OrganizationManifest>, id: &str) -> Option<i64> {
    current.and_then(|c| c.department_order.iter().position(|x| x == id)).map(|i| i as i64)
}
fn p_ordinal(current: Option<&OrganizationManifest>, id: &str) -> Option<i64> {
    current.and_then(|c| c.people_order.iter().position(|x| x == id)).map(|i| i as i64)
}

// ---------------------------------------------------------------------------
// Read accessors for the atomic org-op family (org_ops) — so those ops NEVER
// name the manifest store's `departments`/`people` tables in raw SQL (fable's
// store-containment contract). The manifest store owns these reads.
// ---------------------------------------------------------------------------

/// A person's department, or `None` when the person has no row. Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure; a missing row is `None`, never an error.
pub fn person_department(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT department_id FROM people WHERE slug = ?1 AND id = ?2",
        params![slug, person_id],
        |row| row.get(0),
    )
    .optional()
}

/// Whether `requester_person_id` currently manages `department_id`, evaluated
/// entirely from normalized rows inside the caller's transaction. Executives
/// have company-wide scope; a head has only the department they currently head
/// and its descendants. Workers, departed people, missing people, disconnected
/// trees, and cycles are out of scope.
///
/// This is the semantic transaction predicate used by caller-revisionless staffing:
/// a pane authorized against an older projection cannot commit after its person
/// was demoted/transferred or the target hierarchy moved.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_manages_department(
    tx: &Transaction<'_>,
    slug: &str,
    requester_person_id: &str,
    department_id: &str,
) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    let requester: Option<(String, String)> = tx
        .query_row(
            "SELECT kind, employment_state FROM people WHERE slug = ?1 AND id = ?2",
            params![slug, requester_person_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((kind, employment_state)) = requester else {
        return Ok(false);
    };
    if employment_state == "departed" {
        return Ok(false);
    }
    if kind == "executive" {
        return Ok(true);
    }
    if kind != "head" {
        return Ok(false);
    }
    let managed_root: Option<String> = tx
        .query_row(
            "SELECT id FROM departments WHERE slug = ?1 AND head_person_id = ?2 LIMIT 1",
            params![slug, requester_person_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(managed_root) = managed_root else {
        return Ok(false);
    };
    let mut cursor = Some(department_id.to_string());
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = cursor {
        if id == managed_root {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            return Ok(false);
        }
        cursor = department_parent_state(tx, slug, &id)?.and_then(|(parent, _)| parent);
    }
    Ok(false)
}

/// Whether `requester_person_id` may create a NEW department beneath
/// `parent_department_id` — the row-level twin of the intercom's
/// `requireDepartmentCreationParent` / `authorityRootDepartmentId`.
///
/// Deliberately more permissive than [`person_manages_department`], and only
/// for creation. Managing an existing unit is rooted at the unit you HEAD;
/// creating a child unit takes authority over nobody, because nothing that
/// already exists changes hands and the creator heads what they made. So the
/// accepted parent is the requester's authority root — the unit they head,
/// or failing that the unit they are assigned to — or anything already inside
/// their management scope.
///
/// This is what makes "every leaf can become a parent" true at the store
/// boundary: before it, `create_department_with_staff_unit` asked
/// [`person_manages_department`], which answers false for every worker, so a
/// worker's create was refused `requester-out-of-scope` no matter what tool it
/// held. Growth stays DOWNWARD: a peer, an ancestor or any unit outside the
/// subtree is still refused by exactly the same walk as before.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_may_create_under_department(
    tx: &Transaction<'_>,
    slug: &str,
    requester_person_id: &str,
    parent_department_id: &str,
) -> rusqlite::Result<bool> {
    if person_manages_department(tx, slug, requester_person_id, parent_department_id)? {
        return Ok(true);
    }
    use rusqlite::OptionalExtension;
    let requester: Option<(String, String)> = tx
        .query_row(
            "SELECT department_id, employment_state FROM people WHERE slug = ?1 AND id = ?2",
            params![slug, requester_person_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((department_id, employment_state)) = requester else {
        return Ok(false);
    };
    if employment_state == "departed" {
        return Ok(false);
    }
    // A head's authority root is the unit it heads, and that case already
    // answered true above. What is left is the leaf case: the unit it sits in.
    let headed = department_headed_by_person(tx, slug, requester_person_id)?;
    if headed.is_some() {
        return Ok(false);
    }
    Ok(department_id == parent_department_id)
}

/// The company root department and the person who heads it — `(root_id, ceo)`.
///
/// This IS the definition of the CEO, and of the one department that never
/// moves. The guard family already found the CEO this way, buried inside
/// [`executive_root_unit_ids`]; it is a named accessor now because the
/// structural guards ask for exactly this and nothing else.
/// [`root_department_head`] is the same read, narrowed to the person.
/// `None` only when the company has no root department (no manifest).
///
/// # Errors
/// Propagates any `rusqlite` failure; a missing row is `None`, never an error.
pub fn company_root(
    tx: &Transaction<'_>,
    slug: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT id, head_person_id FROM departments \
         WHERE slug = ?1 AND parent_id IS NULL",
        params![slug],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

// TOMBSTONE (2026-08-13): `executive_root_unit_ids`. It computed the protected
// REGION — the company root plus the ancestor chains of the CEO's home and
// assigned units and the `office-of-the-ceo` chain — and every guard that read
// it refused people for WHERE THEY SAT. The operator's ruling is that a head
// may act on anyone in its own subtree and the CEO holds every tree, so the
// only legitimate questions are `org_ops::is_ceo` and
// `org_ops::department_is_company_root`, both of which read [`company_root`].
// The set is deleted rather than left unused: a region nobody protects but
// anybody can still compute is how the next guard grows one back. Its doc also
// claimed parity with a TypeScript `executiveRootUnitIds` that #751 deleted and
// #1035 parked, and that phantom parity is what argued the set into its shape
// and then argued against narrowing it. `ExecutiveRootIsNotExempt.test.ts`
// fences the TypeScript side.

// ---------------------------------------------------------------------------
// Shared manifest WRITE accessors for the atomic org-op family (org_ops).
// The verbs COMPOSE these instead of naming departments/people in raw SQL
// (the store-containment contract). Direct organization operations own this suite as the
// reference impl; create_department/transfer/reparent compose these + add their
// own insert_department/set_parent specifics. Each returns the `EventTouch` for
// the row it changed (`upsert`, family CRUD vocab) so the caller collects them
// for one atomic direct operation. `at` is the caller's clock (updates bump updated_at).
// ---------------------------------------------------------------------------

/// Re-point a department's head. UPDATE `departments.head_person_id`.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn set_department_head(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    head_person_id: &str,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "UPDATE departments SET head_person_id = ?3, updated_at = ?4 \
         WHERE slug = ?1 AND id = ?2",
        params![slug, department_id, head_person_id, at],
    )?;
    Ok(EventTouch::new("department", department_id, "upsert", "departments", slug))
}

/// Set a person's `kind` (worker ↔ head ↔ executive). UPDATE `people.kind`.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn set_person_kind(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    kind: PersonKind,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "UPDATE people SET kind = ?3, updated_at = ?4 WHERE slug = ?1 AND id = ?2",
        params![slug, person_id, person_kind_text(kind), at],
    )?;
    Ok(EventTouch::new("person", person_id, "upsert", "people", slug))
}

/// Move a person to another department (R4 demote / H1 transfer).
///
/// One department argument, because there is one column. It took two — a home
/// and an assignment — while a loan could separate them, and every caller but
/// `loan_person` passed the same value twice.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn move_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    department_id: &str,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "UPDATE people SET department_id = ?3, updated_at = ?4 WHERE slug = ?1 AND id = ?2",
        params![slug, person_id, department_id, at],
    )?;
    Ok(EventTouch::new("person", person_id, "upsert", "people", slug))
}

// ---- Guard READ accessors for appoint_department_head (H2) -----------------

/// A department's current head, or `None` when the department does not exist.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_head(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT head_person_id FROM departments WHERE slug = ?1 AND id = ?2",
        params![slug, department_id],
        |r| r.get(0),
    )
    .optional()
}

/// The CEO — the head of the root department (`parent_id IS NULL`), or `None`
/// when the company has no manifest.
///
/// The root is the one department with no parent (schema invariant), and its
/// head is the CEO. Read here rather than derived from `ROOT_DEPARTMENT_ID` so
/// the answer stays correct for a company whose root carries another id.
///
/// One view of [`company_root`] rather than a second query of the same row:
/// two definitions of "which department is the root" is exactly how a
/// structural guard and a destructive one came to disagree about who the CEO
/// is.
///
/// # Errors
/// Propagates any `rusqlite` failure; a missing row is `None`, never an error.
pub fn root_department_head(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<Option<String>> {
    Ok(company_root(tx, slug)?.map(|(_, head_person_id)| head_person_id))
}

/// A person's `(employment_state, department_id)`,
/// or `None` when the person has no row.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_placement(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT employment_state, department_id FROM people WHERE slug = ?1 AND id = ?2",
        params![slug, person_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// A person's stored structural kind, or `None` when absent.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_kind(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT kind FROM people WHERE slug = ?1 AND id = ?2",
        params![slug, person_id],
        |row| row.get(0),
    )
    .optional()
}

/// The department currently headed by a person, or `None`.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_headed_by_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT id FROM departments WHERE slug = ?1 AND head_person_id = ?2 LIMIT 1",
        params![slug, person_id],
        |row| row.get(0),
    )
    .optional()
}

/// The id of a department this person heads OTHER than `except_department_id`,
/// or `None`. Used for the `already-heads-elsewhere` refusal.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn person_heads_department_other_than(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    except_department_id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT id FROM departments \
         WHERE slug = ?1 AND head_person_id = ?2 AND id <> ?3 LIMIT 1",
        params![slug, person_id, except_department_id],
        |r| r.get(0),
    )
    .optional()
}

// ---- org_ops create_department write/read accessors (P1-a) ---------------
// Manifest store owns every INSERT/UPDATE of its rows; the verb composes
// these and never names departments/people in raw SQL (store containment).

/// The number of department rows for `slug` — the append-ordinal for a NEW
/// department. `departments.ordinal` is the whole-tree `department_order`
/// position; appending at `count` keeps the ordinals a gapless 0..N bijection
/// (H1's departmental sibling — never `MAX(ordinal)+1`, which would leave a gap
/// after a removal). Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_count(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<i64> {
    tx.query_row("SELECT COUNT(*) FROM departments WHERE slug = ?1", params![slug], |row| {
        row.get(0)
    })
}

/// The number of people rows for `slug` — the append-ordinal for a NEWLY hired
/// person. `people.ordinal` is the per-company bijection (H1); appending at
/// `count` keeps it a gapless 0..N bijection. Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn people_count(tx: &Transaction<'_>, slug: &str) -> rusqlite::Result<i64> {
    tx.query_row("SELECT COUNT(*) FROM people WHERE slug = ?1", params![slug], |row| row.get(0))
}

/// A department's `state` text (`"active"` | `"paused"`), or `None` when no such
/// department exists. Composed by `create_department` for the `unknown-parent`
/// (None) and `parent-paused` (Some("paused")) refusals — the row-model half of
/// `assertActiveDestination`. Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure; a missing row is `None`, never an error.
pub fn department_state(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT state FROM departments WHERE slug = ?1 AND id = ?2",
        params![slug, id],
        |row| row.get(0),
    )
    .optional()
}

/// Insert a NEW department or contract row at `ordinal` (its `department_order`
/// position), returning the `department` [`EventTouch`]. `head_person_id` is the
/// explicit head decision (R3) — for hire-new the caller inserts that person in
/// the same txn via [`insert_person_minimal`]; for appoint-existing it is a
/// person who already has a row. `state` is the created unit's state (normally
/// `"active"`). Contract metadata is present exactly for `kind = contract`.
///
/// # Errors
/// Any `rusqlite` failure (a duplicate `id` is a real caller error, surfaced —
/// `create_department` refuses it BEFORE calling this).
#[allow(clippy::too_many_arguments)]
pub fn insert_department(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    parent_id: &str,
    name: &str,
    purpose: &str,
    kind: UnitKind,
    transient: Option<&ContractMetadata>,
    state: &str,
    head_person_id: &str,
    ordinal: i64,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    let (kind, engagement, launched_at, expires_at) = match (kind, transient) {
        (UnitKind::Department, None) => ("department", None, None, None),
        (UnitKind::Contract, Some(contract)) => (
            "contract",
            Some(contract.engagement.as_str()),
            Some(contract.launched_at.as_str()),
            contract.expires_at.as_deref(),
        ),
        _ => unreachable!("atomic department creation validates unit metadata before insertion"),
    };
    tx.execute(
        "INSERT INTO departments(slug, id, parent_id, name, purpose, kind, state, \
         head_person_id, contract_engagement, contract_launched_at, contract_expires_at, \
         ordinal, created_at, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            slug,
            id,
            parent_id,
            name,
            purpose,
            kind,
            state,
            head_person_id,
            engagement,
            launched_at,
            expires_at,
            ordinal,
            at,
        ],
    )?;
    Ok(EventTouch::new("department", id, "upsert", "departments", slug))
}

/// Insert a NEWLY hired head person (R3 hire-new) at `ordinal`, placed in
/// the department being created, returning the `person` [`EventTouch`]. A minimal
/// scalar seed — `employment_state = 'active'`, `activation` at its `'resident'`
/// default, no child rows (tools/resources/prompts); the FULL seed contract is
/// P2-f `hire_person`. This accessor writes no runtime state; the fence row that
/// brings the hire up is `launch_intent_rows::insert_person_fence`, written by
/// the same transaction (was: launch_intent untouched — THE
/// HARD RULE): the person is durable-only until work arrives.
///
/// # Errors
/// Any `rusqlite` failure (a duplicate `id` is a real caller error, surfaced).
#[allow(clippy::too_many_arguments)]
pub fn insert_person_minimal(
    tx: &Transaction<'_>,
    slug: &str,
    id: &str,
    name: &str,
    title: &str,
    mandate: &str,
    kind: &str,
    department_id: &str,
    ordinal: i64,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, \
         department_id, ordinal, created_at, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?9)",
        params![slug, id, name, title, mandate, kind, department_id, ordinal, at],
    )?;
    Ok(EventTouch::new("person", id, "upsert", "people", slug))
}

/// Set a person's `employment_state` (active / benched / departed). UPDATE
/// `people.employment_state`. Shared suite addition for bench/recall/offboard.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn set_employment_state(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    state: EmploymentState,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "UPDATE people SET employment_state = ?3, updated_at = ?4 WHERE slug = ?1 AND id = ?2",
        params![slug, person_id, employment_text(state), at],
    )?;
    Ok(EventTouch::new("person", person_id, "upsert", "people", slug))
}

/// The durable seed for a NEW hire — every persisted PersonRecord field an
/// atomic `hire_person` inserts. Placed in the hiring department (set by
/// [`insert_person`], not carried here). `activation` is `resident`/`on-demand`
/// (it does not decide whether the hire comes up: the launch fence
/// `org_ops::hire_person` writes for an active seed does). `tools` are the grant
/// rows, written verbatim —
/// every kind may hold `bash` (operator decision, 2026-08-10).
#[derive(Debug, Clone)]
pub struct NewPersonSeed<'a> {
    /// Display name (`people.name`, NOT NULL).
    pub name: &'a str,
    /// Role title (`people.title`, NOT NULL).
    pub title: &'a str,
    /// The person's mandate (`people.mandate`, NOT NULL).
    pub mandate: &'a str,
    /// worker / head / executive (`people.kind`).
    pub kind: PersonKind,
    /// active / benched (`people.employment_state`). A hire never starts a
    /// pane regardless of this roster state.
    pub employment_state: EmploymentState,
    /// `resident` (default) | `on-demand` (`people.activation`).
    pub activation: &'a str,
    /// Tool grants (child `person_tools` rows), written exactly as declared —
    /// every kind may hold `bash` (operator decision, 2026-08-10).
    ///
    /// TOMBSTONE (chief-home-is-cwd §4e): the `skills`/`extensions`/`packages`
    /// id lists stood here. A hire selects no Pi resource — the skills an agent
    /// has are whatever is in `<dir>/.pi/skills` when Pi looks — so a seed that
    /// carried them was describing a decision nobody makes.
    pub tools: &'a [String],
    /// Project-local prompt template ids.
    pub prompts: &'a [String],
}

/// INSERT a brand-new person into a department (the manifest half of the atomic
/// `hire_person`). Writes the `people` row (placed in `department_id`,
/// with the supplied active/benched employment state) at the NEXT gapless
/// ordinal (`MAX(ordinal)+1`;
/// [`org_ops::hire_person`] composes [`refresh_people_order`] afterward so the
/// whole 0..N bijection is re-asserted) plus its `person_tools` child rows
/// (written verbatim — no tool is filtered by kind). Returns the `person`
/// `upsert` touch. Shared suite addition for `hire_person`; mirrors the
/// reconstruct/publish people INSERT column set.
///
/// # Errors
/// Propagates any `rusqlite` failure (e.g. a duplicate `id` — the caller guards
/// `duplicate-person-id` before the fence).
pub fn insert_person(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    department_id: &str,
    seed: &NewPersonSeed<'_>,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, \
         department_id, activation, ordinal, created_at, updated_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9, \
         (SELECT COALESCE(MAX(ordinal), -1) + 1 FROM people WHERE slug = ?1),?10,?10)",
        params![
            slug,
            person_id,
            seed.name,
            seed.title,
            seed.mandate,
            person_kind_text(seed.kind),
            employment_text(seed.employment_state),
            department_id,
            seed.activation,
            at,
        ],
    )?;
    // `ordinal` preserves tool array order and satisfies the UNIQUE
    // (slug,person_id,ordinal) index.
    for (ordinal, tool) in seed.tools.iter().enumerate() {
        tx.execute(
            "INSERT INTO person_tools(slug, person_id, ordinal, tool) VALUES(?1,?2,?3,?4)",
            params![slug, person_id, ordinal as i64, tool],
        )?;
    }
    for (ordinal, template) in seed.prompts.iter().enumerate() {
        tx.execute(
            "INSERT INTO person_prompts(slug, person_id, ordinal, template) VALUES(?1,?2,?3,?4)",
            params![slug, person_id, ordinal as i64, template],
        )?;
    }
    Ok(EventTouch::new("person", person_id, "upsert", "people", slug))
}

/// The ORDINARY members of one department: everybody assigned to it who is not
/// its head and has not departed — in `people_order`
/// position.
///
/// This is the set `move_department_members` moves when a caller names no
/// explicit batch, and it exists here rather than in a client for the same
/// reason department ids are minted here (#751/R3): "who counts as an ordinary
/// member" is a rule about these rows, and every copy of it in a caller is a
/// place it drifts. The three exclusions are not policy invented for this
/// query — they are exactly what `validate_mover` would refuse the batch for
/// (`head-needs-successor`, `person-departed`), so a derived
/// batch can never be refused for containing somebody it should not have.
///
/// Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_ordinary_members(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT p.id FROM people p WHERE p.slug = ?1 AND p.department_id = ?2 AND p.employment_state <> 'departed' AND p.id IS NOT ( SELECT d.head_person_id FROM departments d WHERE d.slug = ?1 AND d.id = ?2 ) ORDER BY p.ordinal",
    )?;
    let rows = stmt.query_map(params![slug, department_id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Every person assigned into a department's SUBTREE — the department itself
/// plus all its recursive descendant departments (`parent_id` chain). Returns
/// the sorted `people.id` set (all employment states; a departed member with no
/// launch-intent/open-transition is a harmless no-op for the sweep). Read-only —
/// composed by `pause_department` to clear the paused subtree's launch-intent
/// fences and supersede its open transitions IN the same txn (#534: an uncleared
/// launch_intent re-creeps a pane, so the clear must be atomic, not reactive).
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_subtree_members(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "WITH RECURSIVE subtree(id) AS ( SELECT id FROM departments WHERE slug = ?1 AND id = ?2 UNION ALL SELECT d.id FROM departments d JOIN subtree s ON d.parent_id = s.id WHERE d.slug = ?1 ) SELECT p.id FROM people p JOIN subtree s ON p.department_id = s.id WHERE p.slug = ?1 ORDER BY p.id",
    )?;
    let rows = stmt.query_map(params![slug, department_id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// The target department and every descendant, deepest child first. This is
/// the exact delete order required by the self-referential department FK.
/// Read-only; named removal operations validate all current rows before using
/// the returned identities in their one atomic write.
pub fn department_subtree_ids_descending(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "WITH RECURSIVE subtree(id, depth) AS ( \
             SELECT id, 0 FROM departments WHERE slug = ?1 AND id = ?2 \
             UNION ALL \
             SELECT d.id, s.depth + 1 FROM departments d JOIN subtree s ON d.parent_id = s.id \
             WHERE d.slug = ?1 \
         ) \
         SELECT id FROM subtree ORDER BY depth DESC, id",
    )?;
    let rows = stmt.query_map(params![slug, department_id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Every person placed in `department_ids`, as `(person_id, department_id)`.
///
/// The department comes back with the person because the caller — the subtree
/// removal — has to record the exact unit each leaver LEFT in the staffing
/// ledger, and that is a descendant, not the parent they are re-homed to.
/// It used to come back for a second reason as well: the row carried the
/// ASSIGNED unit while the filter matched on HOME, so the caller compared the
/// two to spot a borrowed person. There is one column now, so the value is an
/// answer rather than half of a comparison.
///
/// The caller supplies the exact subtree identities it read in the same SQLite
/// transaction.
pub fn people_in_departments(
    tx: &Transaction<'_>,
    slug: &str,
    department_ids: &[String],
) -> rusqlite::Result<Vec<(String, String)>> {
    if department_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", department_ids.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, department_id FROM people WHERE slug = ? AND department_id IN ({placeholders}) ORDER BY ordinal, id"
    );
    let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(department_ids.len() + 1);
    values.push(&slug);
    for id in department_ids {
        values.push(id);
    }
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt.query_map(values.as_slice(), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect()
}

// TOMBSTONE: `delete_person` — one person's `people` row plus their
// `person_tools`/`person_prompts` children, deleted
// outright. Its only caller was `org_ops::remove_department_tree`, which now
// OFFBOARDS the removed subtree's people instead (departed, re-homed to the
// parent, `staffing_history 'offboarded'`) — the same durable departure
// `offboard_person` writes, through the one shared `depart_person_rows`. There
// is deliberately no named verb left for erasing a person: a hire followed by a
// departure is a fact about the company, and the hard delete left the ledger
// actively wrong (an orphaned `hired` entry with no `offboarded` entry and
// nobody it belongs to). The whole-manifest publish diff (`diff_people`) still
// reconciles a person the incoming manifest omits; that is a different
// operation with a different authority, not a fired person.

/// Delete one department after its people have departed and its descendants
/// have been removed. Callers use [`department_subtree_ids_descending`] so the
/// self-referential department FK is always satisfied.
pub fn delete_department(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<EventTouch> {
    tx.execute(
        "DELETE FROM departments WHERE slug = ?1 AND id = ?2",
        params![slug, department_id],
    )?;
    Ok(EventTouch::new("department", department_id, "delete", "departments", slug))
}

/// Flip a department's `state` between `'active'` and `'paused'` (the
/// pause/resume verbs' one manifest write). UPDATE `departments.state`,
/// mirroring `set_department_head`; `at` drives `updated_at`. Returns the
/// `department` upsert `EventTouch`. The `'paused'`/`'active'` vocab is the SAME
/// column the `destination-paused` transfer gate reads — a paused dept refuses
/// transfers into it with no further wiring. Shared suite addition (settle-ux).
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn set_department_paused(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    paused: bool,
    at: &str,
) -> rusqlite::Result<EventTouch> {
    let state = if paused { "paused" } else { "active" };
    tx.execute(
        "UPDATE departments SET state = ?3, updated_at = ?4 WHERE slug = ?1 AND id = ?2",
        params![slug, department_id, state, at],
    )?;
    Ok(EventTouch::new("department", department_id, "upsert", "departments", slug))
}

#[cfg(test)]
mod manifest_genesis_tests {
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use crate::test_support::northstar_manifest;
    use rusqlite::Connection;

    const EPOCH: i64 = 1_784_116_800_000;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("company schema");
        conn
    }

    #[test]
    fn genesis_creates_once_and_never_exposes_or_overwrites_the_incumbent() {
        let mut conn = open();
        let manifest = northstar_manifest(EPOCH);
        let slug = manifest.slug.clone();

        let tx = conn.transaction().expect("first transaction");
        assert!(matches!(
            genesis(&tx, &slug, &manifest).expect("first genesis"),
            ManifestGenesisOutcome::Created
        ));
        tx.commit().expect("commit first genesis");

        let mut contender = manifest.clone();
        contender.purpose = "Attempted stale overwrite".to_string();
        let tx = conn.transaction().expect("second transaction");
        assert!(matches!(
            genesis(&tx, &slug, &contender).expect("duplicate genesis result"),
            ManifestGenesisOutcome::AlreadyExists
        ));
        let incumbent =
            reconstruct(&tx, &slug).expect("read incumbent").expect("incumbent remains");
        assert_eq!(incumbent.purpose, manifest.purpose);
        tx.commit().expect("commit duplicate observation");
    }
}

#[cfg(test)]
mod manifest_write_accessor_tests {
    use super::*;

    fn open() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE departments(slug TEXT, id TEXT, head_person_id TEXT, updated_at TEXT);
             CREATE TABLE people(slug TEXT, id TEXT, kind TEXT, department_id TEXT, updated_at TEXT);
             INSERT INTO departments VALUES('acme','eng','emery','t0');
             INSERT INTO people VALUES('acme','quinn','worker','eng','t0');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn set_department_head_repoints_and_touches() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let touch = set_department_head(&tx, "acme", "eng", "quinn", "t1").unwrap();
        let (head, at): (String, String) = tx
            .query_row(
                "SELECT head_person_id, updated_at FROM departments WHERE id='eng'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(head, "quinn");
        assert_eq!(at, "t1");
        assert_eq!(touch.entity, "department");
        assert_eq!(touch.op, "upsert");
        tx.commit().unwrap();
    }

    #[test]
    fn set_person_kind_and_move_person_update_and_touch() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        let k = set_person_kind(&tx, "acme", "quinn", PersonKind::Head, "t1").unwrap();
        let m = move_person(&tx, "acme", "quinn", "executive", "t2").unwrap();
        let (kind, department_id): (String, String) = tx
            .query_row("SELECT kind, department_id FROM people WHERE id='quinn'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(kind, "head");
        assert_eq!(department_id, "executive");
        assert_eq!(k.entity, "person");
        assert_eq!(m.entity, "person");
        assert!(k.op == "upsert" && m.op == "upsert");
        tx.commit().unwrap();
    }

    /// `bash` is an ordinary tool for every kind (operator decision,
    /// 2026-08-10: "Everybody should have bash... Every agent should have a
    /// bash"). This replaces `strip_manager_bash_tools_removes_bash_only_and_is_idempotent`,
    /// which asserted the opposite: that appointing someone head DELETED their
    /// `bash` row. The stripper is gone, so the contract to pin is that a
    /// non-worker's declared `bash` survives being written.
    #[test]
    fn a_non_worker_seed_keeps_its_declared_bash_on_insert() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).unwrap();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        let tx = conn.transaction().unwrap();
        let tools = ["read".to_string(), "bash".to_string(), "edit".to_string()];
        let seed = NewPersonSeed {
            name: "Quinn",
            title: "Head of Platform",
            mandate: "lead",
            kind: PersonKind::Head,
            employment_state: EmploymentState::Active,
            activation: "resident",
            tools: &tools,
            prompts: &[],
        };
        insert_person(&tx, "acme", "quinn", "platform", &seed, "t").unwrap();
        let stored: Vec<String> = tx
            .prepare(
                "SELECT tool FROM person_tools WHERE slug='acme' AND person_id='quinn' \
                 ORDER BY ordinal",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(stored, vec!["read", "bash", "edit"], "a head's bash must survive the insert");
        tx.commit().unwrap();
    }

    #[test]
    fn append_staffing_history_allocates_per_slug_seq() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE counters(name TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE staffing_history(slug TEXT, seq INTEGER, person_id TEXT, action TEXT, \
               from_department_id TEXT, to_department_id TEXT, reason TEXT, at TEXT, PRIMARY KEY(slug, seq));",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        append_staffing_history(
            &tx,
            "acme",
            "quinn",
            "appointed-head",
            Some("eng"),
            Some("eng"),
            "promoted",
            "t1",
        )
        .unwrap();
        append_staffing_history(
            &tx,
            "acme",
            "emery",
            "stepped-down",
            Some("eng"),
            None,
            "replaced",
            "t1",
        )
        .unwrap();
        let rows: Vec<(i64, String, String)> = tx
            .prepare("SELECT seq, person_id, action FROM staffing_history ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "quinn".to_string(), "appointed-head".to_string()),
                (2, "emery".to_string(), "stepped-down".to_string()),
            ]
        );
        tx.commit().unwrap();
    }

    #[test]
    fn refresh_people_order_compacts_gaps_and_touches_only_moved() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE departments(slug TEXT, id TEXT, ordinal INTEGER, head_person_id TEXT, PRIMARY KEY(slug, id));
             CREATE TABLE people(slug TEXT, id TEXT, department_id TEXT, ordinal INTEGER, updated_at TEXT, PRIMARY KEY(slug, id));
             INSERT INTO departments VALUES('acme','engineering',0,'');
             INSERT INTO people VALUES('acme','a','engineering',0,'t0'),('acme','b','engineering',2,'t0'),('acme','c','engineering',5,'t0');",
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let touches = super::refresh_people_order(&tx, "acme", "t1").unwrap();
        let ords: Vec<(String, i64)> = tx
            .prepare("SELECT id, ordinal FROM people ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ords, vec![("a".to_string(), 0), ("b".to_string(), 1), ("c".to_string(), 2)]);
        // 'a' was already 0 (no touch); 'b' 2->1 and 'c' 5->2 moved.
        assert_eq!(touches.len(), 2);
        assert!(touches.iter().all(|t| t.entity == "person" && t.op == "upsert"));
        tx.commit().unwrap();
    }
}

/// Append one row to the append-only `staffing_history` ledger, allocating its
/// `seq` from the per-slug staffing counter (D2 — never `MAX(seq)+1`). `action`
/// is one of the ledger verbs (`hired`/`benched`/`recalled`/
/// `returned`/`transferred`/`offboarded`/`appointed-head`/`stepped-down`),
/// enforced by the column CHECK. NO `org_events` touch: the staffing ledger is
/// its OWN feed (its own D2 seq), not part of the org_events entity stream.
///
/// # Errors
/// Propagates any `rusqlite` failure (an invalid `action` is a CHECK violation).
#[allow(clippy::too_many_arguments)]
pub fn append_staffing_history(
    tx: &Transaction<'_>,
    slug: &str,
    person_id: &str,
    action: &str,
    from_department_id: Option<&str>,
    to_department_id: Option<&str>,
    reason: &str,
    at: &str,
) -> rusqlite::Result<()> {
    let seq = crate::store::rows_txn::allocate_seq(
        tx,
        &crate::store::rows_txn::staffing_counter_key(slug),
    )?;
    tx.execute(
        "INSERT INTO staffing_history(slug, seq, person_id, action, from_department_id, to_department_id, reason, at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![slug, seq, person_id, action, from_department_id, to_department_id, reason, at],
    )?;
    Ok(())
}

/// Recompute the canonical 0-based `people.ordinal` order (invariant 2): people
/// are grouped by their department in department-preorder, each
/// department's head is first, and remaining members retain their relative
/// order. This exactly mirrors the launcher's `refreshPeopleOrder`, while also
/// compacting gaps. Emits a person `upsert` touch ONLY for rows whose ordinal
/// actually moved (no churn for a no-op).
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn refresh_people_order(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
) -> rusqlite::Result<Vec<EventTouch>> {
    use std::collections::{HashMap, HashSet};

    let department_rank: HashMap<String, i64> = {
        let mut stmt =
            tx.prepare("SELECT id, ordinal FROM departments WHERE slug = ?1 ORDER BY ordinal")?;
        let rows = stmt.query_map(params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<rusqlite::Result<HashMap<String, i64>>>()?
    };
    let heads: HashSet<String> = {
        let mut stmt = tx.prepare("SELECT head_person_id FROM departments WHERE slug = ?1")?;
        let rows = stmt.query_map(params![slug], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<HashSet<String>>>()?
    };
    let mut people: Vec<(String, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT id, department_id, ordinal FROM people WHERE slug = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    people.sort_by(|left, right| {
        let left_rank = department_rank.get(&left.1).copied().unwrap_or(i64::MAX);
        let right_rank = department_rank.get(&right.1).copied().unwrap_or(i64::MAX);
        left_rank
            .cmp(&right_rank)
            .then_with(|| (!heads.contains(&left.0)).cmp(&(!heads.contains(&right.0))))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.0.cmp(&right.0))
    });

    // Vacate the unique `(slug, ordinal)` namespace before a real reorder.
    tx.execute("UPDATE people SET ordinal = -1 - ordinal WHERE slug = ?1", params![slug])?;

    let mut touches = Vec::new();
    for (index, (id, _, old_ordinal)) in people.iter().enumerate() {
        let target = index as i64;
        let moved = *old_ordinal != target;
        tx.execute(
            "UPDATE people SET ordinal = ?3, \
             updated_at = CASE WHEN ?4 THEN ?5 ELSE updated_at END \
             WHERE slug = ?1 AND id = ?2",
            params![slug, id, target, moved, at],
        )?;
        if moved {
            touches.push(EventTouch::new("person", id, "upsert", "people", slug));
        }
    }
    Ok(touches)
}

// ---------------------------------------------------------------------------
// Reparent / reorg accessors for the atomic org-op family (org_ops). The
// manifest store OWNS every `departments` read/write; org_ops composes these
// and never names the table in raw SQL (fable's store-containment contract).
//
// `department_parent_map` + `would_create_cycle` + `refresh_department_order`
// are SHARED with the create/move verbs (additive, reuse-don't-duplicate — a
// consolidation fold keeps both callers).
// ---------------------------------------------------------------------------

/// The temporary ordinal a just-reparented department is parked at so it sorts
/// LAST among its new siblings before [`refresh_department_order`] normalizes
/// the whole tree back to a gapless preorder bijection. Chosen far above any
/// real dept count so it never collides with a live sibling ordinal.
const REPARENT_APPEND_SENTINEL: i64 = 1_000_000_000;

/// A department's `(parent_id, state)`, or `None` when the id has no row.
/// Read-only — the existence/paused/ root refusal reads compose this.
///
/// # Errors
/// Propagates any `rusqlite` failure; a missing row is `None`, never an error.
pub fn department_parent_state(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
) -> rusqlite::Result<Option<(Option<String>, String)>> {
    use rusqlite::OptionalExtension;
    tx.query_row(
        "SELECT parent_id, state FROM departments WHERE slug = ?1 AND id = ?2",
        params![slug, department_id],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()
}

/// `id -> parent_id` for every department in the company (root maps to `None`).
/// The one read the cycle walk and the preorder recompute both build on.
/// Read-only.
///
/// # Errors
/// Propagates any `rusqlite` failure.
pub fn department_parent_map(
    tx: &Transaction<'_>,
    slug: &str,
) -> rusqlite::Result<std::collections::HashMap<String, Option<String>>> {
    use std::collections::HashMap;
    let mut parent: HashMap<String, Option<String>> = HashMap::new();
    let mut stmt = tx.prepare("SELECT id, parent_id FROM departments WHERE slug = ?1")?;
    let rows = stmt.query_map(params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, parent_id) = row?;
        parent.insert(id, parent_id);
    }
    Ok(parent)
}

/// Would making `candidate` the parent of `department_id` create a cycle? True
/// when `candidate == department_id` (self-parent, the degenerate cycle) OR
/// `department_id` is an ancestor of `candidate` (reparenting under one's own
/// descendant). Pure walk over the pre-read `parent_map`; O(depth), visited-set
/// guarded against a pre-existing malformed cycle.
#[must_use]
pub fn would_create_cycle(
    parent_map: &std::collections::HashMap<String, Option<String>>,
    department_id: &str,
    candidate: &str,
) -> bool {
    use std::collections::HashSet;
    let mut cursor = Some(candidate.to_string());
    let mut visited: HashSet<String> = HashSet::new();
    while let Some(id) = cursor {
        if id == department_id {
            return true;
        }
        if !visited.insert(id.clone()) {
            break; // a pre-existing cycle not involving department_id
        }
        cursor = parent_map.get(&id).cloned().flatten();
    }
    false
}

/// Re-point a department to `new_parent_id` and PARK it at the append sentinel
/// ordinal (so [`refresh_department_order`] slots it last among its new
/// siblings, then normalizes the tree). Sets `updated_at = at`. Returns the
/// department `update` touch (the parent_id change).
///
/// The moved dept's ordinal necessarily changes off the sentinel, so
/// [`refresh_department_order`] would also produce a touch for it — the caller
/// (`org_ops::reparent_department`) DEDUPES: it keeps this touch and drops the
/// refresh's duplicate for the same id, so exactly one org_events row is emitted
/// per department. Composes no cross-store SQL.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn set_parent(
    tx: &Transaction<'_>,
    slug: &str,
    department_id: &str,
    new_parent_id: &str,
    at: &str,
) -> rusqlite::Result<crate::store::rows_txn::EventTouch> {
    tx.execute(
        "UPDATE departments SET parent_id = ?3, ordinal = ?4, updated_at = ?5 \
         WHERE slug = ?1 AND id = ?2",
        params![slug, department_id, new_parent_id, REPARENT_APPEND_SENTINEL, at],
    )?;
    Ok(crate::store::rows_txn::EventTouch::new(
        "department",
        department_id,
        "upsert",
        "departments",
        slug,
    ))
}

/// Recompute the WHOLE-TREE preorder `ordinal` bijection (root first, then each
/// node's children in current-ordinal order) and rewrite every department whose
/// ordinal changed — the H1 guarantee that after commit the dept ordinals are a
/// gapless `0..n-1` bijection matching the manifest publish's
/// `department_order` enumerate. Emits one `update` touch PER department whose
/// ordinal actually moved (untouched depts stay silent — no spurious feed).
///
/// Two-pass to dodge the `(slug, parent_id, ordinal)` unique index: pass 1
/// parks every ordinal at a distinct negative (originals are per-parent unique,
/// so negation stays unique); pass 2 writes the final preorder index.
///
/// # Errors
/// Any `rusqlite` failure.
pub fn refresh_department_order(
    tx: &Transaction<'_>,
    slug: &str,
    at: &str,
) -> rusqlite::Result<Vec<crate::store::rows_txn::EventTouch>> {
    use std::collections::BTreeMap;

    // Read (id, parent_id, current_ordinal) in ordinal order — the same order
    // reconstruct() feeds preorder_departments, so the derived order matches.
    let mut current: BTreeMap<String, i64> = BTreeMap::new();
    let mut children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id, parent_id, ordinal FROM departments WHERE slug = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![slug], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (id, parent_id, ordinal) = row?;
            current.insert(id.clone(), ordinal);
            children.entry(parent_id).or_default().push(id);
        }
    }

    // Preorder DFS: root(s) first (parent None), children in ORDER BY ordinal.
    let mut order: Vec<String> = Vec::new();
    let mut stack: Vec<String> = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    while let Some(id) = stack.pop() {
        order.push(id.clone());
        if let Some(kids) = children.get(&Some(id)) {
            for kid in kids.iter().rev() {
                stack.push(kid.clone());
            }
        }
    }

    // Pass 1: park every ordinal at a distinct negative to clear the unique
    // index's namespace before writing the final indices.
    tx.execute("UPDATE departments SET ordinal = -1 - ordinal WHERE slug = ?1", params![slug])?;

    // Pass 2: write the final preorder index; touch/update timestamps only for
    // departments that moved.
    let mut touches = Vec::new();
    for (idx, id) in order.iter().enumerate() {
        let target = idx as i64;
        let moved = current.get(id).copied() != Some(target);
        tx.execute(
            "UPDATE departments SET ordinal = ?3, \
             updated_at = CASE WHEN ?4 THEN ?5 ELSE updated_at END \
             WHERE slug = ?1 AND id = ?2",
            params![slug, id, target, moved, at],
        )?;
        if moved {
            touches.push(crate::store::rows_txn::EventTouch::new(
                "department",
                id.clone(),
                "upsert",
                "departments",
                slug,
            ));
        }
    }
    Ok(touches)
}

#[cfg(test)]
mod move_family_accessor_tests {
    //! H1 move-family read accessor coverage (transfer / move-members compose
    //! these): `department_state` (the destination gate this module adds) plus a
    //! proof that the SHARED `refresh_people_order` primitive densifies the
    //! whole-company ordinal into a gapless bijection inside the txn.
    use super::*;
    use crate::schema::COMPANY_SCHEMA_SQL;
    use rusqlite::Connection;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(COMPANY_SCHEMA_SQL).expect("schema");
        conn.pragma_update(None, "foreign_keys", false).expect("fk off");
        conn.execute_batch(
            "INSERT INTO departments(slug, id, parent_id, name, kind, state, head_person_id, ordinal, created_at, updated_at) VALUES ('acme','root',NULL,'Root','company','active','ada',0,'t','t'), ('acme','eng','root','Eng','department','active','bo',0,'t','t'), ('acme','park','root','Park','department','paused','pz',1,'t','t'); INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, department_id, ordinal, created_at, updated_at) VALUES ('acme','ada','Ada','CEO','lead','executive','active','root',0,'t','t'), ('acme','bo','Bo','Eng','build','head','active','eng',1,'t','t'), ('acme','cy','Cy','Eng','build','worker','active','eng',2,'t','t');",
        )
        .expect("seed");
        conn
    }

    #[test]
    fn department_state_reports_active_paused_and_unknown() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(department_state(&tx, "acme", "eng").unwrap().as_deref(), Some("active"));
        assert_eq!(department_state(&tx, "acme", "park").unwrap().as_deref(), Some("paused"));
        assert!(department_state(&tx, "acme", "nope").unwrap().is_none());
        tx.commit().unwrap();
    }

    #[test]
    fn refresh_people_order_densifies_a_gap_to_a_gapless_bijection() {
        let mut conn = open();
        // Introduce a gap: ada=0, bo=1, cy=7.
        conn.execute("UPDATE people SET ordinal=7 WHERE slug='acme' AND id='cy'", []).unwrap();
        let tx = conn.transaction().unwrap();
        let touches = refresh_people_order(&tx, "acme", "t2").unwrap();
        let got: Vec<i64> = tx
            .prepare("SELECT ordinal FROM people WHERE slug='acme' ORDER BY ordinal")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(got, vec![0, 1, 2], "ordinals must be a gapless 0..N-1 bijection");
        // Only cy actually changed (7 -> 2).
        assert_eq!(
            touches.iter().map(|t| t.entity_id.clone()).collect::<Vec<_>>(),
            vec!["cy".to_string()]
        );
        tx.commit().unwrap();
    }
}
