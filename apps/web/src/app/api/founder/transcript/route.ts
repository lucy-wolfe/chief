/**
 * GET /api/founder/transcript — the Founder conversation so far.
 *
 * Answers an EMPTY transcript for a Founder nobody has started, rather than a
 * refusal: this is the first request the page makes, and a box with no route
 * configured would otherwise greet every visitor with an error before they had
 * asked for anything. The refusal belongs on `say`, which is the request an
 * operator makes deliberately.
 */
import { transcript } from '@/server/FounderTalk'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function GET(): Promise<Response> {
  return routeResult(async () => transcript())
}
