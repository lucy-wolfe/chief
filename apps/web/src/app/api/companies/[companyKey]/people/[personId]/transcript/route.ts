/**
 * GET /api/companies/:companyKey/people/:personId/transcript — the conversation.
 *
 * Read from the session the person's own harness writes to, so a read straight
 * after a turn sees that turn.
 */
import { transcript } from '@/server/PersonTalk'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  return routeResult(async () => transcript(companyKey, personId))
}
