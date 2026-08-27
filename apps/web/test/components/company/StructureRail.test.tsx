// @vitest-environment jsdom
// Pausing and resuming a department from the structure rail.
//
// # Why this verb needed its own test
//
// Every other verb on this rail reports a refusal by REJECTING — the route maps
// chiefd's 422 onto the error envelope and `ChiefApiClientService` throws. Pause
// and resume do not: chiefd's `AtomicDirectOutcome` carries `{refused, detail}`
// as a SUCCESSFUL value, the route passes it through, and the response is a 200.
//
// So `await api.pauseDepartment(...)` RESOLVES on exactly the cases an operator
// most needs to read — pausing the executive root answers
// `exec-root-protected` — and a handler that only awaited it would clear the
// busy flag, close the draft and show nothing at all. Silent failure, on the
// one verb whose failure is most likely. The rail turns that value into a
// thrown error so it lands on the same surface as every other verb, in chiefd's
// own words, and the last test below is the whole reason that exists.
//
// The REAL `ChiefApiClientService` over an injected `fetch` throughout, never a
// hand-written client stub: a stub would be a second opinion about what the
// client does, and a client that agrees only with its own stub is the exact
// shape of defect this suite keeps finding.
import {
  createFakeChiefApi,
  FIXTURE_COMPANY_KEY as COMPANY_KEY,
  FIXTURE_JWT
} from '@test/harness/FakeChiefApi'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { StructureRail } from '@/components/company/StructureRail'
import { ApiSessionProvider } from '@/providers/ApiSessionProvider'
import { ChiefApiClientService } from '@/services/ChiefApiClientService'
import type { DepartmentNode } from '@/types/ChiefApi'
import type { FetchImpl } from '@/types/Fetch'

const BASE_URL = 'http://web.example'

/** Root, one active child, one paused child. The root is deliberately present:
 * chiefd refuses `exec-root-protected` there, and the rail must not offer a
 * choice that can only fail. */
function departments(): DepartmentNode[] {
  return [
    {
      id: 'executive',
      name: 'Executive',
      headPersonId: 'ceo',
      state: 'active',
      people: [
        { id: 'ceo', name: 'Cleo', title: 'CEO', kind: 'executive', employmentState: 'active' }
      ],
      children: [
        {
          id: 'engineering',
          name: 'Engineering',
          headPersonId: 'ada',
          state: 'active',
          people: [
            { id: 'ada', name: 'Ada', title: 'Engineer', kind: 'worker', employmentState: 'active' }
          ],
          children: []
        },
        {
          id: 'sales',
          name: 'Sales',
          headPersonId: 'sam',
          state: 'paused',
          people: [
            { id: 'sam', name: 'Sam', title: 'Rep', kind: 'worker', employmentState: 'active' }
          ],
          children: []
        }
      ]
    }
  ]
}

let container: HTMLDivElement
let root: Root

beforeEach(() => {
  container = document.createElement('div')
  document.body.appendChild(container)
  root = createRoot(container)
})

afterEach(() => {
  act(() => root.unmount())
  container.remove()
})

function render(fetchImpl: FetchImpl, tree: DepartmentNode[] = departments()): void {
  const client = new ChiefApiClientService({
    baseUrl: BASE_URL,
    accessToken: () => FIXTURE_JWT,
    fetchImpl
  })
  act(() => {
    root.render(
      <ApiSessionProvider client={client} tokenGetter={(): string => FIXTURE_JWT}>
        <StructureRail companyKey={COMPANY_KEY} departments={tree} readOnly={false} />
      </ApiSessionProvider>
    )
  })
}

function button(selector: string): HTMLButtonElement {
  const found = container.querySelector<HTMLButtonElement>(selector)
  if (!found) throw new Error(`expected a control matching ${selector}`)
  return found
}

describe('StructureRail — pause and resume', () => {
  it('offers exactly one of Pause and Resume, chosen by the department’s own state', () => {
    // A department is active or paused. Offering both would let an operator
    // press the one that is already true, and the tree's `state` field — which
    // comes back from chiefd's `org-manifest` push — is what flips it.
    render(createFakeChiefApi().fetchImpl)

    expect(button('[data-department-pause="engineering"]').textContent).toBe('Pause')
    expect(container.querySelector('[data-department-resume="engineering"]')).toBeNull()

    expect(button('[data-department-resume="sales"]').textContent).toBe('Resume')
    expect(container.querySelector('[data-department-pause="sales"]')).toBeNull()
  })

  it('offers neither for the executive root', () => {
    // chiefd refuses that with `exec-root-protected`, and the same reasoning
    // already hides Move there: do not offer a choice that can only fail.
    render(createFakeChiefApi().fetchImpl)

    expect(container.querySelector('[data-department-pause="executive"]')).toBeNull()
    expect(container.querySelector('[data-department-resume="executive"]')).toBeNull()
  })

  it('names a paused unit rather than styling it away', () => {
    // A paused unit still exists, and an operator who cannot see that it is
    // paused has no reason to press Resume.
    render(createFakeChiefApi().fetchImpl)

    expect(container.textContent).toContain('paused')
  })

  it('pauses through the department’s own route, with no form in the way', async () => {
    // Pause and resume take no operator input — a unit is running or it is not
    // — so they go straight through rather than opening a draft.
    const { fetchImpl, requests } = createFakeChiefApi()
    render(fetchImpl)

    await act(async () => {
      button('[data-department-pause="engineering"]').click()
    })

    expect(requests).toHaveLength(1)
    expect(requests[0]?.method).toBe('POST')
    expect(requests[0]?.path).toBe(`/companies/${COMPANY_KEY}/departments/engineering/pause`)
    expect(container.querySelector('[data-structure-error="true"]')).toBeNull()
  })

  it('resumes through the inverse route', async () => {
    const { fetchImpl, requests } = createFakeChiefApi()
    render(fetchImpl)

    await act(async () => {
      button('[data-department-resume="sales"]').click()
    })

    expect(requests[0]?.path).toBe(`/companies/${COMPANY_KEY}/departments/sales/resume`)
  })

  it('shows chiefd’s refusal even though the route answered 200', async () => {
    // The defect: `{refused, detail}` arrives as a SUCCESSFUL value, so the
    // promise resolves and a handler that only awaited it would report nothing
    // at all. chiefd's own code and detail are shown verbatim — "a direct
    // operator has no durable hiring-manager route" tells an operator what to
    // do, and "failed" does not.
    const fetchImpl: FetchImpl = async () =>
      new Response('{"refused":"exec-root-protected","detail":"the executive root stays up"}', {
        status: 200,
        headers: { 'content-type': 'application/json' }
      })
    render(fetchImpl)

    await act(async () => {
      button('[data-department-pause="engineering"]').click()
    })

    const alert = container.querySelector('[data-structure-error="true"]')
    expect(alert?.getAttribute('role')).toBe('alert')
    expect(alert?.textContent).toContain('exec-root-protected')
    expect(alert?.textContent).toContain('the executive root stays up')
  })

  it('offers no state change at all on a company that is not running', () => {
    // Every route below resolves a chiefd client first, so a stopped company
    // cannot be restructured — and a button that can only fail is worse than
    // its absence.
    const client = new ChiefApiClientService({
      baseUrl: BASE_URL,
      accessToken: () => FIXTURE_JWT,
      fetchImpl: createFakeChiefApi().fetchImpl
    })
    act(() => {
      root.render(
        <ApiSessionProvider client={client} tokenGetter={(): string => FIXTURE_JWT}>
          <StructureRail companyKey={COMPANY_KEY} departments={departments()} readOnly />
        </ApiSessionProvider>
      )
    })

    expect(container.querySelector('[data-department-pause="engineering"]')).toBeNull()
    expect(container.querySelector('[data-department-resume="sales"]')).toBeNull()
    expect(container.textContent).toContain('Boot it to change its structure')
  })
})

describe('StructureRail — somebody who has left', () => {
  /** Engineering with a head and two workers: one still employed, one gone.
   * Transfer and Offboard only ever render for a NON-head, so a fixture of
   * heads alone cannot see this rule at all — which is why nothing caught it. */
  function withDepartedWorker(): DepartmentNode[] {
    return [
      {
        id: 'executive',
        name: 'Executive',
        headPersonId: 'ceo',
        state: 'active',
        people: [
          { id: 'ceo', name: 'Cleo', title: 'CEO', kind: 'executive', employmentState: 'active' }
        ],
        children: [
          {
            id: 'engineering',
            name: 'Engineering',
            headPersonId: 'ada',
            state: 'active',
            people: [
              {
                id: 'ada',
                name: 'Ada',
                title: 'Engineer',
                kind: 'worker',
                employmentState: 'active'
              },
              {
                id: 'bob',
                name: 'Bob',
                title: 'Engineer',
                kind: 'worker',
                employmentState: 'active'
              },
              {
                id: 'zed',
                name: 'Zed',
                title: 'Engineer',
                kind: 'worker',
                employmentState: 'departed'
              }
            ],
            children: []
          }
        ]
      }
    ]
  }

  function personRow(personId: string): HTMLElement {
    const found = container.querySelector<HTMLElement>(`[data-person-id="${personId}"]`)
    if (!found) throw new Error(`expected ${personId} to still be listed`)
    return found
  }

  function verbs(personId: string): string[] {
    return Array.from(personRow(personId).querySelectorAll('button')).map(
      (control) => control.textContent ?? ''
    )
  }

  it('still LISTS somebody who has left', () => {
    // The manifest keeps them and chiefd's tree places them; dropping them here
    // would be the rail deciding history. The bug was never that they showed —
    // it was that they showed as though nothing had happened.
    render(createFakeChiefApi().fetchImpl, withDepartedWorker())

    expect(personRow('zed').textContent).toContain('Zed')
  })

  it('names the departure rather than styling it away', () => {
    // Same rule as a paused department one describe above: an operator who
    // cannot see that somebody left cannot tell the roster from the alumni.
    render(createFakeChiefApi().fetchImpl, withDepartedWorker())

    expect(personRow('zed').textContent).toContain('departed')
    expect(personRow('bob').textContent).not.toContain('departed')
    expect(personRow('zed').dataset.employmentState).toBe('departed')
    expect(personRow('bob').dataset.employmentState).toBe('active')
  })

  it('offers a departed person neither Transfer nor Offboard', () => {
    // THE DEFECT. `employmentState` never left chiefd's `/v1/org/tree/structured`
    // projection, so a departed person was byte-for-byte identical to an active
    // one here and kept both verbs. chiefd routes transfer and offboard through
    // `movable_worker`, which refuses a departed target with `PERSON_DEPARTED`,
    // so both buttons were choices that could only fail — offered on somebody an
    // operator had already offboarded. Observed live on `research-bram`.
    render(createFakeChiefApi().fetchImpl, withDepartedWorker())

    expect(verbs('zed')).toEqual([])
  })

  it('still offers both to a colleague who is still here', () => {
    // The other half: withdrawing the verbs must not withdraw them from
    // everybody. A rail that hid Transfer for the whole department would pass
    // the test above and be just as broken.
    render(createFakeChiefApi().fetchImpl, withDepartedWorker())

    expect(verbs('bob')).toEqual(['Transfer', 'Offboard'])
  })

  it('leaves the head’s existing rule alone', () => {
    // A head never had these verbs — chiefd reparents the whole unit instead —
    // and that is a different reason from departure. Both still hold.
    render(createFakeChiefApi().fetchImpl, withDepartedWorker())

    expect(verbs('ada')).toEqual([])
  })
})
