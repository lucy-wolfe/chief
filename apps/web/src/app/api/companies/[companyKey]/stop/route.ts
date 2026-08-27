/**
 * POST /api/companies/:companyKey/stop — stop a company's runtime.
 *
 * One request, one answer: unlike boot there is no step a caller could act on
 * differently, so this is ordinary JSON rather than a stream.
 *
 * Durable state is untouched. Stopping a company is not deleting it — the
 * company keeps its manifest, its people, their transcripts and their mail,
 * and `POST …/boot` brings the same company back.
 */
import { stopCompany } from '@/server/CompanyLifecycle'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function POST(
  _request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  return routeResult(async () => stopCompany(companyKey))
}
