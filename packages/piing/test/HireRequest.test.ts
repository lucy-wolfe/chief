/**
 * #751/P3: the person an `org_hire` call describes, as chiefd's API accepts it.
 *
 * This mapping used to be spread across a CLI's argument parsing, a JSON
 * document on that CLI's stdin, and that CLI's own client-side id minting, and
 * it reached the daemon by spawning `bun apps/cli/src/Main.ts org hire`.
 * `apps/cli` is deleted, so that spawn answered `chiefd: unknown command 'org'`
 * and a manager could not hire anybody at all.
 *
 * `hireRequest` is the whole translation now, and it is pure, so these tests
 * pin it with no daemon, no subprocess and no company. What they CANNOT prove
 * is that chiefd accepts the shape or mints the blanks the way this function
 * assumes — that is `apps/chiefd/crates/chiefd-api/tests/org_person_hire_http.rs`
 * plus the live exercise, and no amount of agreement between a fake and this
 * function would substitute for either.
 */
import type { ChiefdHireRequest } from '@test-assets/organization-intercom'
import { hireRequest } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

function request(person: Record<string, unknown> = {}): ChiefdHireRequest {
  return hireRequest({
    slug: 'acme@0123456789ab',
    departmentId: 'growth',
    hiringManagerPersonId: 'ceo',
    person: { name: 'Dana Rivers', mandate: 'Own the funnel end to end.', ...person }
  })
}

describe('hireRequest', () => {
  test('the id and title a manager did not choose are left for chiefd to mint', () => {
    const body = request()
    // EMPTY, not invented. The CLI minted `${department}-${slugify(name)}` here
    // — a second opinion about a name chiefd already knows how to derive — and
    // sent `title ?? ""` straight into a seed validator that refuses blanks, so
    // a hire naming no title could never succeed.
    expect(body.personId).toBe('')
    expect(body.title).toBe('')
  })

  test('an id and title the manager DID choose are carried through untouched', () => {
    const body = request({ id: 'dana', title: 'Growth Lead' })
    expect(body.personId).toBe('dana')
    expect(body.title).toBe('Growth Lead')
  })

  test('the attested manager is the requester, and the ONLY authority on the wire', () => {
    // This used to assert a second field, `hiringManagerPersonId`, carrying the
    // same id — chiefd refused `hiring-manager-mismatch` when the two
    // disagreed. That refusal protected ROUTE INHERITANCE: the manager was
    // named so the new person could inherit that manager's model. With no
    // route to inherit the field names nothing the route decides, so it is
    // deleted and chiefd refuses it as unknown rather than ignoring it.
    const body = request()
    expect(body.requester).toEqual({ kind: 'person', personId: 'ceo' })
    expect(body).not.toHaveProperty('hiringManagerPersonId')
  })

  // THE RULE: chief is out of the provider/model business, so a hire carries
  // NO route, NO task class and no attestation of either. A manager that tries
  // to name one is not obeyed quietly — the field is dropped, because there is
  // nothing on the wire for it to travel in and a new person boots as plain Pi
  // on the operator's own defaults like everybody else. `taskClass` goes with
  // them: it only ever existed to pick a model out of the catalog.
  test('a hire carries no route, task class or attestation of one', () => {
    const named = request({
      provider: 'anthropic',
      model: 'claude-opus-4',
      taskClass: 'coding-senior'
    })
    for (const field of [
      'provider',
      'model',
      'modelReason',
      'taskClass',
      'observation',
      'expectedProvider',
      'expectedModel',
      'expectedModelReason'
    ]) {
      expect(named).not.toHaveProperty(field)
      expect(request()).not.toHaveProperty(field)
    }
  })

  test('startActive:false is the only thing that benches a new hire', () => {
    expect(request().employmentState).toBe('active')
    expect(request({ startActive: true }).employmentState).toBe('active')
    expect(request({ startActive: false }).employmentState).toBe('benched')
  })

  test('the tool grant defaults to empty and keeps only strings', () => {
    expect(request().tools).toEqual([])
    expect(request({ tools: ['read', 7, 'bash'] }).tools).toEqual(['read', 'bash'])
  })

  test('an automatic org tool is refused before the hire request is built', () => {
    expect(() => request({ tools: ['org_send'] })).toThrow(
      /Never put org_\* names.*installed automatically.*omit them/i
    )
  })

  // chief-home-is-cwd §3/§4e: a hire selects no Pi resource. The three arrays
  // this used to assert (`skills`/`extensions`/`packages`, plus #1093's already
  // deleted `resourceRationale`) are gone from the request shape, so what has
  // to be pinned is that nothing puts them BACK on the wire — chiefd's hire
  // struct is `deny_unknown_fields`, so a resurrected key would 400 every hire.
  test('no resource selection reaches the wire, whatever the caller passes', () => {
    const body = request({
      skills: ['reviewing'],
      extensions: ['weather'],
      packages: ['qa-kit'],
      resourceRationale: { reviewing: 'the field is gone' }
    })
    expect(body).not.toHaveProperty('skills')
    expect(body).not.toHaveProperty('extensions')
    expect(body).not.toHaveProperty('packages')
    expect(body).not.toHaveProperty('resourceRationale')
  })

  test('the route the person is hired into is the department the tool named', () => {
    expect(request().departmentId).toBe('growth')
    expect(request().slug).toBe('acme@0123456789ab')
  })

  test('a hire with no name or no mandate is refused before any call is made', () => {
    expect(() =>
      hireRequest({
        slug: 'acme@0123456789ab',
        departmentId: 'growth',
        hiringManagerPersonId: 'ceo',
        person: { mandate: 'Own it.' }
      })
    ).toThrow(/requires a name/)
    expect(() =>
      hireRequest({
        slug: 'acme@0123456789ab',
        departmentId: 'growth',
        hiringManagerPersonId: 'ceo',
        person: { name: 'Dana Rivers' }
      })
    ).toThrow(/requires a mandate/)
  })
})
