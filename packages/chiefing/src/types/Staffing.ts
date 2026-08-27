// Types for StaffingClient. Staffing verbs surface refusals AS VALUES
// (never thrown) — Refusal is the common shape a 422 body decodes to.
//
// Source: src/organization/org-durable-store.ts:176-356, re-verified against
// apps/chiefd/crates/chiefd-core/src/store/org_ops.rs (the per-verb
// `*Outcome`/`*Refusal` enums the docstore router maps to `{applied}` /
// `{refused, detail}` JSON).
//
// No whole-company-removal wire family here (ruling D24/F25): E7-S4/E7-S7
// delete the two-phase whole-company removal protocol, so its wire types are
// never authored in this package.

export interface Refusal {
  refused: string
  detail: string
}

/** Rust authority: chiefd-core/src/store/org_ops.rs's hire/create-department
 * seed construction (`OwnedNewPersonSeed`/`NewPersonSeed`). Complete
 * normalized person payload accepted by caller-revisionless staffing
 * transactions. Placement/ordinal are chosen inside chiefd's transaction;
 * every other durable PersonRecord field is explicit so reconstructing the
 * manifest never invents or drops data. */
export interface AtomicPersonSeed {
  name: string
  title: string
  mandate: string
  kind: 'worker' | 'head' | 'executive'
  employmentState: 'active' | 'benched'
  // Every field below is `#[serde(default)]` on chiefd's hire request, and
  // `activation` even has a named default (`resident`) that chiefd applies
  // itself. Declaring them REQUIRED here forced each caller to invent values
  // chiefd already decides — and a caller that invents a default is a second
  // opinion about it, which drifts the moment chiefd changes its own.
  activation?: 'resident' | 'on-demand'
  tools?: string[]
  prompts?: string[]
}

/** Rust authority: org_ops.rs `DepartmentStaffSeed`/`HeadDecision` seed
 * construction — the per-person seed carried inside an atomic
 * create-department-with-staff call. */
export interface AtomicDepartmentNewPersonSeed {
  personId: string
  name: string
  title: string
  mandate: string
  personKind: 'head' | 'worker'
  employmentState: 'active' | 'benched'
  activation: 'resident' | 'on-demand'
  tools: string[]
  prompts: string[]
}

/** Rust authority: org_ops.rs `HeadDecision` (`AppointExisting`/`HireNew`
 * variants). */
export type AtomicDepartmentHead =
  | { kind: 'appoint-existing'; personId: string }
  | ({ kind: 'hire-new' } & AtomicDepartmentNewPersonSeed & { personKind: 'head' })

/** Complete normalized initial worker carried by an atomic department
 * create. Rust authority: org_ops.rs `DepartmentStaffSeed`. */
export type AtomicDepartmentStaff = AtomicDepartmentNewPersonSeed & {
  kind: 'hire-new'
  personKind: 'worker'
}

/** Typed unit metadata committed with an atomic department create. Rust
 * authority: org_ops.rs `DepartmentCreateUnit`. */
export type AtomicDepartmentUnit =
  | { kind: 'department' }
  | {
      kind: 'contract'
      transient: {
        engagement: string
        launchedAt: string
        expiresAt?: string
      }
    }

/** Attested authorizer for a caller-revisionless staffing transaction. A pane
 * may only send its launcher-bound person id; an env-stripped direct command
 * is attributed explicitly to the operator instead of impersonating the CEO. */
export type AtomicStaffingRequester = { kind: 'person'; personId: string } | { kind: 'operator' }

/** Rust authority: org_ops.rs `RemoveDepartmentOutcome`. Immutable identities
 * committed by one named recursive unit removal. The removal deletes units and
 * OFFBOARDS their people — `departedPersonIds` name rows that are retained
 * (`employmentState: 'departed'`), never deleted. */
export type AtomicRemoveDepartmentOutcome =
  { applied: true; removedDepartmentIds: string[]; departedPersonIds: string[] } | Refusal

/** Rust authority: org_ops.rs `HireOutcome`/`HireRefusal`. */
export type AtomicHireOutcome = { applied: true } | Refusal

/** Rust authority: org_ops.rs `CreateDepartmentOutcome`/`CreateDepartmentRefusal`
 * — verified field-for-field: `{applied: true, departmentId}` on
 * `Applied{department_id}`, `{refused, detail}` on `Refused{reason}` (`reason.code()`/
 * `reason.detail()`). */
export type AtomicCreateDepartmentOutcome = { applied: true; departmentId: string } | Refusal

/** Rust authority: org_ops.rs `ReparentOutcome`/`ReparentRefusal`. Result of
 * one caller-revisionless whole-department reparent. */
export type AtomicReparentDepartmentOutcome = { applied: true; departmentId: string } | Refusal

/** Rust authority: org_ops.rs `TransferOutcome`/`TransferRefusal`. Result of
 * one caller-revisionless normalized person transfer. */
export type AtomicTransferPersonOutcome = { applied: true; moved: string[] } | Refusal
