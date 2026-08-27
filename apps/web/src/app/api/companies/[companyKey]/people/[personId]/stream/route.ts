/**
 * GET /api/companies/:companyKey/people/:personId/stream — the turn, as it happens.
 *
 * A refusal here is JSON with a status, not an SSE stream carrying an error
 * frame: an `EventSource` that connects successfully and then reports a
 * problem in-band is indistinguishable, to a browser, from a healthy stream
 * with nothing to say.
 */
import { agentFor } from '@/server/HostedRoster'
import { streamPerson } from '@/server/PersonStream'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  const harness = await agentFor(companyKey, personId)
  if (isNullish(harness)) {
    return Response.json(
      {
        error: {
          code: 'person-not-running',
          detail: `"${personId}" is not running in "${companyKey}"`
        }
      },
      { status: 409 }
    )
  }
  return new Response(streamPerson(harness), {
    headers: {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache, no-transform',
      connection: 'keep-alive'
    }
  })
}
