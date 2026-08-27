// Hiring from the web: what it asks chiefd, and what it refuses to invent.
//
// The rule under test: a hire names NO ROUTE. Chief is out of the
// provider/model business, so a form that offered a provider, a model or an
// observation would be offering a choice the product does not have, and every
// new person boots as plain Pi on the operator's own defaults.
import { beforeEach, describe, expect, it, vi } from 'vitest'

const hirePerson = vi.fn()
const createDepartmentCall = vi.fn()

/** The forest `createDepartment` reads to find the hiring manager. `headless`
 * is a real state — a unit created before anybody was hired into it — and the
 * one case where there is nobody to attest the create. */
const TREE = {
  slug: 'acme',
  rootDepartmentId: 'executive',
  departments: [
    {
      id: 'executive',
      name: 'Executive',
      headPersonId: 'ceo',
      state: 'active' as const,
      people: [],
      children: [
        {
          id: 'engineering',
          name: 'Engineering',
          headPersonId: 'ada',
          state: 'active' as const,
          people: [],
          children: []
        },
        {
          id: 'headless',
          name: 'Skunkworks',
          headPersonId: '',
          state: 'active' as const,
          people: [],
          children: []
        }
      ]
    }
  ]
}

vi.mock('@/server/CompanyChiefd', () => ({
  companyChiefd: async () => ({
    orgSlice: { treeStructured: async () => TREE },
    staffing: {
      hirePerson: (...args: unknown[]) => hirePerson(...args),
      createDepartment: (...args: unknown[]) => createDepartmentCall(...args)
    }
  })
}))

const { createDepartment, hire, StaffingRequestError } = await import('@/server/Staffing')

const REQUEST = {
  departmentId: 'engineering',
  name: 'Ada',
  title: 'Engineer',
  mandate: 'Write the compiler.'
}

beforeEach(() => {
  hirePerson.mockReset().mockResolvedValue({ applied: true })
  createDepartmentCall.mockReset().mockResolvedValue({ applied: true, departmentId: 'x' })
})

describe('hire', () => {
  // THE RULE: nothing about a route reaches the wire, whatever the form sends.
  it('sends no provider, model, observation or expected selection', async () => {
    await hire('acme', REQUEST)

    const call = hirePerson.mock.calls[0] ?? []
    // FIVE arguments, down from seven. The model authority and the expected
    // selection were the sixth and seventh and went with model management; the
    // hiring-manager option bag was the sixth after that and went with it too
    // — it named a manager only so the hire could inherit that manager's
    // route. What is left is the company, the (blank) id, the department, the
    // seed, and the ONE attested requester.
    expect(call).toHaveLength(5)
    const [, , , seed] = call
    for (const field of ['provider', 'model', 'modelReason', 'observation', 'taskClass']) {
      expect(seed).not.toHaveProperty(field)
    }
  })

  it('throws chiefd’s refusal rather than returning it as a successful hire', async () => {
    // chiefd's atomic verbs answer `{applied: true, …}` or `{refused, detail}`,
    // and `routeResult` serialized BOTH as success. So a hire chiefd declined —
    // `operator-hirer-invalid`, `exec-root-protected` — reached the browser as
    // HTTP 200, the client parsed it against a schema that does not describe
    // it, and the operator was told nothing at all. A verb that did not happen
    // must not look like one that did.
    //
    // The code is chiefd's OWN, carried verbatim: it understood the request and
    // declined it, and its code is the only text that tells an operator what to
    // do differently.
    hirePerson.mockResolvedValue({
      refused: 'operator-hirer-invalid',
      detail: 'a direct operator has no durable hiring-manager route'
    })

    await expect(hire('acme', REQUEST)).rejects.toMatchObject({
      status: 422,
      code: 'operator-hirer-invalid',
      message: 'a direct operator has no durable hiring-manager route'
    })
    await expect(hire('acme', REQUEST)).rejects.toBeInstanceOf(StaffingRequestError)
  })

  it('leaves the person id to chiefd, exactly as a department’s head is', async () => {
    // This assertion used to read the other way round — that a missing
    // `personId` was REFUSED — and that assertion encoded the bug. chiefd's
    // `mint_hire_ids` fills a blank id with `<department>-<slugify(name)>`
    // before it validates anything else, the same rule
    // `mint_department_create_ids` applies below. Meanwhile the Hire form
    // collects a name, a title and a mandate and no id at all, so the refusal
    // fired on every hire the browser could make:
    // `422 missing-field: "personId" is required`. Observed live.
    await hire('acme', REQUEST)

    expect(hirePerson.mock.calls[0]?.[1]).toBe('')
  })

  it('refuses an empty mandate rather than hiring somebody with no job', async () => {
    await expect(hire('acme', { ...REQUEST, mandate: '   ' })).rejects.toMatchObject({
      code: 'missing-field'
    })
  })

  it('attests the hire as the target department’s own head', async () => {
    // chiefd refuses a bare `{kind: 'operator'}` requester —
    // `operator-hirer-invalid` — because hiring is manager-driven. Which
    // manager is an org fact read from the served tree, not a browser's
    // choice: a hire into Engineering is done by the head of Engineering.
    await hire('acme', REQUEST)

    expect(hirePerson.mock.calls[0]?.[4]).toEqual({ kind: 'person', personId: 'ada' })
  })

  it('sends the department head as the ONE authority, and no second one', async () => {
    // This used to assert that `requester` and `hiringManagerPersonId` named
    // the SAME person, because chiefd checked them against each other:
    // disagreement was `hiring-manager-mismatch` and an operator requester
    // carrying any manager id was `operator-hirer-invalid`. Both refusals
    // existed to protect ROUTE INHERITANCE — the manager was named so the new
    // person could inherit that manager's model — and with no route to inherit
    // neither has a subject. `hiringManagerPersonId` is deleted rather than
    // relaxed, so the assertion becomes: one authority reaches the wire, and
    // nothing follows it that could be mistaken for a second.
    await hire('acme', REQUEST)

    const [, , , , requester, sixth] = hirePerson.mock.calls[0] ?? []
    expect(requester).toEqual({ kind: 'person', personId: 'ada' })
    expect(sixth).toBeUndefined()
  })

  it('refuses a department this company does not have', async () => {
    await expect(hire('acme', { ...REQUEST, departmentId: 'nowhere' })).rejects.toMatchObject({
      code: 'unknown-department'
    })
    // Refused before chiefd is asked what it can run: a unit that cannot hire
    // is not a staffing question chiefd has to answer twice.
    expect(hirePerson).not.toHaveBeenCalled()
    expect(hirePerson).not.toHaveBeenCalled()
  })

  it('refuses a department with nobody in it to do the hiring', async () => {
    await expect(hire('acme', { ...REQUEST, departmentId: 'headless' })).rejects.toMatchObject({
      code: 'department-has-no-head'
    })
    expect(hirePerson).not.toHaveBeenCalled()
  })

  it('refuses a missing department id before reading the tree', async () => {
    await expect(hire('acme', { ...REQUEST, departmentId: undefined })).rejects.toBeInstanceOf(
      StaffingRequestError
    )
    expect(hirePerson).not.toHaveBeenCalled()
  })
})

describe('createDepartment', () => {
  const NEW_DEPARTMENT = {
    parentId: 'executive',
    name: 'Engineering',
    purpose: 'Build the product.',
    head: { name: 'Ada', mandate: 'Run engineering.' }
  }

  it('hires the new unit’s head with it, and leaves every id to chiefd', async () => {
    // `appoint-existing` — what this used to send — names somebody already in
    // the company, and on a fresh company that is nobody: the only person
    // there is the CEO, whom the executive root protects. So the first verb an
    // operator reaches for could not succeed at all.
    await createDepartment('acme', NEW_DEPARTMENT)

    const [, departmentId, parentId, name, head] = createDepartmentCall.mock.calls[0] ?? []
    expect([departmentId, parentId, name]).toEqual(['', 'executive', 'Engineering'])
    // Blank, not derived. chiefd fills the id and the title with
    // `organization_spec`'s own rules, so a department created from the
    // browser is named exactly like one created at genesis.
    expect(head).toMatchObject({
      kind: 'hire-new',
      personId: '',
      title: '',
      name: 'Ada',
      mandate: 'Run engineering.',
      personKind: 'head',
      employmentState: 'active'
    })
  })

  it('inherits the manager’s route rather than naming one', async () => {
    // No provider, no model and no model authority: omitting
    // them is what tells chiefd to inherit the company's Founder route, which
    // chiefd attested at genesis. A form that named one would need a provider
    // credential this server deliberately does not hold.
    await createDepartment('acme', NEW_DEPARTMENT)

    const [, , , , head, , opts] = createDepartmentCall.mock.calls[0] ?? []
    expect(head).not.toHaveProperty('provider')
    expect(head).not.toHaveProperty('model')
    expect(opts).toEqual({ purpose: 'Build the product.' })
    // ABSENT, never empty: an empty list means "I expect nobody", and chiefd's
    // preflight fence refused every such create as
    // `department-model-selection-changed`.
    expect(opts).not.toHaveProperty('expectedSelections')
    expect(opts).not.toHaveProperty('modelAuthority')
  })

  it('attests the create as the PARENT department’s head', async () => {
    // chiefd refuses a hire-new create from a bare operator, and the manager
    // is not a browser's choice: a unit under Engineering is created by the
    // head of Engineering, read from the served tree.
    await createDepartment('acme', { ...NEW_DEPARTMENT, parentId: 'engineering' })

    expect(createDepartmentCall.mock.calls[0]?.[5]).toEqual({ kind: 'person', personId: 'ada' })
  })

  it('refuses a parent this company does not have', async () => {
    await expect(
      createDepartment('acme', { ...NEW_DEPARTMENT, parentId: 'nowhere' })
    ).rejects.toMatchObject({ code: 'unknown-parent' })
    expect(createDepartmentCall).not.toHaveBeenCalled()
  })

  it('refuses a parent with nobody to do the hiring', async () => {
    // chiefd's own answer would be about a missing hiring manager, which does
    // not tell an operator that the unit they picked is empty.
    await expect(
      createDepartment('acme', { ...NEW_DEPARTMENT, parentId: 'headless' })
    ).rejects.toMatchObject({ code: 'parent-has-no-head' })
    expect(createDepartmentCall).not.toHaveBeenCalled()
  })

  it('refuses a department with no head', async () => {
    await expect(
      createDepartment('acme', { ...NEW_DEPARTMENT, head: undefined })
    ).rejects.toMatchObject({ code: 'missing-field' })
    expect(createDepartmentCall).not.toHaveBeenCalled()
  })

  it('throws chiefd’s refusal rather than returning it as a created unit', async () => {
    // The same defect as the hire above, on the other verb: a unit that was
    // NOT created answered 200, and the rail closed its form as if it had been.
    createDepartmentCall.mockResolvedValue({
      refused: 'exec-root-protected',
      detail: 'the executive root cannot be reparented'
    })

    await expect(createDepartment('acme', NEW_DEPARTMENT)).rejects.toMatchObject({
      status: 422,
      code: 'exec-root-protected'
    })
  })
})
