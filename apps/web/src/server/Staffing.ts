/**
 * Hiring and department creation, from the web.
 *
 * # A hire names no route
 *
 * Everybody runs on the operator's own Pi defaults, so a hire carries no
 * provider, no model and no observation to attest one with. A form that
 * offered any of them would be offering a choice the product no longer has.
 *
 * # The ids and the hiring manager are chiefd's, on BOTH verbs
 *
 * `hire` and `createDepartment` are siblings and read as siblings: neither
 * mints an id and neither lets a browser name the manager. A reader who has
 * learned one has learned the other.
 *
 * This module used to argue the opposite for `hire` — that a person id was the
 * caller's to supply "until chiefd derives the id on the caller's behalf".
 * chiefd does derive it, and did already: `mint_hire_ids` fills a blank
 * `personId` with `<department>-<slugify(name)>` before it validates anything
 * else, the same way `mint_department_create_ids` fills a department's. The
 * argument outlived the condition it rested on, and the cost was total — the
 * Hire form collects a name, a title and a mandate, so every hire from the
 * browser was refused `missing-field: "personId" is required` by this server
 * before chiefd was ever asked.
 */
import type { CompanyTreeDepartment } from '@chief/chiefing'

import { companyChiefd } from '@/server/CompanyChiefd'
import { RouteRefusalError } from '@/server/RouteRefusal'
import type {
  AppliedDepartment,
  AppliedHire,
  HireRequest,
  NewDepartmentRequest
} from '@/types/Staffing'
import { isNullish } from '@/utils/Nullish'

/** A request this server will not forward. */
export class StaffingRequestError extends RouteRefusalError {
  constructor(code: string, message: string) {
    // Always 422: chiefd is fine and the request is not, which is what this
    // class exists to say.
    super({ status: 422, code, message })
  }
}

/**
 * chiefd's refusal as a REFUSAL, not a 200 with a sad body.
 *
 * Every atomic staffing verb answers `{applied: true, …}` or
 * `{refused, detail}`, and `routeResult` serialized both as success. So a hire
 * chiefd declined — `operator-hirer-invalid`, `exec-root-protected` — reached
 * the browser as `HTTP 200`, the client parsed it against a schema that does
 * not describe it, and the operator was told nothing at all. A verb that did
 * not happen must not look like one that did.
 *
 * The status is 422 and the code is chiefd's own, carried verbatim: chiefd
 * understood the request and declined it, and its code is the only text that
 * tells an operator what to do differently.
 */
function applied<T extends { applied: true }>(outcome: T | { refused: string; detail: string }): T {
  if ('applied' in outcome) return outcome
  throw new StaffingRequestError(outcome.refused, outcome.detail)
}

function required(value: string | undefined, field: string): string {
  if (isNullish(value) || value.trim() === '') {
    throw new StaffingRequestError('missing-field', `"${field}" is required`)
  }
  return value
}

/** The department `departmentId` names, anywhere in the forest. */
function findDepartment(
  nodes: readonly CompanyTreeDepartment[],
  departmentId: string
): CompanyTreeDepartment | undefined {
  for (const node of nodes) {
    if (node.id === departmentId) return node
    const found = findDepartment(node.children, departmentId)
    if (found) return found
  }
  return undefined
}

/**
 * The person who does the hiring in `departmentId`: that unit's own head.
 *
 * Shared by both verbs because chiefd asks the same question of both — hiring
 * is manager-driven, and a bare `{kind: 'operator'}` requester is refused
 * `operator-hirer-invalid`. Which manager is not a choice the browser makes;
 * it is an org fact, read from the served tree.
 *
 * A headless unit is refused HERE rather than forwarded: chiefd's own answer
 * would be about a missing hiring manager, which does not tell an operator
 * that the unit they picked has nobody in it to do the hiring.
 */
function hiringManager(
  nodes: readonly CompanyTreeDepartment[],
  departmentId: string,
  refusal: { unknown: string; headless: string; cannot: string }
): string {
  const department = findDepartment(nodes, departmentId)
  if (isNullish(department)) {
    throw new StaffingRequestError(
      refusal.unknown,
      `no department "${departmentId}" in this company`
    )
  }
  if (department.headPersonId.trim() === '') {
    throw new StaffingRequestError(
      refusal.headless,
      `"${department.name}" has no head, so nobody there can ${refusal.cannot}`
    )
  }
  return department.headPersonId
}

/**
 * Hire somebody into an existing department.
 *
 * # The id is blank and the manager is the department's own head
 *
 * Both for the reasons `createDepartment` gives below.
 *
 * The requester is the department's own head, and it is the ONLY authority
 * this hire carries. `hiringManagerPersonId` used to ride beside it so chiefd
 * could refuse `hiring-manager-mismatch`; that refusal existed solely to stop
 * a hire inheriting a model route from a manager the caller had not attested,
 * and with no route to inherit it has no subject. The route refuses the field
 * as unknown rather than accepting one it would ignore.
 */
export async function hire(companyKey: string, request: HireRequest): Promise<AppliedHire> {
  const departmentId = required(request.departmentId, 'departmentId')
  const name = required(request.name, 'name')
  const title = required(request.title, 'title')
  const mandate = required(request.mandate, 'mandate')

  const chiefd = await companyChiefd(companyKey)
  const tree = await chiefd.orgSlice.treeStructured(companyKey)
  const hiringManagerPersonId = hiringManager(tree.departments, departmentId, {
    unknown: 'unknown-department',
    headless: 'department-has-no-head',
    cannot: 'do the hiring'
  })

  return applied(
    await chiefd.staffing.hirePerson(
      companyKey,
      // Blank, not derived: `mint_hire_ids` fills it with
      // `<department>-<slugify(name)>`, the same rule genesis uses.
      '',
      departmentId,
      {
        name,
        title,
        mandate,
        kind: request.kind ?? 'worker',
        employmentState: 'active'
      },
      { kind: 'person', personId: hiringManagerPersonId }
    )
  )
}

/**
 * Create a department, hiring its head with it.
 *
 * # Why the head is a NEW person
 *
 * This used to send chiefd's `appoint-existing`, which names somebody who is
 * already in the company. On a FRESH company that is nobody: the only person
 * there is the CEO, and the CEO never moves out of the company root it heads
 * (`exec-root-protected`). So the one verb an operator reaches for first —
 * "give this company a department" — could not succeed at all.
 *
 * The reason above is narrower than the one recorded on 2026-08-10, which said
 * every person on a fresh company was executive-root protected. That stopped
 * being true on 2026-08-13: only the CEO is. It does not change this choice,
 * because a fresh company still holds nobody BUT the CEO — but on a company
 * that has since hired, `appoint-existing` is now a real option for any
 * member, and a future form may offer it. A
 * new unit needs somebody to run it, and hiring that person in the same
 * transaction is what chiefd's `hire-new` path is for: the department and its
 * head commit together, so a failure can never leave a headless unit behind.
 *
 * # The ids and the title are chiefd's
 *
 * `departmentId`, the head's `personId` and the head's `title` are sent BLANK,
 * which chiefd reads as "you decide" and fills with `organization_spec`'s own
 * rules (`mint_department_create_ids`) — so a department created from the
 * browser is named exactly like one created at genesis. Deriving any of them
 * here would be a second implementation of a rule chiefd already owns, in a
 * second language. That is why the form no longer asks for a title either.
 *
 * # The hiring manager is the PARENT department's head
 *
 * chiefd refuses a `hire-new` create from a bare `{kind: 'operator'}`
 * requester: hiring is manager-driven. The manager here is not a choice the
 * browser makes — it is an org fact, read from the served tree: a unit reporting to
 * Engineering is created by the head of Engineering, and one reporting to the
 * root is created by the CEO. Asking the operator to pick one would be asking
 * them to guess at the org chart chiefd is already holding.
 *
 */
export async function createDepartment(
  companyKey: string,
  request: NewDepartmentRequest
): Promise<AppliedDepartment> {
  const parentId = required(request.parentId, 'parentId')
  const name = required(request.name, 'name')
  const purpose = required(request.purpose, 'purpose')
  const headName = required(request.head?.name, 'head.name')
  const headMandate = required(request.head?.mandate, 'head.mandate')

  const chiefd = await companyChiefd(companyKey)
  const tree = await chiefd.orgSlice.treeStructured(companyKey)
  const parentHeadPersonId = hiringManager(tree.departments, parentId, {
    unknown: 'unknown-parent',
    headless: 'parent-has-no-head',
    cannot: 'create a department under it'
  })

  return applied(
    await chiefd.staffing.createDepartment(
      companyKey,
      '',
      parentId,
      name,
      {
        kind: 'hire-new',
        personId: '',
        name: headName,
        title: '',
        mandate: headMandate,
        personKind: 'head',
        employmentState: 'active',
        activation: 'resident',
        // Empty rather than invented: chiefd decides what a head is given, and
        // a browser form has no opinion about tools. (There is no skill field
        // to leave empty: an agent's skills are the company directory's own
        // `.pi/skills`, which Pi loads for everybody — chief-home-is-cwd §3.)
        tools: [],
        prompts: []
      },
      { kind: 'person', personId: parentHeadPersonId },
      { purpose }
    )
  )
}
