/**
 * THE USERNAME IS THE COMMUNICATION IDENTITY.
 *
 * Operator ruling: "Every time, use the USERNAME. That's how we communicate."
 *
 * A person is three strings and they are not interchangeable:
 *
 *  - `id` — a kebab slug such as `portfolio-management-head`. It is the
 *    durable addressing and storage key: mailbox paths, document-store URLs,
 *    transcripts and environment variables are all keyed by it.
 *  - `name` — the roster display name, "Priya Sharma".
 *  - the USERNAME — `priya`, derived from the name.
 *
 * The failure this pins is not cosmetic. Every surface an agent read named
 * people by their id: the delivered message text, the inbox card, the sender
 * line. An agent that is shown an id addresses people by id, and a reply
 * addressed to a person who does not exist is delivered nowhere. The naming
 * and the addressing are one bug, which is why presentation and resolution are
 * asserted together here.
 */
import type {
  IntercomOrganizationManifest,
  OrganizationEnvelope
} from '@test-assets/organization-intercom'
import {
  messageContextForTest,
  primeManifestForTest,
  recipientsForTest
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const ORGANIZATION = 'leo-capital'

function manifest(
  people: Array<{ id: string; name: string; departed?: boolean }>
): IntercomOrganizationManifest {
  return {
    schemaVersion: 1,
    kind: 'organization',
    slug: ORGANIZATION,
    name: 'Leo Capital',
    rootDepartmentId: 'root',
    departmentOrder: ['root'],
    peopleOrder: people.map((person) => person.id),
    departments: {},
    people: Object.fromEntries(
      people.map((person) => [
        person.id,
        {
          id: person.id,
          name: person.name,
          title: 'Analyst',
          kind: 'worker' as const,
          departmentId: 'root',
          employmentState: person.departed === true ? ('departed' as const) : ('active' as const),
          createdAt: '2026-01-01T00:00:00.000Z'
        }
      ])
    )
  }
}

/** The message a refusal carries, so a test can assert what it TELLS the caller. */
function refusalMessage(attempt: () => unknown): string {
  try {
    attempt()
  } catch (error) {
    return error instanceof Error ? error.message : String(error)
  }
  throw new Error('expected a refusal, but the call succeeded')
}

const ROSTER = manifest([
  { id: 'portfolio-management-head', name: 'Priya Sharma' },
  { id: 'signal-researcher', name: 'Dana Okafor' },
  { id: 'retired-analyst', name: 'Sam Vance', departed: true }
])

const ENVELOPE: OrganizationEnvelope = {
  schemaVersion: 1,
  id: 'msg-1',
  organization: ORGANIZATION,
  fromPersonId: 'portfolio-management-head',
  to: 'signal-researcher',
  recipients: ['signal-researcher'],
  body: 'Status please.',
  urgency: 'normal',
  createdAt: '2026-01-01T00:00:00.000Z'
}

describe('a delivered message names its sender by username', () => {
  test('the prompt an agent reads carries @priya, not the kebab id', () => {
    primeManifestForTest(ORGANIZATION, ROSTER)
    const context = messageContextForTest(ENVELOPE, 'signal-researcher')

    expect(context).toContain('@priya')
    expect(context).not.toContain('portfolio-management-head')
    // The envelope's own id is still an id. Ids inside ids are fine; it is the
    // PERSON that has to be a name.
    expect(context).toContain('msg-1')
  })

  test('an unknown sender degrades to the raw id rather than throwing', () => {
    // A cross-organization sender is a real case and its handle is not ours to
    // invent. This is exactly the old behaviour, so it can never be a
    // regression — only the known case improves.
    primeManifestForTest(ORGANIZATION, ROSTER)
    const foreign = { ...ENVELOPE, organization: 'another-company', fromPersonId: 'someone-else' }
    expect(messageContextForTest(foreign, 'signal-researcher')).toContain('someone-else')
  })
})

describe('a recipient may be addressed by username or id', () => {
  test('the username resolves to the person id', () => {
    expect(recipientsForTest(ROSTER, 'signal-researcher', 'priya')).toEqual([
      'portfolio-management-head'
    ])
    expect(recipientsForTest(ROSTER, 'signal-researcher', '@priya')).toEqual([
      'portfolio-management-head'
    ])
  })

  test('the id still resolves, unchanged', () => {
    // The key path must not regress: everything that addresses by id today
    // keeps working, and takes the same exact-match fast path it always did.
    expect(recipientsForTest(ROSTER, 'signal-researcher', 'portfolio-management-head')).toEqual([
      'portfolio-management-head'
    ])
  })

  test('a departed person is not reachable by either spelling', () => {
    expect(() => recipientsForTest(ROSTER, 'signal-researcher', 'sam')).toThrow(/Unknown employed/)
    expect(() => recipientsForTest(ROSTER, 'signal-researcher', 'retired-analyst')).toThrow(
      /Unknown employed/
    )
  })
})

describe('an ambiguous username is refused, naming both people', () => {
  test('two people who share a first name are never guessed between', () => {
    // Guessing would deliver somebody's message to the wrong person and say
    // nothing, which is strictly worse than refusing. The refusal has to name
    // both candidates or the sender cannot act on it.
    const twoPriyas = manifest([
      { id: 'portfolio-management-head', name: 'Priya Sharma' },
      { id: 'risk-lead', name: 'Priya Venkatesan' },
      { id: 'signal-researcher', name: 'Dana Okafor' }
    ])

    const message = refusalMessage(() => recipientsForTest(twoPriyas, 'signal-researcher', 'priya'))

    expect(message).toContain('ambiguous')
    expect(message).toContain('portfolio-management-head')
    expect(message).toContain('risk-lead')
  })
})

describe('an unknown recipient error lists usernames', () => {
  test('the guidance an agent copies from is written in usernames', () => {
    // This error text is not diagnostics: it is the list an agent reads and
    // then addresses somebody from. Listing ids here is how an agent learns to
    // send to ids.
    const message = refusalMessage(() => recipientsForTest(ROSTER, 'signal-researcher', 'nobody'))

    expect(message).toContain('@priya')
    expect(message).toContain('@dana')
    // The id stays available in parentheses for anyone addressing by key.
    expect(message).toContain('(portfolio-management-head)')
    expect(message).toContain('or all')
  })
})
