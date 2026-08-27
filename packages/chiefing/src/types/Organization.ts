// Pure wire types for the organization manifest — the sole structural
// authority of a company. No functions, no logic: every mutation and
// validation rule this file used to carry (`apps/cli/src/legacy/organization/
// org-types.ts`, deleted) now lives in Rust.
//
// Rust authority: apps/chiefd/crates/chiefd-core/src/store/organization.rs.
// Every record type below mirrors its Rust struct field-for-field; the Rust
// side serializes camelCase via `#[serde(rename_all = "camelCase")]`, and an
// `Option` field there is OMITTED from the wire when absent, never sent as
// `null` — modeled here as an optional (`?`) property.
//
// The five seed types at the bottom (`ContractUnitSeed`, `PersonSeed`,
// `HirePersonSeed`, `DepartmentSeed`, `OrganizationSpec`) are the
// request-payload shapes a caller builds to ask chiefd to create people and
// departments. Their field lists are copied from the deleted
// `apps/cli/src/legacy/organization/org-types.ts` (git history,
// commit 4ecc06359^) — but none of that file's normalization/validation
// functions are ported here; every one of them now runs in Rust.

/** Whether a person may be activated at all. Rust: `EmploymentState`. */
export type EmploymentState = 'active' | 'benched' | 'departed'

/** A person's structural role. Only `'worker'` may hold `bash` (invariant 34;
 * the executive is a named exception). Rust: `PersonKind`. */
export type PersonKind = 'executive' | 'head' | 'worker'

/** Which flavour of unit a department record describes. Rust: `UnitKind`. */
export type OrganizationUnitKind = 'company' | 'department' | 'contract'

/** Whether a unit is accepting work. Rust: `UnitState`. */
export type UnitState = 'active' | 'paused'

/** A contract unit's engagement metadata, as stored on a `DepartmentRecord`.
 * Rust authority: `organization.rs` `ContractMetadata`. */
export interface ContractUnitMetadata {
  engagement: string
  launchedAt: string
  expiresAt?: string
}

/** One department or contract unit. Rust: `DepartmentRecord`. */
export interface DepartmentRecord {
  id: string
  name: string
  purpose: string
  /** Optional only so schema-v1 manifests written before unit kinds stay
   * readable; an absent value resolves to `'company'` on the root unit and
   * `'department'` everywhere else. */
  kind?: OrganizationUnitKind
  /** Present iff `kind === 'contract'`. */
  transient?: ContractUnitMetadata
  /** Absent only on the root. */
  parentDepartmentId?: string
  headPersonId: string
  state: UnitState
  createdAt: string
}

/** One person. Rust: `PersonRecord`. */
export interface PersonRecord {
  id: string
  name: string
  title: string
  mandate: string
  kind: PersonKind
  /** Where they belong, which is also where they work. One field since the
   * loan concept was deleted (2026-08-13): a loan was the only thing that
   * could separate membership from placement, so the `home`/`assigned` pair
   * that stood here carried one fact twice. */
  departmentId: string
  employmentState: EmploymentState
  /** Whether a pane is kept resident or spawned on demand. A chiefd-only
   * column absent from the pre-Rust manifest model; omitted from the wire
   * when it holds the default `'resident'`, so an absent value means
   * `'resident'`. */
  activation?: string
  tools: string[]
  prompts: string[]
  createdAt: string
  /** Append-only staffing audit. Opaque on the wire (Rust stores each entry
   * as `serde_json::Value`); never constructed or interpreted here. */
  staffingHistory?: unknown[]
}

/** Launcher-owned supervision policy constants carried in the manifest.
 * Rust: `OrganizationPolicy`. */
export interface OrganizationPolicy {
  supervisionIntervalMs: number
  acknowledgementTimeoutMs: number
  acknowledgementRetryLimit: number
  replacementLimit: number
}

/** The manifest: the sole structural authority for one company.
 * Rust: `OrganizationManifest`. */
export interface OrganizationManifest {
  schemaVersion: 1
  kind: 'organization'
  slug: string
  name: string
  purpose: string
  /** Always {@link ROOT_DEPARTMENT_ID}. */
  rootDepartmentId: string
  /* AC6: `runtimeSession` is DELETED. It carried `org-<slug>` — a tmux
   * session name, on the widest read of chiefd's HTTP surface — and it was
   * pure derivation from `slug`, which every reader already has. A client that
   * needs the name derives it; chiefd, which cannot see a display, no longer
   * asserts it. */
  policy: OrganizationPolicy
  /** Canonical department ordering; a bijection with `departments`. */
  departmentOrder: string[]
  /** Canonical person ordering; a bijection with `people`. */
  peopleOrder: string[]
  departments: Record<string, DepartmentRecord>
  people: Record<string, PersonRecord>
  createdAt: string
  updatedAt: string
}

/** Schema version of the manifest body. Rust:
 * `ORGANIZATION_SCHEMA_VERSION`. */
export const ORGANIZATION_SCHEMA_VERSION = 1 as const

/** The implicit root unit's id. Never derived, never configurable. Rust:
 * `ROOT_DEPARTMENT_ID`. */
export const ROOT_DEPARTMENT_ID = 'executive' as const

// ---- request-payload seed types (caller-built, chiefd-normalized) ---------

/** A caller's request to make a department a transient contract unit. Field
 * list copied from the deleted `org-types.ts`'s `ContractUnitSeed`. */
export interface ContractUnitSeed {
  engagement: string
  expiresAt?: string
}

/** A caller's request to create one person. Field list copied from the
 * deleted `org-types.ts`'s `PersonSeed`. */
export interface PersonSeed {
  id?: string
  name: string
  title?: string
  mandate?: string
  tools?: string[]
  /** Explicit project-local Pi prompt templates under `prompts/`. */
  prompts?: string[]
  startActive?: boolean
}

/** Raw ordinary-hire shape. Rust authors model provenance; callers supply
 * only intent. Field list copied from the deleted `org-types.ts`'s
 * `HirePersonSeed`.
 *
 * Once an `Omit<PersonSeed, …> & { … }` pair that removed a provenance field
 * and re-added it as "rejected by the live hire boundary". #1139 collapsed the
 * `modelReason` half and this packet collapsed the `modelApproval` half, so a
 * genesis seed and a hire seed now differ in nothing and this is a plain
 * alias. */
export type HirePersonSeed = PersonSeed

/** A caller's request to create one department, recursively. Field list
 * copied from the deleted `org-types.ts`'s `DepartmentSeed`. */
export interface DepartmentSeed {
  id?: string
  name: string
  purpose: string
  /** `'company'` is reserved for the implicit root unit. */
  kind?: 'department' | 'contract'
  /** Required only for transient contract units. */
  transient?: ContractUnitSeed
  head: HirePersonSeed
  staff?: HirePersonSeed[]
  departments?: DepartmentSeed[]
}

/** A caller's request to create a whole company. Field list copied from the
 * deleted `org-types.ts`'s `OrganizationSpec`. */
export interface OrganizationSpec {
  name: string
  purpose: string
  /** The root person's seed, under the key chiefd's own normalizer reads.
   * `normalize_organization_spec` refuses with `organization spec.chief is
   * required` for anything else, and the root person's id falls out of this
   * one fallback — which is why the field name is not cosmetic. Was `ceo`
   * until the root person became `chief`. */
  chief: PersonSeed
  departments?: DepartmentSeed[]
}
