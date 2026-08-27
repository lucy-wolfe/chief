/**
 * Public types for the tool-registration harness.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */

/** One tool exactly as `pi.registerTool` received it. */
export interface CapturedTool {
  name: string
  label?: string
  description?: string
  parameters?: unknown
  [key: string]: unknown
}

/** One message renderer exactly as `pi.registerMessageRenderer` received it.
 *
 * Loosely typed on purpose: the extension registers these with `any` message
 * and theme parameters, and a test's whole job here is to hand it a shape and
 * read what comes back. */
export type CapturedRenderer = (
  message: { details?: unknown; content?: unknown },
  options: { expanded?: boolean },
  theme: unknown
) => unknown

export interface ToolRegistrationCapture {
  readonly tools: readonly CapturedTool[]
  /** Every message renderer the install registered, keyed by custom type. */
  readonly renderers: ReadonlyMap<string, CapturedRenderer>
  /** Every ENTRY renderer the install registered, keyed by custom type. */
  readonly entryRenderers: ReadonlyMap<string, CapturedRenderer>
  /** Every chiefd route the install touched, for non-vacuity. */
  readonly chiefdPaths: readonly string[]
  stop(): Promise<void>
}

/** One canned chiefd answer, exactly as the wire would carry it. */
export interface StubbedRoute {
  readonly status: number
  readonly body: string
}

export interface ToolRegistrationOptions {
  /** Extra people merged into the fixture manifest, keyed by person id. */
  readonly people?: Readonly<Record<string, unknown>>
  /** Whose pane this install is. Defaults to the fixture CEO; naming one of
   *  `people` is how a suite asks what a WORKER's pane carries, which is the
   *  question `installSubtreeTools` exists to answer. */
  readonly personId?: string
  /** Canned answers for named chiefd routes, keyed by path. Anything not
   *  named here keeps the harness's permissive `{found:false}` default.
   *
   *  An ARRAY answers successive calls to that path in order, with the last
   *  entry repeating once exhausted. A FUNCTION receives the request body and
   *  the zero-based call index and builds the answer — which is what a managed
   *  fanout needs, since it queues one request per target and each returned
   *  row is checked against its own target, so an answer that does not echo
   *  the person it was asked about refuses for a fixture reason. */
  readonly routes?: Readonly<
    Record<
      string,
      StubbedRoute | readonly StubbedRoute[] | ((body: string, call: number) => StubbedRoute)
    >
  >
}
