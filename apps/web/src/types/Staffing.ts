/** Public types for the web's staffing verbs.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** What a caller may say when hiring.
 *
 * INTENT only, exactly like [`NewDepartmentRequest`]. There is no route in it:
 * every agent runs on the operator's own Pi defaults.
 *
 * There is no `personId`: chiefd mints it with `organization_spec`'s own rules
 * (`mint_hire_ids`, `<department>-<slugified name>`), and a second opinion here
 * would be a second implementation of a rule chiefd already owns. There is no
 * `requesterPersonId` either — the hiring manager is the TARGET department's
 * own head, an org fact read from the served tree rather than something a
 * browser gets to name. */
export interface HireRequest {
  departmentId?: string
  name?: string
  title?: string
  mandate?: string
  kind?: 'worker' | 'head' | 'executive'
}

/** What a caller may say when creating a department.
 *
 * INTENT only. There is no `departmentId` and no head `personId`: chiefd mints
 * both with `organization_spec`'s rules, and a second opinion here is exactly
 * what `mint_department_create_ids` exists to remove. There is no
 * `requesterPersonId` either — the hiring manager is the parent department's
 * own head, which is an org fact read from the served tree rather than
 * something a browser gets to name. */
export interface NewDepartmentRequest {
  parentId?: string
  name?: string
  purpose?: string
  /** The head, hired with the unit. `title` is absent because chiefd derives
   * it (`Head of <name>`). */
  head?: { name?: string; mandate?: string }
}

/** A hire that HAPPENED.
 *
 * chiefd's outcome union minus its refusal arm: a refused verb is thrown as a
 * `StaffingRequestError` and never reaches a caller, so the type a route
 * returns is the applied case alone. Leaving the union in the return type is
 * what let a refusal be serialized as a 200. */
export interface AppliedHire {
  readonly applied: true
}

/** A department that was CREATED, for the same reason. */
export interface AppliedDepartment {
  readonly applied: true
  readonly departmentId: string
}
