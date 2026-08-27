/** The wire vocabulary for Founder Mode.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`, and defined ONCE
 * as schemas the types are inferred from. Both halves read this file: the
 * server returns these shapes and the browser parses them. The alternative —
 * an interface on one side and a schema on the other — is how `say` came to
 * send `message` while the route read `text`, with both suites green.
 */
import { z } from 'zod'

import type { ChiefApiErrorShape } from '@/types/ApiErrors'
import type { ConversationRow } from '@/types/Conversation'

/** `POST /api/founder/say`'s body.
 *
 * The wire word is `text`, the same word the person `say` route uses and the
 * same word it is called everywhere it ends up — `harness.prompt(text)` and
 * the transcript entry. */
export const FounderSayBodySchema = z.object({
  text: z.string().trim().min(1, 'a turn needs text')
})

/** The company a Founder turn actually created, when one was.
 *
 * Reported as DATA alongside the reply rather than left in the prose: the
 * model announces a launch in English, and a UI parsing a slug out of English
 * would sooner or later link somewhere that does not exist. */
export const FounderLaunchedSchema = z.object({
  /** What ADDRESSES the created company. `/c/[companyKey]` resolves by this. */
  key: z.string(),
  /** What a person READS. Never used to build a URL — two directories may
   * hold companies with the same slug. */
  slug: z.string(),
  name: z.string()
})
export type FounderLaunched = z.infer<typeof FounderLaunchedSchema>

/** What one Founder turn did.
 *
 * `launched` is `nullish` rather than `optional` per
 * `lucy/no-optional-nullable`: a turn that created nothing and a turn whose
 * launch was null must not be two different answers. */
export const FounderSayResponseSchema = z.object({
  reply: z.string(),
  launched: FounderLaunchedSchema.nullish()
})
export type FounderSayOutcome = z.infer<typeof FounderSayResponseSchema>

/** The Founder conversation so far.
 *
 * `entries` is `z.unknown()` for the same reason the person transcript's is:
 * the browser's `rowsFromTranscript` is the one reader of a session entry, and
 * a second description of that shape here would be a second thing to keep in
 * step with Pi. */
export const FounderTranscriptResponseSchema = z.object({
  entries: z.array(z.unknown()),
  launched: FounderLaunchedSchema.nullish()
})
export type FounderTranscript = z.infer<typeof FounderTranscriptResponseSchema>

/** What abort found. `false` means there was no turn in flight, which is not
 * an error — an operator pressing stop twice has done nothing wrong. */
export const FounderAbortResponseSchema = z.object({
  aborted: z.boolean()
})
export type FounderAbortOutcome = z.infer<typeof FounderAbortResponseSchema>

/** Everything the Founder page renders, from `useFounderConversation`. */
export interface FounderConversationResult {
  readonly rows: readonly ConversationRow[]
  /** A turn is in flight. The composer waits on it: a Founder turn IS the
   * request, so there is nothing to queue behind it. */
  readonly pending: boolean
  readonly hydrating: boolean
  readonly error: ChiefApiErrorShape | undefined
  /** The company this conversation created, once it has. */
  readonly launched: FounderLaunched | undefined
  readonly send: (text: string) => Promise<void>
  readonly abort: () => Promise<void>
}
