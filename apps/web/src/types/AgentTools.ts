/** Public types for the hosted tool surface.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
import type { AgentTool } from '@earendil-works/pi-agent-core'

import type { HostedLifecycle, LifecycleSubject } from '@/types/ExtensionLifecycle'

/** What a person's tool selection actually resolved to.
 *
 * `unavailable` is part of the RESULT rather than a thrown error: a person
 * granted 60 tools who can be given 56 is still a person worth running, and
 * the caller's job is to say so on the roster rather than to take them off the
 * air. */
export interface ToolSelection {
  /** The tools the harness will be built with. */
  readonly tools: AgentTool[]
  /** Ids chiefd granted that this host cannot provide, sorted. */
  readonly unavailable: string[]
  /** Lifecycle hooks the extensions registered that this host refuses to
   * drive, sorted. Carried for the same reason `unavailable` is. */
  readonly refusedHandlers: readonly string[]
  /** Bind the person the extension tools act on, and start their lifecycle.
   *
   * Late by construction: the harness is built FROM `tools`, so it cannot
   * exist when they are made. Nothing can call a tool before the harness
   * exists either, so the window is empty. */
  bind(subject: LifecycleSubject, transcriptPath: string): HostedLifecycle
}
