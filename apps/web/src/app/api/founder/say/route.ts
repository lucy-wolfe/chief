/**
 * POST /api/founder/say — one turn with the Founder.
 *
 * The harness is hosted in THIS process, so like the person `say` route and
 * unlike every other route in this app, it is not a pass-through to chiefd.
 * What it does NOT check is whether a roster wants this agent running: Founder
 * is on no roster, because it exists so that one can. The precondition it does
 * have — a model route this box holds a credential for — is refused by name in
 * `FounderAgent`.
 *
 * The body is read INSIDE `routeResult`, for the reason the person route
 * records: a malformed or empty body rejecting outside it produced an unmapped
 * 500 with no envelope, the one failure shape the client cannot read. And it is
 * `safeParse`, not `parse`: a thrown `ZodError` is not a `RouteRefusalError`,
 * so it would have fallen through to `502 upstream-unreachable` — this server
 * blaming chiefd for a body this server rejected.
 */
import { say } from '@/server/FounderTalk'
import { RouteRefusalError } from '@/server/RouteRefusal'
import { routeResult } from '@/server/RouteResult'
import { FounderSayBodySchema } from '@/types/Founder'

export const runtime = 'nodejs'

export async function POST(request: Request): Promise<Response> {
  return routeResult(async () => {
    const parsed = FounderSayBodySchema.safeParse(await request.json())
    if (!parsed.success) {
      throw new RouteRefusalError({
        status: 422,
        code: 'invalid-request',
        message: parsed.error.issues.map((issue) => issue.message).join('; ')
      })
    }
    return say(parsed.data.text)
  })
}
