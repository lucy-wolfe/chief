/**
 * POST /api/companies/:companyKey/people/:personId/say — send one turn.
 *
 * The harness is hosted in THIS process, so unlike every sibling route this is
 * not a pass-through to chiefd. What stays chiefd's is the only decision that
 * matters here: whether this person is meant to be running at all.
 *
 * # The body is read inside `routeResult`
 *
 * `await request.json()` used to run OUTSIDE it, so a malformed or empty body
 * rejected before the try/catch and the browser got an unmapped 500 with no
 * envelope — the one failure shape the client cannot read.
 */
import { say } from '@/server/PersonTalk'
import { routeResult } from '@/server/RouteResult'
import { sayRequest } from '@/server/SayRequest'

export const runtime = 'nodejs'

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  return routeResult(async () => {
    const { text, mode } = sayRequest(await request.json())
    return say(companyKey, personId, text, mode)
  })
}
