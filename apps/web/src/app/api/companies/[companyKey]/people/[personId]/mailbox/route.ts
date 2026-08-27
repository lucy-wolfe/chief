/**
 * GET /api/companies/:companyKey/people/:personId/mailbox — this person's mail.
 *
 * A pass-through to chiefd's own per-person mailbox read. The mailbox is a
 * durable row store, not a live-agent fact, so it is answered for a person
 * whether or not this host is running them — a dormant person's unread mail is
 * exactly what an operator needs to see before deciding to bring them up.
 */
import { personMailbox } from '@/server/Mailbox'
import { routeResult } from '@/server/RouteResult'

export const runtime = 'nodejs'

export async function GET(
  _request: Request,
  context: { params: Promise<{ companyKey: string; personId: string }> }
): Promise<Response> {
  const { companyKey, personId } = await context.params
  return routeResult(async () => personMailbox(companyKey, personId))
}
