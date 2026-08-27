/**
 * A REFUSED TURN MUST NOT SILENTLY DESTROY THE MESSAGE THAT STARTED IT.
 *
 * Measured on the operator's live company (a live box, 2026-08-24),
 * caught happening twice during the investigation itself:
 *
 *   11:49:54 message-queued    intel-lead -> intel-news  "CADENCE + X-LOOP ..."
 *   11:50:08 message-accepted  msg-19cfc543...           <- consumed
 *   11:50:20 provider-turn-failed content_filter
 *   11:50:20 work-resume-settle                          <- looks DONE
 *   11:52:53 message-queued    intel-lead -> intel-news  "PRE-OPEN SWEEP ..."
 *   11:52:54 message-accepted  msg-7db7e80f...           <- consumed
 *   11:53:08 provider-turn-failed content_filter
 *
 * Acceptance is at TURN START and is correct — an envelope must not stay
 * pending while a turn reads it, or a crash re-delivers work already begun. The
 * defect is what happens when the turn then DIES: the envelope is gone, the
 * turn answered nothing, the person settles as though they had finished, and
 * the sender is told nothing at all. Iris sent two operational requests into a
 * void and had no way to learn either had ceased to exist.
 *
 * # Why this drives the handler instead of testing a helper
 *
 * `ProviderFailureClassification.test.ts` records the lesson: a classifier that
 * NAMES a thing correctly proves nothing about a caller that ignores the name.
 * The rule here is not "a bounce message can be composed" — it is "the failure
 * path sends one" — so this drives the real `message_start` and `agent_end`
 * handlers through `test/support/InstalledPane.ts`, over a stub chiefd whose
 * mailbox is real enough to consume a row, and reads the durable trail back off
 * disk.
 */
import { installedPane, stopInstalledPanes, TRANSIENT } from '@test/support/InstalledPane'
import { isNullish } from '@test/support/Nullish'
import type { OrganizationEvent, Pane } from '@test/types/InstalledPane'
import { afterEach, describe, expect, test } from 'vitest'

afterEach(stopInstalledPanes)

/** The refusal exactly as DeepSeek's route produces it through Pi. */
const CONTENT_FILTER = 'Provider finish_reason: content_filter'

/** The 402 the operator's own account produced, 2026-08-20. */
const INSUFFICIENT_CREDITS = '402: {"message":"insufficient_credits"}'

function of(pane: Pane, event: string): readonly OrganizationEvent[] {
  return pane.events().filter((entry) => entry.event === event)
}

/** Every message this pane SENT. Two DIFFERENT things land here on a first
 *  refusal and they must not be conflated: the bounce to whoever lost a
 *  message, and the one-off content-refusal alert to the manager. In this
 *  fixture the CEO is both, so a helper that counted "messages sent" would
 *  read one of them as a duplicate of the other. */
function sent(pane: Pane): readonly OrganizationEvent[] {
  return of(pane, 'message-queued')
}

function bounces(pane: Pane): readonly OrganizationEvent[] {
  return sent(pane).filter((entry) => (entry.body ?? '').includes('could not process'))
}

function alerts(pane: Pane): readonly OrganizationEvent[] {
  return sent(pane).filter((entry) => (entry.body ?? '').includes('Content refusal'))
}

/**
 * A rendered card, with its wrapping and padding collapsed.
 *
 * The card is drawn into a fixed width, so any sentence long enough to be worth
 * asserting is long enough to be broken across lines by the renderer. Matching
 * raw output therefore tests the WRAP POINT, which is not the rule and moves
 * whenever the wording near it does.
 */
function flat(drawn: string): string {
  return drawn.replaceAll(/\s+/gu, ' ').trim()
}

describe('a turn the provider refuses on content', () => {
  test('bounces the message it consumed back to its sender, naming the envelope', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-19cfc543', fromPersonId: 'ceo', body: 'CADENCE + X-LOOP' })

    await pane.endTurn(CONTENT_FILTER)

    // CONSUMED — the receipt really happened, and this test would be worthless
    // against a mailbox that could not lose a message.
    expect(of(pane, 'message-accepted').map((entry) => entry.id)).toEqual(['msg-19cfc543'])

    const bounced = bounces(pane)
    expect(bounced).toHaveLength(1)
    expect(bounced[0]?.to).toBe('ceo')
    // The sender's ONE question is "which message?", so the id is in the body.
    expect(bounced[0]?.body).toContain('msg-19cfc543')
    // And the fact that decides what they do next: it was not read.
    expect(bounced[0]?.body).toContain('NOT read')
    // TWO messages leave this pane on a first refusal and they are different
    // facts: the sender is told their message died, the manager is told this
    // person is being refused. The fixture's CEO happens to be both.
    expect(sent(pane)).toHaveLength(2)
    expect(alerts(pane)).toHaveLength(1)

    const durable = of(pane, 'message-bounced')
    expect(durable).toHaveLength(1)
    expect(durable[0]?.recipientPersonId).toBe('ceo')
    expect(durable[0]?.messageIds).toEqual(['msg-19cfc543'])
    expect(durable[0]?.kind).toBe('content_filter')
  })

  test('bounces once per distinct sender when a batch is consumed', async () => {
    const pane = await installedPane()
    await pane.deliver(
      { id: 'msg-a', fromPersonId: 'ceo', body: 'first' },
      { id: 'msg-b', fromPersonId: 'ceo', body: 'second' }
    )

    await pane.endTurn(CONTENT_FILTER)

    // One sender lost two messages: one bounce, both ids. Two bounces to the
    // same person for one refused turn is noise, and a bounce that names only
    // the first leaves the second silently destroyed.
    const bounced = bounces(pane)
    expect(bounced).toHaveLength(1)
    expect(bounced[0]?.body).toContain('msg-a')
    expect(bounced[0]?.body).toContain('msg-b')
    expect(of(pane, 'message-bounced')[0]?.messageIds).toEqual(['msg-a', 'msg-b'])
  })

  test('a turn that COMPLETES bounces nobody', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-read', fromPersonId: 'ceo', body: 'read me' })

    await pane.completeTurn()

    // The message was consumed and ANSWERED. This is the direction that turns a
    // fix into a new bug: a bounce here tells a sender their read message was
    // destroyed.
    expect(sent(pane)).toHaveLength(0)
    expect(of(pane, 'message-bounced')).toHaveLength(0)
  })

  test('a later refusal never bounces a message an earlier turn read', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-read', fromPersonId: 'ceo', body: 'read me' })
    await pane.completeTurn()

    // A second turn, refused, having consumed nothing.
    await pane.endTurn(CONTENT_FILTER)

    expect(bounces(pane)).toHaveLength(0)
    expect(sent(pane).some((entry) => (entry.body ?? '').includes('msg-read'))).toBe(false)
    expect(of(pane, 'message-bounced')).toHaveLength(0)
  })

  test('a TRANSIENT failure is bounced too — the mail is equally destroyed', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-transient', fromPersonId: 'ceo', body: 'hello' })

    await pane.endTurn(TRANSIENT)

    // THE DESTRUCTION IS A PROPERTY OF THE ACCEPTANCE BOUNDARY, not of why the
    // turn died. Measured on a live box: ~2,226 accepted envelopes were
    // followed by a failed turn within 180s, about 22% of ALL mail in the
    // period — and only 67 of those were content refusals. A bounce scoped to
    // the refusal would have left ~97% of the real loss silent through the next
    // outage. Nothing retries a transient failure either (#751/P4), so "a retry
    // could clear it" was never a reason to stay quiet.
    const durable = of(pane, 'message-bounced')
    expect(durable).toHaveLength(1)
    expect(durable[0]?.kind).toBe('provider_error')
    expect(bounces(pane)[0]?.body).toContain('msg-transient')
    // And no manager alert: a transient failure has not reached the reliability
    // limit, so this send is the bounce and nothing else.
    expect(alerts(pane)).toHaveLength(0)
  })

  test('a bounce is never itself bounced, so two failing people cannot ping-pong', async () => {
    const pane = await installedPane()
    // The shape that loops: a bounce is an ordinary delivery to its recipient,
    // so it starts a turn — and a person whose context is what the provider
    // refuses fails EVERY turn, including the one reading a bounce. Each round
    // would mint a new envelope id, so the idempotent-send dedup never collapses
    // the chain. This company has three content-filtered people today.
    await pane.deliver(
      { id: 'content-filter-bounce-abc123', fromPersonId: 'ceo', body: 'your message died' },
      { id: 'provider-health-def456', fromPersonId: 'ceo', body: 'reliability alert' },
      { id: 'msg-real', fromPersonId: 'ceo', body: 'a real request' }
    )

    await pane.endTurn(CONTENT_FILTER)

    // The real message is bounced. The two system envelopes are not, and the
    // bounce that IS sent must not name them either.
    const durable = of(pane, 'message-bounced')
    expect(durable).toHaveLength(1)
    expect(durable[0]?.messageIds).toEqual(['msg-real'])
    expect(bounces(pane)[0]?.body).not.toContain('content-filter-bounce-abc123')
    expect(bounces(pane)[0]?.body).not.toContain('provider-health-def456')
  })

  test('a turn that had begun a tool call warns the sender that work may have run', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-tooled', fromPersonId: 'ceo', body: 'place the order' })

    await pane.endTurnWithToolCall(TRANSIENT)

    // The sender's decision is whether to resend, and the fact that changes the
    // answer is whether anything already ran. A turn that reached a tool call
    // may have executed it before the provider dropped the turn — the same
    // reason nothing on this path replays.
    expect(bounces(pane)[0]?.body).toContain('may have run')
  })
})

describe('an empty provider account (402)', () => {
  test('is permanent: never counted, carded once, and told to the OPERATOR', async () => {
    const pane = await installedPane()

    await pane.endTurn(INSUFFICIENT_CREDITS)
    await pane.endTurn(INSUFFICIENT_CREDITS)
    await pane.endTurn(INSUFFICIENT_CREDITS)

    const failures = of(pane, 'provider-turn-failed')
    expect(failures.map((entry) => entry.kind)).toEqual([
      'insufficient_credits',
      'insufficient_credits',
      'insufficient_credits'
    ])
    // Inert on the counter, like `request_too_large`: no number of retries can
    // clear an empty account.
    expect(failures.map((entry) => entry.consecutiveFailures)).toEqual([0, 0, 0])
    // AND NO MANAGER MAIL. Measured on a live box (2026-08-20): 46 of
    // these in an hour, filed as `provider_error`, escalated to a manager AGENT
    // as "check that Pi's provider access and model health" — a remedy only the
    // account's owner can perform.
    expect(of(pane, 'provider-failure-escalated')).toHaveLength(0)
    expect(sent(pane)).toHaveLength(0)

    const cards = pane.entries().filter((entry) => entry.data.insufficientCredits === true)
    expect(cards).toHaveLength(1)
    const card = cards[0]
    if (isNullish(card)) throw new Error('the empty account drew no card')
    const drawn = flat(pane.render(card))
    expect(drawn).toContain('out of credits')
    expect(drawn).toContain('No agent in this company can do it')
  })

  test('a completed turn re-arms the card, because credits can come back', async () => {
    const pane = await installedPane()

    await pane.endTurn(INSUFFICIENT_CREDITS)
    await pane.completeTurn()
    await pane.endTurn(INSUFFICIENT_CREDITS)

    expect(pane.entries().filter((entry) => entry.data.insufficientCredits === true)).toHaveLength(
      2
    )
  })
})

describe('the content-refusal escalation', () => {
  test('reports the FIRST refusal, once, and never as a provider-health problem', async () => {
    const pane = await installedPane()

    await pane.endTurn(CONTENT_FILTER)

    const escalations = of(pane, 'provider-failure-escalated')
    expect(escalations).toHaveLength(1)
    expect(escalations[0]?.kind).toBe('content_filter')
    expect(escalations[0]?.automaticRetry).toBe(false)
    // The manager is told the truth: the route is healthy. The generic
    // reliability alert says "check that Pi's provider access and model
    // health", which is unactionable when nothing is down.
    const alert = alerts(pane)[0]
    expect(alert?.to).toBe('ceo')
    expect(alert?.body).toContain('healthy')
  })

  test('is not re-armed by a healthy turn, so one filtered topic is reported once', async () => {
    const pane = await installedPane()

    // The measured shape: intel-news failed six times and escalated ZERO times,
    // because any completed turn in between reset the consecutive counter. The
    // opposite failure is a manager mailed on every refusal for ever. Once.
    await pane.endTurn(CONTENT_FILTER)
    await pane.completeTurn()
    await pane.endTurn(CONTENT_FILTER)
    await pane.completeTurn()
    await pane.endTurn(CONTENT_FILTER)

    expect(of(pane, 'provider-failure-escalated')).toHaveLength(1)
  })

  test('a content refusal never moves the reliability counter, in either direction', async () => {
    const pane = await installedPane()

    await pane.endTurn(TRANSIENT)
    await pane.endTurn(CONTENT_FILTER)
    await pane.endTurn(TRANSIENT)
    await pane.endTurn(CONTENT_FILTER)
    await pane.endTurn(TRANSIENT)

    // Inert, exactly like `request_too_large`: it neither advances the count nor
    // resets it, so the three genuine transient failures still reach the limit.
    const failures = of(pane, 'provider-turn-failed')
    expect(failures.map((entry) => entry.consecutiveFailures)).toEqual([1, 1, 2, 2, 3])
    // Two escalations, and they are different things: one content refusal
    // (first occurrence) and one reliability alert (third consecutive).
    const escalations = of(pane, 'provider-failure-escalated')
    expect(escalations.map((entry) => entry.kind)).toEqual(['content_filter', 'provider_error'])
  })

  test('nothing on this path ever replays the turn', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-x', fromPersonId: 'ceo', body: 'x' })

    await pane.endTurn(CONTENT_FILTER)

    // The tombstone's reason is real: a replayed tool-call turn can duplicate an
    // assignment, a message or a trade. Every durable event this path writes
    // must keep saying so.
    for (const entry of pane.events()) {
      if ('automaticRetry' in entry) expect(entry.automaticRetry).toBe(false)
    }
  })
})

describe('the pane card', () => {
  test('names the refusal, the consumed mail and the two real remedies', async () => {
    const pane = await installedPane()
    await pane.deliver({ id: 'msg-carded', fromPersonId: 'ceo', body: 'hi' })

    await pane.endTurn(CONTENT_FILTER)

    const cards = pane.entries().filter((entry) => entry.data.contentFiltered === true)
    expect(cards).toHaveLength(1)
    const card = cards[0]
    if (isNullish(card)) throw new Error('the refusal drew no card')
    const drawn = flat(pane.render(card))
    expect(drawn).toContain('refused this turn on content')
    // The provider is not down, and a reader who thinks it is checks the wrong
    // thing — this is the sentence that stops that.
    expect(drawn).toContain('healthy')
    expect(drawn).toContain('will not be retried')
    // The message is gone, said at the pane and not only to the sender.
    expect(drawn).toContain('NOT read')
    expect(drawn).toContain('move this person to a model')
  })

  test('is drawn once per episode and re-armed by a completed turn', async () => {
    const pane = await installedPane()

    await pane.endTurn(CONTENT_FILTER)
    await pane.endTurn(CONTENT_FILTER)
    const duringOneEpisode = pane.entries().filter((entry) => entry.data.contentFiltered === true)
    expect(duringOneEpisode).toHaveLength(1)

    // A turn that COMPLETED proves the pane left the state the card described,
    // so the next refusal is new information — same rule as the two cards that
    // were already here.
    await pane.completeTurn()
    await pane.endTurn(CONTENT_FILTER)
    expect(pane.entries().filter((entry) => entry.data.contentFiltered === true)).toHaveLength(2)
  })
})
