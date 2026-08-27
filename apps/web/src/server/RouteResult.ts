/**
 * The one place a route handler turns an outcome or a failure into a Response.
 *
 * Every handler under `app/api/companies/` has the same two failure shapes —
 * a company that cannot be reached, and everything else — and repeating that
 * `try`/`catch` per file is how the shapes drift apart until two routes report
 * the same problem with different codes.
 */
import { OrgRowRefusalError } from '@chief/chiefing'
import { NextResponse } from 'next/server'

import { RouteRefusalError } from '@/server/RouteRefusal'

/** Run `work`, mapping its failures to the shared error envelope. */
export async function routeResult(work: () => Promise<unknown>): Promise<Response> {
  try {
    return NextResponse.json(await work())
  } catch (error) {
    // A failure this server has already judged carries its own status and
    // code — a 404 for a company key nobody registered, a 409 for a company that is
    // registered but not running or a person chiefd does not want up, a 422
    // for a request this server will not forward. None of them is an upstream
    // fault, and reporting one as a 502 tells the operator the daemon is
    // broken when it is answering correctly.
    //
    // ONE `instanceof`, against a base class in a module that imports nothing.
    // Naming the three concrete classes here meant importing the modules that
    // raise them, and those modules ARE the agent runtime — so listing
    // companies loaded every provider in Pi. See `RouteRefusal`.
    if (error instanceof RouteRefusalError) {
      return NextResponse.json(
        { error: { code: error.code, detail: error.message } },
        { status: error.status }
      )
    }
    // chiefd's OWN refusal, forwarded with chiefd's code and chiefd's status.
    // A reparent that would detach a subtree, a transfer to a department that
    // does not exist, a head who cannot leave: chiefd answers 422 with a code
    // naming exactly that, and this layer used to flatten all of it to `502
    // upstream-unreachable`. The operator was told the daemon was unreachable
    // by a daemon that had just explained, precisely, why it said no.
    if (error instanceof OrgRowRefusalError) {
      return NextResponse.json(
        { error: { code: error.code, detail: error.message } },
        { status: error.status }
      )
    }
    // Everything else IS upstream: chiefd refused, or could not be reached.
    // 502 rather than 500 because this server did its job — it resolved a
    // company and forwarded a request that the daemon then failed.
    return NextResponse.json(
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
