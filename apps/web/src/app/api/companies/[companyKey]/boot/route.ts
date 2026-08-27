/**
 * POST /api/companies/:companyKey/boot — start a company that exists but is down.
 *
 * Answers `text/event-stream`, because booting a company is a sequence of
 * steps that take minutes and the operator needs to watch them: the daemon
 * coming up, the CEO being prepared, the CEO starting. `BootPhaseConsole`
 * renders exactly those frames.
 *
 * The phases are chiefd's. This route starts nothing itself — see
 * `server/CompanyLifecycle.ts` for why that is a pass-through rather than a
 * second lifecycle.
 */
import { bootCompany } from '@/server/CompanyLifecycle'

export const runtime = 'nodejs'

export async function POST(
  _request: Request,
  context: { params: Promise<{ companyKey: string }> }
): Promise<Response> {
  const { companyKey } = await context.params
  return new Response(bootCompany(companyKey), {
    headers: {
      'content-type': 'text/event-stream',
      // A proxy that buffers this stream turns a live narration into one
      // silent wait followed by every phase at once, which is the same thing
      // an operator sees when a boot has hung.
      'cache-control': 'no-cache, no-transform',
      connection: 'keep-alive'
    }
  })
}
