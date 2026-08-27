/**
 * POST /api/companies/:companyKey/people/:personId/transfer — move somebody's HOME
 * department.
 *
 * A pass-through. A transfer is a durable structural decision with its own
 * refusals in chiefd (an unknown destination, a head who cannot leave the unit
 * they head); re-checking any of that here would be a second opinion that can
 * disagree with the one that actually writes.
 */
import { companyChiefd } from '@/server/CompanyChiefd'
import { routeResult } from '@/server/RouteResult'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

interface TransferBody {
  destinationId?: string
  reason?: string
  actor?: string
}

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  const body: TransferBody = await request.json()
  return routeResult(async () => {
    const chiefd = await companyChiefd(companyKey)
    return chiefd.staffing.transferPerson(companyKey, personId, body.destinationId ?? '', {
      ...(isNullish(body.reason) ? {} : { reason: body.reason }),
      ...(isNullish(body.actor) ? {} : { actor: body.actor })
    })
  })
}
