/**
 * GET /api/companies/:companyKey/people — who is running, and who is not.
 *
 * The one route that reports the HOST's state rather than chiefd's. The tree
 * route already answers "who works here"; this answers "who is up right now,
 * and for anybody who is not, why".
 *
 * The `why` is the reason this exists. A person chiefd wants running who has
 * no usable route is dormant, and a UI with no word for that shows an agent
 * that looks perfectly healthy and never answers — the failure this program
 * keeps reproducing. Converging on read is deliberate: the answer is only
 * true if it is made true, and reporting a stale registry would describe a
 * roster nobody is actually running.
 */
import { convergeRoster } from '@/server/HostedRoster'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  return routeResult(async () => convergeRoster(companyKey))
}
