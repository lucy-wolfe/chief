/**
 * The talk verbs, against a live in-process harness.
 *
 * # Why each verb refuses rather than starts something
 *
 * Every verb here needs a person who is ALREADY meant to be running, and
 * `agentFor` answers that from chiefd's roster. A verb that launched an agent
 * because somebody typed at it would be this process deciding who runs — the
 * second roster mandate 3 forbids.
 *
 * The refusal is `person-not-running` with a 409, which is the honest answer
 * for a person chiefd does not want up. It is also the exact status apps/api
 * returned for EVERY person, because its own roster read the wrong field. The
 * difference now is that the answer comes from chiefd, so a 409 means the
 * roster says so rather than that the host failed to ask.
 */
import type { AgentHarness } from '@earendil-works/pi-agent-core'
import type { AssistantMessage } from '@earendil-works/pi-ai'

import { hostedSession } from '@/server/AgentHost'
import { agentFor } from '@/server/HostedRoster'
import { RouteRefusalError } from '@/server/RouteRefusal'
import { assistantText, turnFailure } from '@/server/TurnOutcome'
import type { SayMode, SayOutcome, TranscriptEntry } from '@/types/PersonTalk'
import { isNullish } from '@/utils/Nullish'

/** A person chiefd does not want running, or a verb that cannot apply. */
export class PersonTalkError extends RouteRefusalError {}

function notRunning(companyKey: string, personId: string): PersonTalkError {
  return new PersonTalkError({
    status: 409,
    code: 'person-not-running',
    message: `"${personId}" is not running in "${companyKey}"`
  })
}

async function liveAgent(companyKey: string, personId: string): Promise<AgentHarness> {
  const harness = await agentFor(companyKey, personId)
  if (isNullish(harness)) throw notRunning(companyKey, personId)
  return harness
}

/**
 * Send a turn and wait for the reply.
 *
 * Deliberately awaits: the caller gets the assistant's answer or the harness's
 * own failure, and nothing is queued behind a promise nobody holds. A
 * fire-and-forget `say` would report success for a turn that then failed
 * silently, which is how an agent comes to look healthy while answering
 * nothing.
 */
export async function say(
  companyKey: string,
  personId: string,
  text: string,
  mode: SayMode = 'prompt'
): Promise<SayOutcome> {
  if (text.trim() === '') {
    throw new PersonTalkError({
      status: 422,
      code: 'empty-message',
      message: 'a turn needs text'
    })
  }
  const harness = await liveAgent(companyKey, personId)
  // The composer offers three modes and this honoured one. `steer` and
  // `followUp` were sent, ignored, and run as ordinary prompts — so an
  // operator correcting an agent MID-TURN started a second turn instead, and
  // the harness's own queue was never used.
  //
  // Only `prompt` has a reply to wait for. The queued modes hand a message to
  // a turn that is already running and return as soon as it is queued; there
  // is no answer to give yet, and inventing an empty one would make a queued
  // message look like an agent that said nothing.
  if (mode === 'steer') {
    await harness.steer(text)
    return { personId, mode }
  }
  if (mode === 'followUp') {
    await harness.followUp(text)
    return { personId, mode }
  }
  const message = await harness.prompt(text)
  refuseFailedTurn(personId, message)
  return { personId, mode, reply: assistantText(message.content) }
}

/**
 * A turn the provider did not complete is a FAILURE, not an empty reply.
 *
 * `prompt` resolves with the assistant message whatever happened: a turn that
 * died on `Connection error.` comes back with `stopReason: 'error'` and no
 * content, and this used to be returned as `{"reply":""}` with a 200. An
 * operator saw the agent answer nothing, twice, and had no way to tell a
 * broken route from a quiet agent — while the reason sat in the transcript.
 *
 * # Four failures, four answers
 *
 * Reporting all of them as one `502 turn-failed` with the provider's own
 * sentence as the headline is what this now avoids, and it cost real time: a
 * REJECTED CREDENTIAL surfaced as `502 turn-failed: "Connection error."`, which
 * sent a reader to check the network on a host whose egress was answering
 * `200` in 100ms. An operator acts differently on each of these, so each is
 * named:
 *
 *  - a credential the provider REFUSED is a durable condition of the company.
 *    Nothing about retrying changes it and the turn will fail identically
 *    forever until somebody fixes the key, which is why it is a 409 and not a
 *    502 — the same class of answer as "this person is not running";
 *  - a RATE LIMIT is the one genuinely retryable case, and it says so with the
 *    status whose whole meaning is "later";
 *  - a request the provider REJECTED is OUR defect, not the operator's. The
 *    known instance is the tool catalog: a provider validates every tool
 *    definition's schema and rejects the WHOLE catalog over any one of them,
 *    so a single malformed tool disarms every tool the person has (#1011).
 *    Saying so beats sending somebody to look at their key;
 *  - everything else is a transport fault, and keeps the TLS note below,
 *    because it is invisible from the browser and it is the common cause.
 *
 * The provider's own words are kept in every case — as DETAIL, after this
 * server has said what it thinks happened, rather than as the whole answer.
 */
function refuseFailedTurn(personId: string, message: AssistantMessage): void {
  if (message.stopReason !== 'error') return
  const reason = message.errorMessage ?? 'no reason given'
  const detail = `The provider said: ${reason}`

  // WHICH of the four this is, is `TurnOutcome`'s decision — shared with the
  // Founder's own talk verbs, because the provider's words mean the same thing
  // whoever asked. What each failure is CALLED here, and the file it sends an
  // operator to, stays this module's: a person's credential lives in the
  // pi-home chiefd materialized for them, and the Founder's does not.
  const failure = turnFailure(reason)

  if (failure === 'credential-rejected') {
    throw new PersonTalkError({
      status: 409,
      code: 'provider-credential-rejected',
      message:
        `"${personId}" has a credential its provider refused, so no turn can succeed until it ` +
        `is replaced. This is the company's own key for this person, read from their Pi home ` +
        `rather than from this server's environment. ${detail}`
    })
  }

  if (failure === 'rate-limited') {
    throw new PersonTalkError({
      status: 429,
      code: 'provider-rate-limited',
      message: `"${personId}" was rate-limited by its provider. Worth retrying. ${detail}`
    })
  }

  if (failure === 'rejected-request') {
    throw new PersonTalkError({
      status: 502,
      code: 'provider-rejected-request',
      message:
        `"${personId}"'s provider rejected the request this server sent. The provider's own ` +
        `words are the only diagnosis available here — this server does not know which part ` +
        `of the request it objected to, and naming a cause it has not established would send ` +
        `a reader after the wrong one. Two that have really happened: a malformed tool schema ` +
        `(a provider validates every tool definition and rejects the WHOLE catalog over one ` +
        `of them), and a context window overrun. ${detail}`
    })
  }

  throw new PersonTalkError({
    status: 502,
    code: 'turn-failed',
    message:
      `"${personId}" could not reach its provider. This server hosts the harness in its OWN ` +
      `process, so the call uses this process's environment and TLS trust store rather than ` +
      `the one chiefd materialized for the person — a host behind a TLS-intercepting egress ` +
      `fails every provider call without that CA bundle. ${detail}`
  })
}

/**
 * Stop the turn in flight.
 *
 * Abort on an idle agent is NOT an error: an operator pressing stop twice, or
 * stopping a turn that has just finished, has done nothing wrong, and a
 * refusal there would train them to ignore refusals.
 *
 * What is reported is what abort actually DID — the queued messages it threw
 * away. The harness discards anything steered or queued behind the running
 * turn, and an operator who steered three messages and then stopped needs to
 * know those three are gone rather than pending.
 */
export async function abort(
  companyKey: string,
  personId: string
): Promise<{ clearedSteer: number; clearedFollowUp: number }> {
  const harness = await liveAgent(companyKey, personId)
  const result = await harness.abort()
  return {
    clearedSteer: result.clearedSteer.length,
    clearedFollowUp: result.clearedFollowUp.length
  }
}

/**
 * The person's conversation so far.
 *
 * Read from the session the harness itself writes to, so a read straight after
 * a turn sees that turn. A second `Session` over the same file would answer
 * from its own view and show a conversation one turn behind.
 */
export async function transcript(
  companyKey: string,
  personId: string
): Promise<{ personId: string; entries: TranscriptEntry[] }> {
  await liveAgent(companyKey, personId)
  const session = hostedSession(companyKey, personId)
  if (isNullish(session)) throw notRunning(companyKey, personId)
  const branch = await session.getBranch()
  const entries: TranscriptEntry[] = []
  for (const entry of branch) {
    if (entry.type !== 'message') continue
    const role = entry.message.role
    if (role !== 'user' && role !== 'assistant') continue
    const text = assistantText(entry.message.content)
    // Shaped as a session ENTRY — `{type: 'message', message: {role, content}}`
    // — because that is what the browser's `rowsFromTranscript` reads. It used
    // to be `{id, role, text}`, a flat shape of this module's own invention, so
    // the fold matched no case and skipped every entry: the pane rendered an
    // empty conversation for an agent with a full transcript. The same defect
    // as the person stream's private frame names, one read further along.
    //
    // The CONTENT is still reduced to readable text: thinking blocks and tool
    // calls are dropped here exactly as they are on the stream.
    entries.push({
      type: 'message',
      id: entry.id,
      message: { role, content: [{ type: 'text', text }] }
    })
  }
  return { personId, entries }
}
