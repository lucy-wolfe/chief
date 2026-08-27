//! Company genesis: a spec in, a validated manifest out.
//!
//! Port of `normalizeOrganizationSpec` / `normalizePersonSeed` /
//! `normalizeDepartmentUnitFields` / `defaultTools` / `slugify` from
//! `apps/cli/src/legacy/organization/org-types.ts` (deleted by this change).
//!
//! # Why this had to move, and why it was nearly missed
//!
//! `POST /v1/org/manifest/genesis-with-models` took a **pre-normalized**
//! manifest string. That meant the launcher decided every id, every default
//! tool grant, every employment state and the whole department tree, and
//! chiefd merely stored the result — so the single most consequential decision
//! in the product, what a company IS at birth, was still TypeScript's. Deleting
//! `org-types.ts` without this port would have left the genesis route with no
//! caller able to build its input.
//!
//! # There is no default tool grant any more
//!
//! This module used to own `default_tools`, a per-kind/per-task-class table
//! written into each person's record at genesis — and ONLY at genesis, which
//! is why the same omission on `org_hire` produced people who could not open a
//! file. Both halves are gone. The Pi builtin floor is now composed for every
//! person, on every path, by
//! `chiefd_host::converge_apply::resource_catalog::person_tool_names`
//! (operator decision, 2026-08-10: "every agent should have those tools. Do
//! not block them"). A person's stored `tools` holds only what their spec
//! actually asked for.
//!
//! # Ordering is part of the output, not an incidental
//!
//! `department_order` is insertion order — the root first, then each
//! department in spec order, depth-first. `people_order` is the CEO, then each
//! department's head followed by its staff. The activity ledger, the roster
//! projection and the identity-accent allocator all read those orders as
//! canonical, so producing them in a different order produces a different
//! company.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::Refusal;
use crate::store::organization::{
    validate_organization_manifest, ContractMetadata, DepartmentRecord, EmploymentState,
    OrganizationManifest, OrganizationPolicy, PersonKind, PersonRecord, UnitKind, UnitState,
    MANIFEST_INVALID, ORGANIZATION_SCHEMA_VERSION, ROOT_DEPARTMENT_ID,
};

/// Refusal code for a spec chiefd will not turn into a company.
pub const SPEC_INVALID: &str = "organization-spec-invalid";

/// The maximum characters a derived slug keeps.
const MAX_SLUG_CHARS: usize = 48;

/// The full Pi builtin toolset. Granted whole only to the executive.
pub const BUILTIN_TOOLS: &[&str] = &["read", "bash", "edit", "write", "grep", "find", "ls"];

/// The default supervision policy every company starts with.
const SUPERVISION_INTERVAL_MS: i64 = 15 * 60 * 1_000;
const ACKNOWLEDGEMENT_TIMEOUT_MS: i64 = 90 * 1_000;

/// Lowercase, collapse non-alphanumerics to `-`, bound the length, trim the
/// dashes. The company slug and every derived id come from this.
#[must_use]
pub fn slugify(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let mut collapsed = String::with_capacity(lowered.len());
    let mut in_dash = false;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            collapsed.push(ch);
            in_dash = false;
        } else if !in_dash {
            collapsed.push('-');
            in_dash = true;
        }
    }
    // Truncate on a CHARACTER boundary, then trim — the TypeScript twin sliced
    // before trimming, so a truncation that lands mid-dash-run must still lose
    // the dash.
    let bounded: String = collapsed.chars().take(MAX_SLUG_CHARS).collect();
    bounded.trim_matches('-').to_string()
}

/// The longest run of caller-supplied text a refusal echoes back.
///
/// A refusal that enumerates the accepted vocabulary is only useful if it also
/// shows what the caller actually sent, but that value is untrusted request
/// input on a path that writes nothing, so it is bounded exactly like the
/// identifiers it sits beside (the 64-byte entity id) rather than pasted into a
/// log line at whatever length arrived.
pub const MAX_REFUSAL_ECHO_BYTES: usize = 64;

/// `value`, truncated to [`MAX_REFUSAL_ECHO_BYTES`] on a char boundary.
#[must_use]
pub fn bounded(value: &str) -> &str {
    if value.len() <= MAX_REFUSAL_ECHO_BYTES {
        return value;
    }
    let mut end = MAX_REFUSAL_ECHO_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// The first declared tool name that no seed may name, or `None` if every name
/// is declarable.
///
/// **Declaring is narrower than holding, and that is the point.** A person ends
/// up with far more tools than their seed lists: `person_tool_names` composes
/// the intercom, memory, manager, root-executive and loop surfaces in
/// automatically from the person's kind and monitoring state. None of those
/// is ever declared — declaring one is at best redundant, and on the wrong kind
/// it is a grant the person is not entitled to. What a seed legitimately
/// declares is the Pi builtin floor, and nothing else.
///
/// A name outside [`BUILTIN_TOOLS`] is therefore a typo and nothing else — and
/// typos are the whole reason this exists, because `tools: ["bahs"]` used to be
/// accepted in silence and left a person who looked configured and held nothing
/// they asked for.
///
/// TOMBSTONE (chief-home-is-cwd §4e): the `selects_resources` escape hatch.
/// A seed that selected an extension or a package could name the tools that
/// resource exports, and chiefd — resolving no catalog at seed time — deferred
/// the judgement to materialization. A seed selects neither any more, so no
/// seed can export a tool name and there is nothing left to defer.
#[must_use]
pub fn undeclarable_tool(tools: &[String]) -> Option<&str> {
    tools.iter().map(String::as_str).find(|tool| !BUILTIN_TOOLS.contains(tool))
}

/// The one-line vocabulary an undeclarable-tool refusal quotes.
///
/// Note what is NOT promised here: the Pi builtins are no longer something a
/// seed has to ask for. They are composed for every person on every path
/// (operator decision, 2026-08-10). Naming a builtin is still accepted — it is
/// simply redundant — which is why they are listed as such rather than refused,
/// but a caller reading this should not conclude that omitting them costs
/// anything.
#[must_use]
pub fn declarable_tools_sentence(_kind: PersonKind) -> String {
    format!(
        "the Pi builtins ({}) are granted to every person automatically and need not be \
         declared — declaring one is accepted but redundant. They are the only declarable \
         names: a name outside them is a typo, since a seed selects no extension or package \
         that could export one. The org_* \
         surfaces are composed from the person's kind and must NEVER be declared",
        BUILTIN_TOOLS.join(", ")
    )
}

fn invalid(message: impl Into<String>) -> Refusal {
    Refusal::new(SPEC_INVALID, message)
}

fn object<'v>(value: &'v Value, path: &str) -> Result<&'v serde_json::Map<String, Value>, Refusal> {
    value.as_object().ok_or_else(|| invalid(format!("{path} must be an object")))
}

fn text(value: Option<&Value>, path: &str) -> Result<String, Refusal> {
    let raw = value.and_then(Value::as_str).map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Err(invalid(format!("{path} is required")));
    }
    Ok(raw.to_string())
}

fn optional_strings(value: Option<&Value>, path: &str) -> Result<Vec<String>, Refusal> {
    let Some(value) = value else { return Ok(Vec::new()) };
    let array =
        value.as_array().ok_or_else(|| invalid(format!("{path} must be a string array")))?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in array {
        let entry =
            entry.as_str().ok_or_else(|| invalid(format!("{path} must be a string array")))?;
        if seen.insert(entry.to_string()) {
            out.push(entry.to_string());
        }
    }
    Ok(out)
}

/// `^[a-z][a-z0-9-]{0,63}$`, checked without a regex engine.
fn is_kebab_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn identifier(value: Option<&Value>, fallback: &str, path: &str) -> Result<String, Refusal> {
    let normalized = match value {
        None | Some(Value::Null) => slugify(fallback),
        Some(other) => text(Some(other), path)?,
    };
    if !is_kebab_id(&normalized) {
        return Err(invalid(format!("{path} must be kebab-case")));
    }
    Ok(normalized)
}

/// An ISO-8601 instant, as epoch millis.
fn iso_millis_of(value: &str, path: &str) -> Result<i64, Refusal> {
    crate::isotime::parse_iso_millis(value)
        .ok_or_else(|| invalid(format!("{path} must be an ISO-8601 timestamp")))
}

/// Resolve a unit's `kind`/`transient` pair from its seed.
fn normalize_unit_fields(
    seed: &serde_json::Map<String, Value>,
    name: &str,
    created_at: &str,
) -> Result<(UnitKind, Option<ContractMetadata>), Refusal> {
    let kind = seed.get("kind").and_then(Value::as_str).unwrap_or("department");
    match kind {
        "department" => {
            if seed.get("transient").is_some_and(|v| !v.is_null()) {
                return Err(invalid(format!(
                    "department {name}.transient is valid only for a contract unit"
                )));
            }
            Ok((UnitKind::Department, None))
        }
        "contract" => {
            let raw = seed
                .get("transient")
                .ok_or_else(|| invalid(format!("contract {name}.transient must be an object")))?;
            let raw = object(raw, &format!("contract {name}.transient"))?;
            let engagement =
                text(raw.get("engagement"), &format!("contract {name}.transient.engagement"))?;
            let expires_at = match raw.get("expiresAt") {
                None | Some(Value::Null) => None,
                Some(value) => {
                    Some(text(Some(value), &format!("contract {name}.transient.expiresAt"))?)
                }
            };
            let launched =
                iso_millis_of(created_at, &format!("contract {name}.transient.launchedAt"))?;
            if let Some(expires) = expires_at.as_deref() {
                let expiry =
                    iso_millis_of(expires, &format!("contract {name}.transient.expiresAt"))?;
                if expiry <= launched {
                    return Err(invalid(format!(
                        "contract {name}.transient.expiresAt must be later than launchedAt"
                    )));
                }
            }
            Ok((
                UnitKind::Contract,
                Some(ContractMetadata {
                    engagement,
                    launched_at: created_at.to_string(),
                    expires_at,
                }),
            ))
        }
        other => Err(invalid(format!(
            "department {name}.kind must be department or contract, got '{other}'"
        ))),
    }
}

/// Everything a person seed needs that the seed itself does not carry.
struct PersonContext<'a> {
    fallback_id: &'a str,
    default_title: String,
    default_mandate: String,
    department_id: &'a str,
    kind: PersonKind,
    created_at: &'a str,
}

/// Turn one person seed into a durable record.
fn normalize_person_seed(
    raw_value: &Value,
    context: &PersonContext<'_>,
) -> Result<PersonRecord, Refusal> {
    let raw = object(raw_value, &format!("person {}", context.fallback_id))?;
    let name = text(raw.get("name"), &format!("person {}.name", context.fallback_id))?;
    let person_id = identifier(
        raw.get("id"),
        context.fallback_id,
        &format!("person {}.id", context.fallback_id),
    )?;
    // TOMBSTONE (chief-home-is-cwd §3/§4e): `skills`, `extensions` and
    // `packages` were read off the genesis seed here. A company's skills are the
    // files in `<dir>/.pi/skills`, which genesis seeds once and Pi loads through
    // one symlink; no person selects a subset of them, so a spec key naming one
    // describes nothing and is ignored like any other unmodelled key.
    let prompts = optional_strings(raw.get("prompts"), &format!("person {person_id}.prompts"))?;
    for (index, prompt) in prompts.iter().enumerate() {
        crate::store::organization::prompt_template_reference(
            prompt,
            &format!("person {person_id}.prompts[{index}]"),
        )?;
    }
    let requested_tools = optional_strings(raw.get("tools"), &format!("person {person_id}.tools"))?;
    // The same silent-acceptance hole the department seed validator had: a
    // mistyped name used to be stored verbatim and grant nothing.
    if let Some(unknown) = undeclarable_tool(&requested_tools) {
        return Err(invalid(format!(
            "person {person_id}.tools names '{}', which is not a declarable tool — {}",
            bounded(unknown),
            declarable_tools_sentence(context.kind)
        )));
    }
    // A worker starts BENCHED unless the spec says otherwise: a company boots
    // its leadership, not its whole roster.
    let employment_state = if context.kind == PersonKind::Worker
        && raw.get("startActive") != Some(&Value::Bool(true))
    {
        EmploymentState::Benched
    } else {
        EmploymentState::Active
    };
    let title = match raw.get("title") {
        None | Some(Value::Null) => context.default_title.clone(),
        Some(value) => text(Some(value), &format!("person {person_id}.title"))?,
    };
    let mandate = match raw.get("mandate") {
        None | Some(Value::Null) => context.default_mandate.clone(),
        Some(value) => text(Some(value), &format!("person {person_id}.mandate"))?,
    };
    // No default grant is written. The Pi builtin floor is COMPOSED for every
    // person by `person_tool_names` (operator decision, 2026-08-10), so
    // defaulting it into the record here would be a second mechanism for one
    // fact — and the genesis-only version of exactly that is what left 23
    // people in a live company unable to open a file. `tools` stores only what
    // the spec actually asked for.
    let tools = requested_tools;
    Ok(PersonRecord {
        id: person_id,
        name,
        title,
        mandate,
        kind: context.kind,
        department_id: context.department_id.to_string(),
        employment_state,
        activation: "resident".to_string(),
        tools,
        prompts,
        created_at: context.created_at.to_string(),
        staffing_history: None,
        extra: BTreeMap::new(),
    })
}

/// The accumulating genesis draft.
struct Draft {
    departments: BTreeMap<String, DepartmentRecord>,
    people: BTreeMap<String, PersonRecord>,
    department_order: Vec<String>,
    people_order: Vec<String>,
}

impl Draft {
    fn register_person(&mut self, person: PersonRecord) -> Result<(), Refusal> {
        if self.people.contains_key(&person.id) {
            return Err(invalid(format!("Duplicate person id '{}'", person.id)));
        }
        self.people_order.push(person.id.clone());
        self.people.insert(person.id.clone(), person);
        Ok(())
    }
}

/// Normalize one department subtree, depth-first, in spec order.
fn visit_department(
    draft: &mut Draft,
    value: &Value,
    parent_department_id: &str,
    path: &str,
    now: &str,
) -> Result<(), Refusal> {
    let department = object(value, path)?;
    let department_name = text(department.get("name"), &format!("{path}.name"))?;
    let local_id = identifier(department.get("id"), &department_name, &format!("{path}.id"))?;
    // A nested unit's id is `<parent>-<local>`, so ids stay globally unique and
    // readable without a lookup. The root's children keep the bare local id.
    let department_id = if parent_department_id == ROOT_DEPARTMENT_ID {
        local_id
    } else {
        format!("{parent_department_id}-{local_id}")
    };
    if !is_kebab_id(&department_id) {
        return Err(invalid(format!(
            "{path}.id produces an invalid hierarchical id '{department_id}'"
        )));
    }
    if draft.departments.contains_key(&department_id) {
        return Err(invalid(format!("Duplicate department id '{department_id}'")));
    }
    let purpose = text(department.get("purpose"), &format!("{path}.purpose"))?;
    let (kind, transient) = normalize_unit_fields(department, &department_name, now)?;

    let head_seed = department
        .get("head")
        .ok_or_else(|| invalid(format!("department {department_name}.head is required")))?;
    let head_fallback = format!("{department_id}-head");
    let head = normalize_person_seed(
        head_seed,
        &PersonContext {
            fallback_id: &head_fallback,
            default_title: format!("Head of {department_name}"),
            default_mandate: format!(
                "Delegate {department_name} work, supervise delivery, and report decision-ready results to the parent head."
            ),
            department_id: &department_id,
            kind: PersonKind::Head,
            created_at: now,
        },
    )?;
    let head_id = head.id.clone();
    draft.register_person(head)?;
    draft.department_order.push(department_id.clone());
    draft.departments.insert(
        department_id.clone(),
        DepartmentRecord {
            id: department_id.clone(),
            name: department_name.clone(),
            purpose,
            kind: Some(kind),
            transient,
            parent_department_id: Some(parent_department_id.to_string()),
            head_person_id: head_id,
            state: UnitState::Active,
            created_at: now.to_string(),
            extra: BTreeMap::new(),
        },
    );

    let empty = Vec::new();
    let staff = match department.get("staff") {
        None | Some(Value::Null) => &empty,
        Some(value) => {
            value.as_array().ok_or_else(|| invalid(format!("{path}.staff must be an array")))?
        }
    };
    for (index, entry) in staff.iter().enumerate() {
        let seed_path = format!("{path}.staff[{index}]");
        let seed = object(entry, &seed_path)?;
        let staff_name = text(seed.get("name"), &format!("{seed_path}.name"))?;
        let fallback = format!("{department_id}-{}", slugify(&staff_name));
        let worker = normalize_person_seed(
            entry,
            &PersonContext {
                fallback_id: &fallback,
                default_title: staff_name,
                default_mandate: format!(
                    "Own assigned {department_name} work and return a concise, verified result to the department head."
                ),
                department_id: &department_id,
                kind: PersonKind::Worker,
                created_at: now,
            },
        )?;
        draft.register_person(worker)?;
    }

    let children = match department.get("departments") {
        None | Some(Value::Null) => &empty,
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid(format!("{path}.departments must be an array")))?,
    };
    for (index, child) in children.iter().enumerate() {
        visit_department(
            draft,
            child,
            &department_id,
            &format!("{path}.departments[{index}]"),
            now,
        )?;
    }
    Ok(())
}

/// Turn a company spec into a validated manifest.
///
/// # Errors
/// [`SPEC_INVALID`] for any malformed field, and
/// [`MANIFEST_INVALID`] if the assembled manifest somehow fails the validator —
/// which is a bug in this function, not in the spec, and is deliberately not
/// silently repaired.
pub fn normalize_organization_spec(
    input: &Value,
    now: &str,
) -> Result<OrganizationManifest, Refusal> {
    let raw = object(input, "organization spec")?;
    let name = text(raw.get("name"), "organization spec.name")?;
    let purpose = text(raw.get("purpose"), "organization spec.purpose")?;
    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(invalid("organization spec.name must produce a usable slug"));
    }

    let mut draft = Draft {
        departments: BTreeMap::new(),
        people: BTreeMap::new(),
        department_order: Vec::new(),
        people_order: Vec::new(),
    };

    // THE MINT POINT for the root person's identity. The id falls out of this
    // fallback, and everything downstream is derivational: `@chief` in the
    // footer, `# Chief — <title>` in the contract, the mailbox key. Nothing
    // reads a `"chief"` literal — the fences and authority rules go through the
    // derived accessor over the root department's head — so this line and the
    // spec key below are the whole rename.
    let chief_seed =
        raw.get("chief").ok_or_else(|| invalid("organization spec.chief is required"))?;
    let chief = normalize_person_seed(
        chief_seed,
        &PersonContext {
            fallback_id: "chief",
            default_title: "Chief".to_string(),
            default_mandate: format!(
                "Set direction for {name}, delegate through department heads, and make final organization decisions."
            ),
            department_id: ROOT_DEPARTMENT_ID,
            kind: PersonKind::Executive,
            created_at: now,
        },
    )?;
    let chief_id = chief.id.clone();
    draft.register_person(chief)?;
    draft.department_order.push(ROOT_DEPARTMENT_ID.to_string());
    draft.departments.insert(
        ROOT_DEPARTMENT_ID.to_string(),
        DepartmentRecord {
            id: ROOT_DEPARTMENT_ID.to_string(),
            name: name.clone(),
            purpose: purpose.clone(),
            kind: Some(UnitKind::Company),
            transient: None,
            parent_department_id: None,
            head_person_id: chief_id,
            state: UnitState::Active,
            created_at: now.to_string(),
            extra: BTreeMap::new(),
        },
    );

    let empty = Vec::new();
    let roots = match raw.get("departments") {
        None | Some(Value::Null) => &empty,
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid("organization spec.departments must be an array"))?,
    };
    for (index, department) in roots.iter().enumerate() {
        visit_department(
            &mut draft,
            department,
            ROOT_DEPARTMENT_ID,
            &format!("organization spec.departments[{index}]"),
            now,
        )?;
    }

    let manifest = OrganizationManifest {
        schema_version: ORGANIZATION_SCHEMA_VERSION,
        kind: "organization".to_string(),
        slug: slug.clone(),
        name,
        purpose,
        root_department_id: ROOT_DEPARTMENT_ID.to_string(),
        policy: OrganizationPolicy {
            supervision_interval_ms: SUPERVISION_INTERVAL_MS,
            acknowledgement_timeout_ms: ACKNOWLEDGEMENT_TIMEOUT_MS,
            acknowledgement_retry_limit: 1,
            replacement_limit: 1,
        },
        department_order: draft.department_order,
        people_order: draft.people_order,
        departments: draft.departments,
        people: draft.people,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        extra: BTreeMap::new(),
    };
    validate_organization_manifest(&manifest).map_err(|refusal| {
        Refusal::new(
            MANIFEST_INVALID,
            format!("the spec produced an invalid manifest: {}", refusal.message),
        )
    })?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-08-07T00:00:00.000Z";

    fn minimal_spec() -> Value {
        json!({
            "name": "Northstar Conformance",
            "purpose": "Freeze durable store behaviour as language-neutral fixtures.",
            "chief": { "name": "Avery" }
        })
    }

    /// THE SHARED ADVERSARIAL CORPUS. Every company-slug producer in this
    /// repository is driven against these exact inputs, in its own crate,
    /// because the two producers may not link each other.
    ///
    /// This copy and `chief-cli`'s `genesis.rs` copy are held
    /// character-identical by `scripts/test/slug-producers-agree.test.mjs`.
    /// Editing one alone fails that guard by name: two producers tested against
    /// two different corpora are two producers nobody compared.
    ///
    /// The underscores are the point. `_` is
    /// `chief_cli::placement::SESSION_TERMINATOR`, the one character the
    /// client's whole session-name prefix-collision proof turns on, and the
    /// corpus that stood here before this test did not contain a single one.
    const SLUG_PRODUCER_CORPUS: &[&str] = &[
        "Acme",
        "Acme_Corp",
        "acme_corp",
        "_leading",
        "trailing_",
        "__",
        "org-acme_",
        "  --Acme Capital--  ",
        "Leo Capital Inc.",
        "A B C D E F G H I J K L M N O P Q R S T U V W X Y Z",
        "Northstar Operations Engineering Department E2E Team",
        "Ünïcødé Ç☃mpany",
        "tab\there",
        "new\nline",
        "null\0byte",
        "dots.and.dots",
        "slash/es and back\\slashes",
        "%_$#@!",
        "...",
        "   ",
        "-- --",
    ];

    /// `chief-cli`'s `paths::is_canonical_slug`, character for character.
    ///
    /// A SECOND COPY OF ONE RULE, and deliberately so. The validator is
    /// `pub(crate)` inside the operator client's BINARY, and that crate and
    /// this one are forbidden to link in either direction by the client
    /// boundary guard under `scripts/test/` — so the only ways to
    /// assert this producer against the real validator are to break that
    /// boundary or to copy the rule. The copy is held byte-identical to the
    /// original by `scripts/test/slug-producers-agree.test.mjs`, which fails
    /// naming both files the moment they differ.
    fn is_canonical_slug(slug: &str) -> bool {
        if slug.is_empty() {
            return false;
        }
        let mut previous_was_hyphen = true; // a leading hyphen is illegal
        for character in slug.chars() {
            match character {
                'a'..='z' | '0'..='9' => previous_was_hyphen = false,
                '-' if !previous_was_hyphen => previous_was_hyphen = true,
                _ => return false,
            }
        }
        !previous_was_hyphen // a trailing hyphen is illegal
    }

    /// THE PROPERTY THE CLIENT'S COLLISION PROOF ACTUALLY NEEDS, asserted here
    /// for this producer and in `chief-cli`'s `genesis.rs` for the other one.
    ///
    /// `chief-cli`'s `placement::session_name_for_slug` proves that no two
    /// company session names can prefix-collide, and its first fact is "a slug
    /// can never contain the terminator `_`". That fact used to be argued by
    /// naming ONE producer — `genesis::slugify` — which was never the whole
    /// set: THIS function mints `manifest.slug` and every department and person
    /// id derived from it, and nothing anywhere asserted the two agreed. The
    /// argument does not need a producer count. It needs this, from every
    /// producer: whatever it emits satisfies the validator, which refuses `_`.
    ///
    /// Empty is an accepted answer and is not a slug — `identifier` refuses it
    /// through `is_kebab_id`, and `chief-cli`'s `launch` refuses it before a
    /// beacond row is claimed — so the property is "empty, or canonical".
    #[test]
    fn no_input_makes_this_producer_emit_a_non_canonical_slug() {
        for input in SLUG_PRODUCER_CORPUS {
            let slug = slugify(input);
            if slug.is_empty() {
                continue;
            }
            assert!(
                is_canonical_slug(&slug),
                "slugify({input:?}) emitted {slug:?}, which chief-cli's is_canonical_slug \
                 refuses — the validator that guards every company path join and the client's \
                 session name built from it"
            );
            assert!(
                !slug.contains('_'),
                "slugify({input:?}) emitted {slug:?}, which carries the client's session-name \
                 terminator '_' — two companies could then mint prefix-colliding sessions"
            );
        }
    }

    /// The negative control for the copied rule. Without it,
    /// `no_input_makes_this_producer_emit_a_non_canonical_slug` would pass
    /// identically against a validator that always answered `true`.
    #[test]
    fn the_copied_validator_refuses_what_the_original_refuses() {
        for good in ["acme", "acme-corp", "a", "a-b-c", "acme2"] {
            assert!(is_canonical_slug(good), "{good} must be accepted");
        }
        for bad in ["", "-acme", "acme-", "acme--corp", "Acme", "acme_corp", "acme corp", "acmé"] {
            assert!(!is_canonical_slug(bad), "{bad} must be refused");
        }
    }

    // NOTE ON THIS TEST'S NAME: there is no TypeScript twin left in the tree.
    // A shape sweep for a lowercase-and-collapse-to-dash producer over every
    // `.ts`/`.tsx`/`.mjs`/`.cjs`/`.js` file finds exactly one, and it derives a
    // Playwright profile name from an already-canonical company slug — it is
    // not a company-slug producer. Whether the rule below still MATCHES some
    // TypeScript is not re-derivable here, and this comment does not guess; the
    // assertions themselves are unchanged and still pin this producer's output.
    #[test]
    fn slugify_matches_the_typescript_rule() {
        assert_eq!(slugify("Northstar Conformance"), "northstar-conformance");
        assert_eq!(slugify("  Spaced   Out  "), "spaced-out");
        assert_eq!(slugify("Acme, Inc."), "acme-inc");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("ALLCAPS"), "allcaps");
    }

    #[test]
    fn a_long_name_is_bounded_and_still_trimmed() {
        let slug = slugify(&"a b ".repeat(40));
        assert!(slug.len() <= MAX_SLUG_CHARS);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn a_minimal_spec_produces_a_valid_one_person_company() {
        let manifest = normalize_organization_spec(&minimal_spec(), NOW).expect("manifest");
        assert_eq!(manifest.slug, "northstar-conformance");
        assert_eq!(manifest.department_order, vec!["executive".to_string()]);
        assert_eq!(manifest.people_order, vec!["chief".to_string()]);
        assert_eq!(manifest.policy.supervision_interval_ms, SUPERVISION_INTERVAL_MS);
        validate_organization_manifest(&manifest).expect("valid");
    }

    #[test]
    fn the_ceo_carries_no_invented_tool_grant() {
        // Was `the_ceo_gets_the_full_builtin_toolset_including_bash`. The CEO
        // still HOLDS the whole builtin set — but it is composed at launch by
        // `person_tool_names`, not written here, so the record carries only
        // what the spec asked for. The grant itself is pinned in chiefd-host's
        // `every_person_gets_the_whole_builtin_floor_whatever_their_seed_declared`.
        let spec = json!({ "name": "Acme", "purpose": "Do things.", "chief": { "name": "Avery" } });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        let ceo_id = manifest.chief_person_id().expect("a ceo").to_string();
        assert!(manifest.people[&ceo_id].tools.is_empty());
    }

    /// The spec the OPERATOR CLIENT actually sends, normalized here.
    ///
    /// Moved from `chief-cli`'s `genesis.rs` by P6 of
    /// the design record: that crate must not link this
    /// one, so the two assertions it carried — "the Founder spec becomes
    /// exactly one CEO and no eager team, with the root department headed by
    /// that CEO" and "that CEO holds the full builtin tool grant" — could not
    /// stay there. Deleting them was not an option
    /// either, so they run here, against the shared fixture
    /// `apps/chiefd/fixtures/founder-genesis-spec.json` that `chief-cli`'s
    /// `the_founder_spec_is_byte_for_byte_the_shared_genesis_fixture` pins its
    /// builder to. One file, both ends.
    ///
    /// This is NOT the same assertion as
    /// [`the_ceo_gets_the_full_builtin_toolset_including_bash`]: that one runs
    /// on a spec this test file invents, and this one runs on the bytes a real
    /// `chief` puts on the wire.
    #[test]
    fn the_founder_spec_the_operator_client_sends_normalizes_to_one_ceo() {
        let spec: Value =
            serde_json::from_str(include_str!("../../../../fixtures/founder-genesis-spec.json"))
                .expect("the shared genesis fixture must be JSON");
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        assert_eq!(manifest.slug, "acme");
        assert_eq!(manifest.root_department_id, ROOT_DEPARTMENT_ID);
        assert_eq!(manifest.departments.len(), 1, "no eager team");
        assert_eq!(manifest.people.len(), 1, "exactly one CEO");
        let ceo_id = &manifest.people_order[0];
        let ceo = manifest.people.get(ceo_id).expect("the CEO");
        assert_eq!(
            &manifest.departments.get(ROOT_DEPARTMENT_ID).expect("root").head_person_id,
            ceo_id
        );
        // The CEO's builtin grant is COMPOSED at launch by `person_tool_names`,
        // not written into the record, so the normalized manifest carries none.
        // What this fixture pins is the structure the real `chief` sends.
        assert!(ceo.tools.is_empty(), "{:?}", ceo.tools);
        validate_organization_manifest(&manifest).expect("valid");
    }

    /// Was `a_department_head_is_refused_bash`, asserting the opposite. Invariant
    /// 34 was removed by operator decision on 2026-08-10 ("Everybody should have
    /// bash"), so the contract to pin is that a head's explicitly requested
    /// `bash` is honoured at genesis.
    #[test]
    fn a_department_head_may_request_bash() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn", "tools": ["read", "bash"] }
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("a head may hold bash");
        let head = manifest.people.values().find(|p| p.name == "Quinn").expect("Quinn");
        assert_eq!(head.tools, vec!["read".to_string(), "bash".to_string()]);
    }

    /// Was `a_head_that_declares_no_tools_still_gets_bash_by_default`, written
    /// when the fix was a per-kind DEFAULT. The operator replaced that with an
    /// unconditional floor, so there is no default left to assert here: a head
    /// who names no tools stores none, and receives the whole builtin set at
    /// launch. The composed half is pinned in chiefd-host's
    /// `every_person_gets_the_whole_builtin_floor_whatever_their_seed_declared`;
    /// this half proves genesis is no longer a second place the grant is
    /// decided.
    #[test]
    fn a_head_that_declares_no_tools_stores_none() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" }
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        let head = manifest.people.values().find(|p| p.name == "Quinn").expect("Quinn");
        assert!(head.tools.is_empty(), "{:?}", head.tools);
    }

    /// Genesis had the same silent hole as the department seed validator: any
    /// string at all was accepted into `tools` and granted nothing.
    #[test]
    fn a_mistyped_tool_name_at_genesis_is_refused_with_the_declarable_set() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" },
                "staff": [{ "name": "Robin", "taskClass": "coding", "tools": ["read", "bahs"] }]
            }]
        });
        let err = normalize_organization_spec(&spec, NOW)
            .expect_err("a mistyped tool must be refused, not stored");
        assert_eq!(err.code, SPEC_INVALID);
        assert!(err.message.contains("'bahs'"), "{}", err.message);
        assert!(err.message.contains("not a declarable tool"), "{}", err.message);
        assert!(
            err.message.contains("read, bash, edit, write, grep, find, ls"),
            "the refusal must name the valid ids: {}",
            err.message
        );
    }

    // TOMBSTONE (chief-home-is-cwd §4e): two genesis resource tests.
    //   `a_genesis_seed_that_selects_an_extension_may_declare_its_tool` pinned
    //   the `selects_resources` escape hatch, which is deleted with the
    //   selection that fed it; the mistyped-tool test above now describes the
    //   whole rule. `a_genesis_person_may_select_resources_without_any_rationale`
    //   (#1093) pinned that selecting a skill, an extension and a package needed
    //   no justification — there is no selection left to justify, because the
    //   company's skills are the files in `<dir>/.pi/skills`.

    #[test]
    fn bounded_truncates_on_a_char_boundary() {
        let long = "é".repeat(200);
        let kept = bounded(&long);
        assert!(kept.len() <= MAX_REFUSAL_ECHO_BYTES);
        assert!(long.starts_with(kept));
        assert_eq!(bounded("read"), "read");
    }

    #[test]
    fn nested_department_ids_are_parent_prefixed_but_top_level_ones_are_not() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" },
                "departments": [{
                    "name": "Platform",
                    "purpose": "Foundations.",
                    "head": { "name": "Pat" }
                }]
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        assert_eq!(
            manifest.department_order,
            vec![
                "executive".to_string(),
                "engineering".to_string(),
                "engineering-platform".to_string()
            ]
        );
    }

    #[test]
    fn people_order_is_the_ceo_then_each_head_then_its_staff() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" },
                "staff": [{ "name": "Sam" }, { "name": "Robin" }]
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        assert_eq!(
            manifest.people_order,
            vec![
                "chief".to_string(),
                "engineering-head".to_string(),
                "engineering-sam".to_string(),
                "engineering-robin".to_string()
            ]
        );
    }

    #[test]
    fn a_worker_starts_benched_unless_the_spec_says_otherwise() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" },
                "staff": [{ "name": "Sam" }, { "name": "Robin", "startActive": true }]
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        assert_eq!(
            manifest.people.get("engineering-sam").map(|p| p.employment_state),
            Some(EmploymentState::Benched)
        );
        assert_eq!(
            manifest.people.get("engineering-robin").map(|p| p.employment_state),
            Some(EmploymentState::Active)
        );
    }

    #[test]
    fn a_contract_unit_requires_engagement_metadata_and_a_later_expiry() {
        let missing = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Audit",
                "purpose": "Review.",
                "kind": "contract",
                "head": { "name": "Quinn" }
            }]
        });
        assert!(normalize_organization_spec(&missing, NOW).is_err());

        let backwards = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Audit",
                "purpose": "Review.",
                "kind": "contract",
                "transient": { "engagement": "Q3 review", "expiresAt": "2026-08-06T00:00:00.000Z" },
                "head": { "name": "Quinn" }
            }]
        });
        let err = normalize_organization_spec(&backwards, NOW).expect_err("refusal");
        assert!(err.message.contains("must be later than launchedAt"));

        let good = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Audit",
                "purpose": "Review.",
                "kind": "contract",
                "transient": { "engagement": "Q3 review", "expiresAt": "2026-09-01T00:00:00.000Z" },
                "head": { "name": "Quinn" }
            }]
        });
        let manifest = normalize_organization_spec(&good, NOW).expect("manifest");
        let unit = manifest.departments.get("audit").expect("unit");
        assert_eq!(unit.kind, Some(UnitKind::Contract));
        assert_eq!(unit.transient.as_ref().map(|t| t.engagement.as_str()), Some("Q3 review"));
        assert_eq!(unit.transient.as_ref().map(|t| t.launched_at.as_str()), Some(NOW));
    }

    #[test]
    fn a_non_contract_unit_may_not_carry_transient_metadata() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "transient": { "engagement": "nope" },
                "head": { "name": "Quinn" }
            }]
        });
        let err = normalize_organization_spec(&spec, NOW).expect_err("refusal");
        assert!(err.message.contains("valid only for a contract unit"));
    }

    #[test]
    fn two_people_cannot_share_an_id() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn", "id": "chief" }
            }]
        });
        let err = normalize_organization_spec(&spec, NOW).expect_err("refusal");
        assert!(err.message.contains("Duplicate person id 'chief'"));
    }

    #[test]
    fn a_blank_name_produces_no_company() {
        let spec = json!({ "name": "   ", "purpose": "Do things.", "chief": { "name": "Avery" } });
        assert!(normalize_organization_spec(&spec, NOW).is_err());
    }

    /// Was `the_worker_tool_grant_follows_the_task_class`, which pinned
    /// `default_tools` — a per-task-class grant written into the record at
    /// genesis. That table is deleted: the Pi builtin floor is composed for
    /// every person on every path instead (operator decision, 2026-08-10), so
    /// genesis stores only what the spec asked for.
    ///
    /// The floor itself is pinned where it now lives, in chiefd-host's
    /// `every_person_gets_the_whole_builtin_floor_whatever_their_seed_declared`.
    /// This crate cannot call that derivation, so what it asserts is the other
    /// half of the same fact: nothing is invented into the record here.
    #[test]
    fn genesis_stores_only_the_tools_the_spec_asked_for() {
        let spec = json!({
            "name": "Acme",
            "purpose": "Do things.",
            "chief": { "name": "Avery" },
            "departments": [{
                "name": "Engineering",
                "purpose": "Ship.",
                "head": { "name": "Quinn" },
                "staff": [{ "name": "Robin", "taskClass": "coding" }]
            }]
        });
        let manifest = normalize_organization_spec(&spec, NOW).expect("manifest");
        for person in manifest.people.values() {
            assert!(
                person.tools.is_empty(),
                "{} must carry no invented grant, got {:?}",
                person.id,
                person.tools
            );
        }
    }
}
