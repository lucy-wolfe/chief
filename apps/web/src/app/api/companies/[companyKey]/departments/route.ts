/**
 * POST /api/companies/:companyKey/departments — create a department.
 *
 * The verb an operator asked a CEO for and did not get. A pass-through with
 * one rule of its own: the head is an EXISTING person, so a failure cannot
 * leave an operator wondering whether the unit was created and only the head
 * failed.
 */
import { routeResult } from '@/server/RouteResult'
import { createDepartment } from '@/server/Staffing'
import type { NewDepartmentRequest } from '@/types/Staffing'

export const runtime = 'nodejs'

export async function POST(
  request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  const body: NewDepartmentRequest = await request.json()
  return routeResult(async () => createDepartment(companyKey, body))
}
