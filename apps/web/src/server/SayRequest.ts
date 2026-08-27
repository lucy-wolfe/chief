/**
 * The one reading of a `say` body.
 *
 * # The defect this exists to make impossible
 *
 * The browser sent `{"message": "...", "mode": "prompt"}` and this route read
 * `body.text`. Both halves were tested — the client against its own schema,
 * the route against its own handler — and both were green, so the mismatch was
 * invisible until an operator typed into the composer and every single message
 * came back `422 empty-message`. That is the same shape as the missing `/api`
 * prefix: two correct halves and nothing checking the seam.
 *
 * The wire word is `text`, because that is what it is called everywhere it
 * ends up — `PersonTalk.say(…, text)`, `AgentHarness.prompt(text)`, and the
 * transcript entry. The client was the only place that called it something
 * else, and one word for one thing is the only durable fix.
 */
import { PersonTalkError } from '@/server/PersonTalk'
import type { SayMode } from '@/types/PersonTalk'
import { isNullish } from '@/utils/Nullish'

const MODES: readonly SayMode[] = ['prompt', 'steer', 'followUp']

function invalid(detail: string): PersonTalkError {
  return new PersonTalkError({ status: 422, code: 'invalid-request', message: detail })
}

/** A `say` body as this server's own request, or a refusal naming the field. */
export function sayRequest(body: unknown): { text: string; mode: SayMode } {
  if (typeof body !== 'object' || isNullish(body) || Array.isArray(body)) {
    throw invalid('a turn needs a JSON object body')
  }
  const record: Record<string, unknown> = Object.fromEntries(Object.entries(body))
  const { text } = record
  if (typeof text !== 'string') throw invalid('"text" is required and must be a string')
  const { mode } = record
  // Absent means `prompt`: the composer's default, and the only mode a caller
  // that has never heard of the queues would want.
  if (isNullish(mode)) return { text, mode: 'prompt' }
  const found = MODES.find((known) => known === mode)
  // A mode this server does not know is REFUSED rather than defaulted. A typo
  // silently becoming `prompt` would start a new turn where the operator meant
  // to correct the running one.
  if (isNullish(found)) throw invalid(`"mode" must be one of: ${MODES.join(', ')}`)
  return { text, mode: found }
}
