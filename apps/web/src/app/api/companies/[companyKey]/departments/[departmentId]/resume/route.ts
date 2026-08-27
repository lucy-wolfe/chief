/**
 * POST /api/companies/:companyKey/departments/:departmentId/resume — bring a paused
 * unit back.
 *
 * A pass-through, and the exact inverse of `pause`: which people come back and
 * which panes are rebuilt is chiefd's decision, derived from what it recorded
 * when the unit was paused rather than from anything this handler remembers.
 */
import { companyChiefd } from '@/server/CompanyChiefd'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function POST(
  _request: Request,
  context: { params: Promise<{ companyKey: string; departmentId: string }> }
): Promise<Response> {
  const { companyKey, departmentId } = await context.params
  return routeResult(async () => {
    const chiefd = await companyChiefd(companyKey)
    return chiefd.staffing.resumeDepartment(companyKey, departmentId)
  })
}
