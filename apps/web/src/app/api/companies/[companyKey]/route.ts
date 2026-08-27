/**
 * GET /api/companies/:companyKey — one company's summary.
 *
 * A 404 for a companyKey beacond has never heard of, which is a different answer
 * from a company that exists and is stopped: the first means the operator
 * typed something wrong, the second means they need to start it.
 */
import { CompanyUnavailableError } from '@/server/CompanyChiefd'
import { companySummary } from '@/server/CompanyDirectory'
import { routeResult } from '@/server/RouteResult'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  return routeResult(async () => {
    const summary = await companySummary(companyKey)
    if (isNullish(summary)) {
      throw new CompanyUnavailableError({
        status: 404,
        code: 'unknown-company',
        message: `no company registered as "${companyKey}"`
      })
    }
    return summary
  })
}
