/**
 * POST /api/founder/abort — stop the Founder's turn in flight.
 *
 * Reports whether there was anything to stop, and never refuses: an operator
 * pressing stop on an idle agent has done nothing wrong.
 */
import { abort } from '@/server/FounderTalk'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function POST(): Promise<Response> {
  return routeResult(async () => abort())
}
