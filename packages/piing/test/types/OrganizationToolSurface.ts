/**
 * Public types for the organization tool-contract surface.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */
import type { InstallOrganizationIntercomOptions } from '@test-assets/organization-intercom'

/** One tool exactly as `pi.registerTool` received it. */
export interface RegisteredToolDefinition {
  name: string
  label?: string
  description?: string
  parameters?: unknown
  execute: (...args: unknown[]) => unknown
  [key: string]: unknown
}

/** What a tool's `execute` resolved to, unwrapped from Pi's content envelope. */
export interface ToolCallOutcome {
  /**
   * Read from `details.ok`, never inferred from the absence of a throw: every
   * defect this fixture exists to catch RESOLVES, with `ok: false`. That is
   * exactly what made them invisible to harnesses that only caught exceptions.
   */
  ok: boolean
  message: string
  details: Record<string, unknown>
}

export interface OrganizationToolSurface {
  readonly tools: Map<string, RegisteredToolDefinition>
  /** Messages the extension pushed at the agent, in order. */
  readonly messages: string[]
  call(name: string, params: Record<string, unknown>): Promise<ToolCallOutcome>
  /**
   * Deliver Pi's `session_start` with a stand-in `ExtensionContext`.
   *
   * The fake `pi`'s `on()` used to discard every handler, so `session_start`
   * never fired and the extension's `latestExtensionContext` stayed undefined.
   * That made a whole family of session-bound behaviour untestable here, which
   * is how a port ships on weaker evidence.
   */
  startSession(options?: { sessionId?: string; entries?: readonly unknown[] }): Promise<void>
  /**
   * Deliver Pi's `agent_settled` with the same stand-in `ExtensionContext`.
   *
   * The settled boundary is not a nicety: it is the ONLY entry point to the
   * session-maintenance claim ladder (`start`/`recover`/`finish`) and to
   * `queueAutomaticParkCompaction`, which is the single reader of
   * `org activity status`. Neither has a tool, so without this the only
   * evidence available for them is a route POST — and #751/P4 has already
   * shipped three packets whose route returned 200 and whose product was
   * broken.
   *
   * `contextUsagePercent` drives `getContextUsage()`, which gates the
   * automatic pre-park compaction at >50%.
   */
  settle(options?: { contextUsagePercent?: number }): Promise<void>
}

export interface OrganizationToolSurfaceOptions {
  /**
   * The company daemon's own address, PUBLISHED into `organizationDir` as the
   * rendezvous rather than handed to the extension.
   *
   * The extension reads `<dir>/.chief/run/daemon.json` for itself — the fixture
   * stands in for the daemon that writes it, not for a registry that answers
   * questions about it. Passing the URL straight through would test a mechanism
   * the product does not have.
   */
  chiefdUrl: string
  /** The company's display slug. */
  organization: string
  /** THE COMPANY DIRECTORY — what a live pane's `ORG_LAUNCHER_ORG_DIR` names,
   * and where its `.chief/run/daemon.json` is published. */
  organizationDir: string
  /** The acting person. Manager tools only register for an executive or a head. */
  personId: string
  /** The repo root; the extension derives its CLI path from it. */
  launcherRoot: string
  /**
   * The daemon's private tmux socket, and the company's tmux session name.
   *
   * BOTH OPTIONAL, and omitting them is the web-hosted shape rather than an
   * incomplete pane: chiefd's API-host profile publishes neither (AC6), and a
   * person hosted by `apps/web` therefore arrives with no tmux identity at all.
   */
  tmuxSocket?: string
  /** See {@link OrganizationToolSurfaceOptions.tmuxSocket}. */
  tmuxSession?: string
  /** Escape hatch for a test that needs a different seam. Merged last. */
  install?: Partial<InstallOrganizationIntercomOptions>
}
