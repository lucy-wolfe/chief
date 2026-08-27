/**
 * The Founder's talk verbs.
 *
 * # Why these are not `PersonTalk`
 *
 * Every verb in `PersonTalk` begins by asking chiefd's roster whether the
 * person is meant to be running, and refuses `409 person-not-running` when it
 * is not. Founder is on no roster and never will be — it exists so that a
 * roster can — so that question has no answer here. What replaces it is the
 * only precondition Founder actually has: a model route this box holds a
 * credential for. `FounderAgent` refuses that, by name, before a harness is
 * ever built.
 *
 * Everything downstream of "there is a live harness" IS shared: reading the
 * assistant's readable text and classifying a failed turn both come from
 * `server/TurnOutcome`, so the Founder and a CEO report a rejected key the
 * same way.
 *
 * # One mode, not three
 *
 * `PersonTalk` honours `steer` and `followUp` because a company's agent runs
 * long turns an operator needs to correct mid-flight. A Founder turn is a
 * question and an answer, or one tool call; there is nothing to steer into,
 * and offering a queue nobody drains would be a control that does nothing.
 */
import type { AssistantMessage } from '@earendil-works/pi-ai'

import { founderAgent, FounderUnavailableError, hostedFounder } from '@/server/FounderAgent'
import { assistantText, turnFailure } from '@/server/TurnOutcome'
import type { FounderSayOutcome, FounderTranscript } from '@/types/Founder'
import type { TranscriptEntry } from '@/types/PersonTalk'
import { isNullish } from '@/utils/Nullish'

/**
 * A turn the provider did not complete is a FAILURE, not an empty reply.
 *
 * `prompt` resolves with the assistant message whatever happened: a turn that
 * died on `Connection error.` comes back with `stopReason: 'error'` and no
 * content, and returning that as `{"reply":""}` with a 200 would show the
 * operator a Founder that said nothing, with the reason sitting in a
 * transcript they cannot see.
 *
 * The four cases are `TurnOutcome`'s; the sentences are this module's, because
 * the file an operator must open for a Founder is the operator's own registry
 * and not a person's pi-home.
 */
function refuseFailedTurn(message: AssistantMessage): void {
  if (message.stopReason !== 'error') return
  const reason = message.errorMessage ?? 'no reason given'
  const detail = `The provider said: ${reason}`

  switch (turnFailure(reason)) {
    case 'credential-rejected':
      throw new FounderUnavailableError({
        status: 409,
        code: 'provider-credential-rejected',
        message:
          'Founder has a credential its provider refused, so no turn can succeed until it is ' +
          'replaced. This is the key the operator registry holds for the provider their Pi is ' +
          `on — not this server's own environment. ${detail}`
      })
    case 'rate-limited':
      throw new FounderUnavailableError({
        status: 429,
        code: 'provider-rate-limited',
        message: `Founder was rate-limited by its provider. Worth retrying. ${detail}`
      })
    case 'rejected-request':
      throw new FounderUnavailableError({
        status: 502,
        code: 'provider-rejected-request',
        message:
          "Founder's provider rejected the request this server sent. The provider's own words " +
          'are the only diagnosis available here — this server does not know which part of the ' +
          'request it objected to, and naming a cause it has not established would send a ' +
          'reader after the wrong one. Two that have really happened: a malformed tool schema ' +
          '(a provider validates every tool definition and rejects the WHOLE catalog over one ' +
          `of them), and a context window overrun. ${detail}`
      })
    case 'transport':
      throw new FounderUnavailableError({
        status: 502,
        code: 'turn-failed',
        message:
          'Founder could not reach its provider. This server hosts the harness in its OWN ' +
          "process, so the call uses this process's environment and TLS trust store — a host " +
          'behind a TLS-intercepting egress fails every provider call without that CA bundle. ' +
          detail
      })
  }
}

/**
 * Send one turn and wait for the reply.
 *
 * Deliberately awaits, exactly as `PersonTalk.say` does: the caller gets the
 * answer or the harness's own failure, and nothing is queued behind a promise
 * nobody holds.
 *
 * `launched` is read AFTER the turn because that is when the tool has run. It
 * is reported only when THIS turn produced it — a conversation that launched a
 * company three turns ago must not keep announcing it as news.
 */
export async function say(text: string): Promise<FounderSayOutcome> {
  if (text.trim() === '') {
    throw new FounderUnavailableError({
      status: 422,
      code: 'empty-message',
      message: 'a turn needs text'
    })
  }
  const founder = await founderAgent()
  const before = founder.launch.launched
  const message = await founder.harness.prompt(text)
  refuseFailedTurn(message)
  const after = founder.launch.launched
  const reply = assistantText(message.content)
  if (isNullish(after) || after === before) return { reply }
  return { reply, launched: after }
}

/** Stop the turn in flight.
 *
 * Abort on an idle Founder is NOT an error, for the same reason it is not one
 * for a person: an operator pressing stop twice has done nothing wrong, and a
 * refusal there trains them to ignore refusals. */
export async function abort(): Promise<{ aborted: boolean }> {
  const founder = hostedFounder()
  // Nothing started means nothing to stop. Building a harness in order to
  // abort it would start a provider session to end it.
  if (isNullish(founder)) return { aborted: false }
  await founder.harness.abort()
  return { aborted: true }
}

/**
 * The Founder conversation so far.
 *
 * An unstarted Founder is an EMPTY transcript rather than a refusal: the page
 * loads before anybody types, and refusing there would put an error banner in
 * front of every first visit. The route that cannot work — `say` — is where
 * the refusal belongs, and it is the one the operator triggers deliberately.
 */
export async function transcript(): Promise<FounderTranscript> {
  const founder = hostedFounder()
  if (isNullish(founder)) return { entries: [] }
  const branch = await founder.session.getBranch()
  const entries: TranscriptEntry[] = []
  for (const entry of branch) {
    if (entry.type !== 'message') continue
    const role = entry.message.role
    if (role !== 'user' && role !== 'assistant') continue
    entries.push({
      type: 'message',
      id: entry.id,
      // Shaped as a session ENTRY, because that is what the browser's
      // `rowsFromTranscript` reads — the same shape `PersonTalk.transcript`
      // returns, so one reader serves both panes.
      message: { role, content: [{ type: 'text', text: assistantText(entry.message.content) }] }
    })
  }
  const { launched } = founder.launch
  return isNullish(launched) ? { entries } : { entries, launched }
}
