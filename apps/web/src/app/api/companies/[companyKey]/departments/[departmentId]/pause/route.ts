/**
 * POST /api/companies/:companyKey/departments/:departmentId/pause — stop a unit
 * without deleting its history.
 *
 * A pass-through. Pausing cascades to the unit's people and their panes, and
 * that cascade is chiefd's.
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
    return chiefd.staffing.pauseDepartment(companyKey, departmentId)
  })
}
