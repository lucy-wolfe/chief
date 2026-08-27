/**
 * A failure this server has already judged, with the status it deserves.
 *
 * # Why the base class lives alone in its own module
 *
 * `routeResult` has to recognise these, and it used to do that by importing
 * the three concrete classes from the modules that raise them —
 * `CompanyChiefd`, `PersonTalk`, `Staffing`. Those modules are the agent
 * runtime: `PersonTalk` pulls in `AgentHost` and `HostedRoster`, which pull in
 * `@earendil-works/pi-agent-core` and `@earendil-works/pi-ai`. So every route
 * that mapped an error — including `GET /api/companies`, which lists companies
 * out of beacond and touches no provider — loaded the whole harness and every
 * provider module behind it.
 *
 * That is not merely wasteful. `pi-ai` raised unhandled rejections at import
 * time under the bundler (see `next.config.ts`), and Node exits on those, so a
 * company LISTING could take the server down. A listing route has no business
 * knowing that a model provider exists.
 *
 * One base class in a module that imports nothing gives `routeResult` a single
 * `instanceof` and leaves the runtime out of every route that does not host an
 * agent.
 */

/** A failure with a status and a machine-readable code the browser can act on.
 *
 * Subclassed rather than thrown directly: the code and status are the contract,
 * and each subclass names a distinct authority for them — chiefd's refusal,
 * the roster's, or this server's own validation. */
export class RouteRefusalError extends Error {
  /** The HTTP status this failure deserves. */
  readonly status: number
  /** The stable code the browser branches on. */
  readonly code: string

  constructor(options: { status: number; code: string; message: string }) {
    super(options.message)
    this.name = new.target.name
    this.status = options.status
    this.code = options.code
  }
}
