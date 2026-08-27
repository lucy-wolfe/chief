/** Public types for the `ExtensionAPI` → `AgentTool` adapter.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
import type { AgentTool } from '@earendil-works/pi-agent-core'
import type { ExtensionAPI } from '@earendil-works/pi-coding-agent'

import type { HostedLifecycle, LifecycleSubject } from '@/types/ExtensionLifecycle'

/** One Pi extension's install, as this host calls it.
 *
 * Every extension in `packages/piing/extensions` is a function of Pi's
 * `ExtensionAPI` plus, where it has one, an explicit environment. The
 * environment is a PARAMETER rather than an ambient read because this server
 * hosts many people from many companies in one process: a per-process variable
 * would give every agent the last one's company. */
export type ExtensionInstaller = (
  pi: ExtensionAPI,
  environment: Readonly<Record<string, string>>
) => Promise<void> | void

/** The tools one person's extensions registered, and the late binding that
 * connects them to the harness those same tools are used to build. */
export interface ExtensionToolSet {
  /** Every tool the extensions registered, by the name the model sees.
   *
   * The map IS the registration: nothing here filters, renames or adds. */
  readonly tools: ReadonlyMap<string, AgentTool>
  /** Lifecycle hooks the extensions registered that this host will NOT drive,
   * sorted.
   *
   * Reported for the same reason an unbuildable tool id is: a callback
   * accepted and never called is a dead mechanism, and the only thing worse
   * than refusing one is refusing it silently. */
  readonly refusedHandlers: readonly string[]
  /** Bind the person the extensions act on, and start driving their lifecycle.
   *
   * Late by construction, not by choice: `AgentHarness` is built FROM these
   * tools, so the harness cannot exist when they are made. Nothing can call a
   * tool before the harness exists either, so the window is empty. */
  bind(subject: LifecycleSubject, transcriptPath: string): HostedLifecycle
}
