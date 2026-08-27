/**
 * POST /api/companies/:companyKey/people/:personId/offboard — end somebody's
 * employment.
 *
 * A pass-through. Offboarding cascades (pane teardown,
 * head replacement), and every one of those is chiefd's; this handler forwards
 * a decision rather than making one.
 */
import { companyChiefd } from '@/server/CompanyChiefd'
import { routeResult } from '@/server/RouteResult'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

interface OffboardBody {
  reason?: string
  actor?: string
}

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  const body: OffboardBody = await request.json().catch(() => ({}))
  return routeResult(async () => {
    const chiefd = await companyChiefd(companyKey)
    return chiefd.staffing.offboardPerson(companyKey, personId, {
      ...(isNullish(body.reason) ? {} : { reason: body.reason }),
      ...(isNullish(body.actor) ? {} : { actor: body.actor })
    })
  })
}
