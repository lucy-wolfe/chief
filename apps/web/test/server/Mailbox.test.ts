// One person's mail, counted once, on the server.
//
// # Why this is not a pass-through, and why that is tested here
//
// chiefd answers `/v1/org/mailbox/read-person` with a row read whose `document`
// is a SERIALIZED JSON string — `{"entries":[…]}` — because the docstore stores
// documents as text and hands them back as text. The route used to forward that
// verbatim while the browser's schema declared `{pendingCount, envelopes}`, a
// shape apps/api synthesised and nothing sends any more, so every mailbox read
// threw a ZodError in the client.
//
// The seam is closed on this side deliberately: a second JSON parse in the
// browser would make the page a reader of chiefd's storage format, and
// `pending` would then be counted in two places.
//
// # `pending` is the only live bucket
//
// chiefd's `MailboxState` has one live bucket and five terminal ones
// (`delivered`, `accepted`, `superseded`, `rejected`, `resolved`). The badge an
// operator reads to decide whether somebody needs waking must not count
// archived mail, so every one of those is exercised below by name.
import { beforeEach, describe, expect, it, vi } from 'vitest'

const readPerson = vi.fn()

vi.mock('@/server/CompanyChiefd', () => ({
  companyChiefd: async () => ({
    mailbox: { readPerson: (...args: unknown[]) => readPerson(...args) }
  })
}))

const { personMailbox } = await import('@/server/Mailbox')

/** chiefd's row read: the document is TEXT, which is the whole point. */
function row(entries: readonly unknown[]): { document: string } {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture serialization, mirroring the docstore's own text column.
  return { document: JSON.stringify({ entries }) }
  /* eslint-enable lucy/no-json-stringify */
}

beforeEach(() => {
  readPerson.mockReset()
})

describe('personMailbox', () => {
  it('parses chiefd’s serialized document rather than forwarding it', async () => {
    // Forwarding the row is what threw a ZodError in the client on every single
    // mailbox read: `{document: "…"}` is not `{personId, pendingCount,
    // envelopes}` and never could be.
    readPerson.mockResolvedValue(row([{ id: 'e1', state: 'pending' }]))

    await expect(personMailbox('acme', 'person-ceo')).resolves.toEqual({
      personId: 'person-ceo',
      pendingCount: 1,
      envelopes: [{ id: 'e1', state: 'pending' }]
    })
  })

  it('asks chiefd for the company’s own person', async () => {
    readPerson.mockResolvedValue(row([]))

    await personMailbox('acme', 'person-ceo')

    expect(readPerson).toHaveBeenCalledWith('acme', 'person-ceo')
  })

  it('counts ONLY pending, never an archive state', async () => {
    // Every terminal bucket chiefd has, by name. Counting any of them would put
    // mail that has already been dealt with on the badge that decides whether
    // an operator wakes somebody up.
    readPerson.mockResolvedValue(
      row([
        { id: 'a', state: 'pending' },
        { id: 'b', state: 'delivered' },
        { id: 'c', state: 'accepted' },
        { id: 'd', state: 'superseded' },
        { id: 'e', state: 'rejected' },
        { id: 'f', state: 'resolved' },
        { id: 'g', state: 'pending' }
      ])
    )

    const mailbox = await personMailbox('acme', 'person-ceo')

    expect(mailbox.pendingCount).toBe(2)
    // The archive is still FORWARDED — it is the operator's history. Only the
    // count is narrowed.
    expect(mailbox.envelopes).toHaveLength(7)
  })

  it('does not count an entry whose state it cannot read', async () => {
    // chiefd's own parser refuses to default an unreadable state, for the same
    // reason: a badge that guessed would send somebody to wake an agent that
    // has nothing waiting.
    readPerson.mockResolvedValue(
      row([{ id: 'a' }, { id: 'b', state: 42 }, { id: 'c', state: null }, 'not-an-entry'])
    )

    expect((await personMailbox('acme', 'person-ceo')).pendingCount).toBe(0)
  })

  it('reports an unreadable document as an EMPTY mailbox rather than a failure', async () => {
    // The mailbox is a badge beside a person. Failing the whole pane over an
    // unparseable storage row would hide a working agent behind a detail the
    // operator cannot act on.
    readPerson.mockResolvedValue({ document: 'not json at all' })

    await expect(personMailbox('acme', 'person-ceo')).resolves.toEqual({
      personId: 'person-ceo',
      pendingCount: 0,
      envelopes: []
    })
  })

  it('treats a row with no document, or no entries, as an empty mailbox', async () => {
    // A person nobody has written to yet has no row content at all, which is
    // the ordinary state of most of a company.
    readPerson.mockResolvedValue({})
    expect((await personMailbox('acme', 'person-ceo')).envelopes).toEqual([])

    readPerson.mockResolvedValue({ document: '{"entries":"soon"}' })
    expect((await personMailbox('acme', 'person-ceo')).envelopes).toEqual([])

    readPerson.mockResolvedValue({ document: '[]' })
    expect((await personMailbox('acme', 'person-ceo')).envelopes).toEqual([])
  })
})
