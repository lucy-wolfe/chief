/**
 * POST /api/companies/:companyKey/people/:personId/abort — stop the turn in flight.
 *
 * No body: there is exactly one thing to stop. The response reports the queued
 * messages abort threw away, because an operator who steered three messages
 * and then stopped needs to know those three are gone.
 */
import { abort } from '@/server/PersonTalk'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function POST(
  _request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  return routeResult(async () => abort(companyKey, personId))
}
