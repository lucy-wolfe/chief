/**
 * POST /api/companies/:companyKey/departments/:departmentId/reparent — move a unit
 * under a new parent.
 *
 * A pass-through. Reparenting is a structural decision with its own refusals
 * in chiefd (a cycle, an unknown parent, the root being moved), and the whole
 * subtree moves with the unit. None of that is re-derived here.
 */
import { companyChiefd } from '@/server/CompanyChiefd'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

interface ReparentBody {
  newParentId?: string
}

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string; departmentId: string }> }
): Promise<Response> {
  const { companyKey, departmentId } = await context.params
  const body: ReparentBody = await request.json()
  return routeResult(async () => {
    const chiefd = await companyChiefd(companyKey)
    return chiefd.staffing.reparentDepartment(companyKey, departmentId, body.newParentId ?? '')
  })
}
