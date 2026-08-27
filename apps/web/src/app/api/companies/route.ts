/**
 * GET /api/companies — every company, and whether it is actually up.
 * POST /api/companies — create one.
 *
 * The landing page's whole content. Existence comes from beacond's registry;
 * liveness comes from probing each daemon, because a registry row keeps its
 * url after a process dies without deregistering.
 *
 * POST answers `text/event-stream` for the same reason boot does: genesis is a
 * sequence of steps that takes minutes — the beacond claim, the daemon coming
 * up, the manifest transaction, the CEO — and `BootPhaseConsole` renders each
 * as it happens. The whole request body is `{name, purpose}`; the slug is
 * chiefd's answer and arrives on the terminal frame.
 */
import { companyDirectory } from '@/server/CompanyDirectory'
import { createCompany } from '@/server/CompanyLifecycle'
import { RouteRefusalError } from '@/server/RouteRefusal'
import { routeResult } from '@/server/RouteResult'
import { CompanyCreateBodySchema } from '@/types/Companies'

export const runtime = 'nodejs'

export async function GET(): Promise<Response> {
  return routeResult(async () => companyDirectory())
}

export async function POST(request: Request): Promise<Response> {
  // Validated BEFORE the stream opens. This is the one failure that can be
  // reported as a status code — nothing has been narrated yet — and a
  // malformed body is the caller's mistake rather than a lifecycle refusal, so
  // reporting it as a `failed` frame would file it under the wrong heading.
  const parsed = CompanyCreateBodySchema.safeParse(await request.json())
  if (!parsed.success) {
    return routeResult(async () => {
      throw new RouteRefusalError({
        status: 422,
        code: 'invalid-company',
        message: parsed.error.issues.map((issue) => issue.message).join('; ')
      })
    })
  }
  return new Response(createCompany(parsed.data), {
    headers: {
      'content-type': 'text/event-stream',
      // A proxy that buffers this stream turns a live narration into one
      // silent wait followed by every phase at once, which is the same thing
      // an operator sees when a create has hung.
      'cache-control': 'no-cache, no-transform',
      connection: 'keep-alive'
    }
  })
}
