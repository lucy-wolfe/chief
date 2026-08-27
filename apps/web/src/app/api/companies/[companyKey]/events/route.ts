/**
 * GET /api/companies/:companyKey/events — the company's change feed.
 *
 * The reactive spine. Every read in the page is issued because this stream
 * said something changed; without it the UI shows whatever it fetched once and
 * never learns anything again. It is the route the browser has been dialling
 * since before it existed.
 *
 * # Adapted, not piped
 *
 * chiefd's frames are not the frames the page matches — see
 * `server/CompanyFeed.ts`. A byte pipe would connect, heartbeat, look
 * perfectly healthy and deliver nothing the browser can act on.
 *
 * # The query is the caller's, the identity is not
 *
 * `stores` and `after` come from the page, because which stores a pane needs
 * and where it resumed from are the page's business. The company KEY is built
 * here from beacond's answer: the browser knows only a bare companyKey, and
 * accepting a pre-built key would let a caller address a company under another
 * data root.
 */
import { companyChiefdEndpoint, CompanyUnavailableError } from '@/server/CompanyChiefd'
import { companyFeed } from '@/server/CompanyFeed'
import { isNullish } from '@/utils/Nullish'

export const runtime = 'nodejs'

export async function GET(
  request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  const asked = new URL(request.url).searchParams
  try {
    const chiefd = await companyChiefdEndpoint(companyKey)
    const stores = (asked.get('stores') ?? '').split(',').filter((name) => name !== '')
    const after = asked.get('after')
    const body = await companyFeed({
      endpoint: chiefd,
      companyKey,
      stores,
      // Absent and empty are different: an absent `after` is a fresh subscribe,
      // and chiefd never fires `reorg` for one. Forwarding an empty string
      // would ask it to resume from sequence "", a different question.
      ...(isNullish(after) || after === '' ? {} : { after }),
      signal: request.signal
    })
    return new Response(body, {
      headers: {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache, no-transform',
        connection: 'keep-alive'
      }
    })
  } catch (error) {
    if (error instanceof CompanyUnavailableError) {
      return Response.json(
        { error: { code: error.code, detail: error.message } },
        { status: error.status }
      )
    }
    return Response.json(
      {
        error: {
          code: 'upstream-unreachable',
          detail: error instanceof Error ? error.message : String(error)
        }
      },
      { status: 502 }
    )
  }
}
