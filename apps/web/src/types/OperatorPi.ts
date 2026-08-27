/** Public types for the operator's own Pi route.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
import type { Api, Model, Models } from '@earendil-works/pi-ai'

/** The catalog plus the model Pi resolves for a session that names none.
 *
 * `models` is Pi's OWN `ModelRuntime`, which implements pi-ai's `Models`: the
 * harness is handed the same catalog a pane would get, never a projection of
 * one this server built. */
export interface OperatorRoute {
  readonly models: Models
  readonly model: Model<Api>
  readonly provider: string
}
