/**
 * POST /api/companies/:companyKey/people/hire — hire somebody.
 *
 * The route the CEO's own tool takes, made available to the page. Everything
 * about the model route is decided in `server/Staffing.ts` from chiefd's own
 * answer, never from the form.
 */
import { routeResult } from '@/server/RouteResult'
import { hire } from '@/server/Staffing'
import type { HireRequest } from '@/types/Staffing'

export const runtime = 'nodejs'

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  const body: HireRequest = await request.json()
  return routeResult(async () => hire(companyKey, body))
}
