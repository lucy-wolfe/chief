// A killed pane resumes and the agent re-issues the tool call it never got a
// result for. `org_send` must deliver that message ONCE — and must still
// deliver a message the sender genuinely means to repeat. Both directions are
// asserted here; the second is the one that proves a duplicate bug was not
// traded for a silent-data-loss bug.
import { describe, expect, it } from 'vitest'

import { resolveSendReplay } from '../../extensions/org-send-replay'

type Envelope = { id: string; createdAt: string; body: string }

const FINGERPRINT = 'a1b2c3d4e5f60718293a4b5c'
const AT = (iso: string): number => Date.parse(iso)

type Mailbox = {
  held: Map<string, Envelope>
  lookup: (candidateId: string) => Promise<Envelope | undefined>
  deliver: (id: string, createdAt: string, body: string) => boolean
}

/** A mailbox that only ever holds what a send actually delivered. */
function mailbox(initial: Envelope[] = []): Mailbox {
  const held = new Map(initial.map((entry) => [entry.id, entry]))
  return {
    held,
    lookup: (candidateId: string): Promise<Envelope | undefined> =>
      Promise.resolve(held.get(candidateId)),
    deliver: (id: string, createdAt: string, body: string): boolean => {
      if (held.has(id)) return false
      held.set(id, { id, createdAt, body })
      return true
    }
  }
}

describe('org_send replay identity', () => {
  it('a replay after a resume delivers nothing new and names the first delivery', async () => {
    const box = mailbox()

    // The original call, interrupted after it committed the mailbox append.
    const first = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:00.000Z'),
      lookup: box.lookup
    })
    expect(first.replayedFrom).toBeUndefined()
    expect(box.deliver(first.id, '2026-08-11T10:00:00.000Z', 'any update?')).toBe(true)

    // The resumed agent re-issues the same call. Under the OLD key this
    // carried a fresh Pi tool-call id and therefore a fresh message id; the
    // content fingerprint does not change, so it collides.
    const replay = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:22.000Z'),
      lookup: box.lookup
    })
    expect(replay.id).toBe(first.id)
    expect(replay.replayedFrom).toBe('2026-08-11T10:00:00.000Z')
    expect(box.deliver(replay.id, '2026-08-11T10:00:22.000Z', 'any update?')).toBe(false)

    expect(box.held.size).toBe(1)
  })

  it('the same words sent again outside the window ARE delivered', async () => {
    const box = mailbox()

    const first = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:00.000Z'),
      lookup: box.lookup
    })
    box.deliver(first.id, '2026-08-11T10:00:00.000Z', 'any update?')

    // Twenty minutes later the sender asks again. This is not a replay and
    // must not be swallowed: a message the sender believes was delivered and
    // was not is invisible, which is strictly worse than a duplicate.
    const deliberate = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:20:00.000Z'),
      lookup: box.lookup
    })
    expect(deliberate.id).not.toBe(first.id)
    expect(deliberate.replayedFrom).toBeUndefined()
    expect(box.deliver(deliberate.id, '2026-08-11T10:20:00.000Z', 'any update?')).toBe(true)

    expect(box.held.size).toBe(2)
  })

  it('a replay OF a deliberate repeat collides with that repeat, not the original', async () => {
    const box = mailbox()
    const first = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:00.000Z'),
      lookup: box.lookup
    })
    box.deliver(first.id, '2026-08-11T10:00:00.000Z', 'any update?')
    const repeat = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:20:00.000Z'),
      lookup: box.lookup
    })
    box.deliver(repeat.id, '2026-08-11T10:20:00.000Z', 'any update?')

    // The repeat is interrupted in its turn. Indexing is deterministic under
    // replay because the replay observes the same prior messages.
    const replayOfRepeat = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:20:11.000Z'),
      lookup: box.lookup
    })
    expect(replayOfRepeat.id).toBe(repeat.id)
    expect(replayOfRepeat.replayedFrom).toBe('2026-08-11T10:20:00.000Z')
    expect(box.held.size).toBe(2)
  })

  it('a corrupt stored timestamp delivers rather than swallowing', async () => {
    const box = mailbox([
      { id: `msg-${FINGERPRINT}`, createdAt: 'not-a-date', body: 'any update?' }
    ])
    const resolved = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:00.000Z'),
      lookup: box.lookup
    })
    expect(resolved.id).not.toBe(`msg-${FINGERPRINT}`)
    expect(resolved.replayedFrom).toBeUndefined()
  })

  it('the id is a function of content only — no clock, no process, no tool-call id', async () => {
    const box = mailbox()
    const a = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T10:00:00.000Z'),
      lookup: box.lookup
    })
    const b = await resolveSendReplay({
      fingerprint: FINGERPRINT,
      nowMs: AT('2026-08-11T18:44:03.000Z'),
      lookup: box.lookup
    })
    expect(a.id).toBe(b.id)
    expect(a.id).toBe(`msg-${FINGERPRINT}`)
  })
})
