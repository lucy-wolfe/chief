/**
 * The agent-facing seam: install the REAL `organization-intercom` extension
 * against a REAL company and hand back the tools an agent actually calls.
 *
 * # Why this exists (#751/P4)
 *
 * Every other test in this repo drives either a pure unit or an HTTP route.
 * A route proof is strictly weaker than a tool proof, because the tool does
 * MORE than call the route: it calls the route, then reconciles the runtime,
 * then classifies the outcome. Three packets in one day proved
 * `POST /v1/org/department/create` returns 200 and shipped a broken product:
 *
 *  - `org_launch_department` returned 200 from the route and then died on the
 *    reconcile step with `chiefd: unknown command 'org'`;
 *  - the staffing-lifecycle verbs, `org_offboard` among them, committed the
 *    whole mutation and then threw `returned an invalid outcome`, because
 *    `/v1/org/staffing/lifecycle` answered `{"status":"applied"}` with no
 *    `applied` key.
 *
 * Both failures are AFTER the 200. A route test cannot see either.
 *
 * # What is real here, and what is not
 *
 * REAL: the extension module, `pi.registerTool`'s definitions, each tool's
 * registered `execute`, the chiefd daemon, the company, the tmux host, and —
 * load-bearing — the DEFAULT `LauncherRunner`. Injecting a scripted runner is
 * precisely what let the reconcile defect through: the fake answered where
 * the real one spawns a CLI that no longer exists.
 *
 * FAKE: `pi` itself (a four-method recorder), which is a live editor session
 * object this process does not have.
 */
import { randomUUID } from 'node:crypto'

import { publishDaemonRendezvous } from '@test/support/CompanyRendezvous'
import { isNullish } from '@test/support/Nullish'
import type {
  OrganizationToolSurface,
  OrganizationToolSurfaceOptions,
  RegisteredToolDefinition,
  ToolCallOutcome
} from '@test/types/OrganizationToolSurface'
import { installOrganizationIntercom } from '@test-assets/organization-intercom'

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

function messageText(value: unknown): string {
  if (typeof value === 'string') return value
  if (isRecord(value) && typeof value.text === 'string') return value.text
  return ''
}

/** Unwrap Pi's `{ content: [{type:'text',text}], details:{ok,...} }` envelope. */
function unwrap(raw: unknown): ToolCallOutcome {
  const envelope = isRecord(raw) ? raw : {}
  const content = Array.isArray(envelope.content) ? envelope.content : []
  const message = content.map(messageText).filter(Boolean).join('\n')
  const details = isRecord(envelope.details) ? envelope.details : {}
  return { ok: details.ok === true, message, details }
}

/**
 * Install the extension for one person against one live company.
 *
 * The injected `environment` below is the ONLY thing that decides which daemon
 * this install talks to. Nothing here touches `process.env`, and nothing in
 * the extension reads it for an address any more: `chiefdEndpoint(context)`
 * resolves from the context this install was built from, so two installs in
 * one process can name two different companies at the same time. Setting a
 * process-wide variable around a call would have been a race whose failure
 * mode is silent — a wrong daemon answers.
 */
export async function installOrganizationToolSurface(
  options: OrganizationToolSurfaceOptions
): Promise<OrganizationToolSurface> {
  const tools = new Map<string, RegisteredToolDefinition>()
  const messages: string[] = []
  const lifecycle = new Map<string, ((payload: unknown, ctx: unknown) => unknown)[]>()

  const recorder = {
    registerTool(definition: RegisteredToolDefinition) {
      tools.set(definition.name, definition)
    },
    registerMessageRenderer() {
      /* presentation only */
    },
    registerEntryRenderer() {
      /* presentation only */
    },
    appendEntry() {
      /* cards are not this test's subject */
    },
    on(event: string, handler: (payload: unknown, ctx: unknown) => unknown) {
      // RECORDED, not discarded. Most tools are called directly and need no
      // lifecycle, but the ones gated on a live Pi session cannot be reached
      // at all without `session_start` — see `startSession`.
      const existing = lifecycle.get(event) ?? []
      existing.push(handler)
      lifecycle.set(event, existing)
    },
    sendMessage(message: unknown) {
      messages.push(messageText(message))
    }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionAPI` is a large concrete class surface; the extension calls
  // exactly the four methods above and nothing else, which is what Pi's own
  // loader ends up handing it. Structurally implementing the whole interface
  // would be a second, always-stale copy of a type this fixture never uses.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  // The daemon's own rendezvous, written where the daemon writes it. This is
  // how the extension finds its company: one local read, no registry.
  publishDaemonRendezvous(options.organizationDir, options.chiefdUrl)

  await installOrganizationIntercom(pi, {
    environment: {
      ORG_LAUNCHER_IDENTITY_DIR:
        options.personId === 'chief'
          ? `${options.organizationDir}/.chief`
          : `${options.organizationDir}/.chief/agent/${options.personId}`,
      ORG_LAUNCHER_ORG_DIR: options.organizationDir,
      ORG_LAUNCHER_ORGANIZATION: options.organization,
      ORG_LAUNCHER_PERSON: options.personId,
      ORG_LAUNCHER_ROOT: options.launcherRoot,
      // A PANE or a WEB-HOSTED PERSON, decided by whether a tmux identity was
      // asked for. chiefd's API-host profile publishes no socket and no session
      // name (AC6, `api_host_environment`), so a hosted surface carries
      // neither.
      ...(isNullish(options.tmuxSocket) || isNullish(options.tmuxSession)
        ? {}
        : {
            ORG_LAUNCHER_RUNTIME_SOCKET: options.tmuxSocket,
            ORG_LAUNCHER_RUNTIME_SESSION: options.tmuxSession
          })
    },
    // 0 constructs NO SseWatcher and disables every background timer. A tool
    // fixture asserts what one call did; a supervision cycle firing underneath
    // it would make the result depend on wall time.
    pollIntervalMs: 0,
    turnWatchdogIntervalMs: 0,
    // The daemon is already up before install runs, so the production boot
    // ladder has nothing to wait for — a millisecond ladder turns a genuine
    // unreachable-store bug into a fast red instead of a slow one.
    bootTransientRetryDelaysMs: [1, 1, 1],
    ...options.install
  })

  /**
   * The one stand-in `ExtensionContext` both lifecycle deliveries hand over.
   *
   * It only has to satisfy what the extension actually reads off it:
   * `sessionManager` must be a plain property (the staleness probe is a
   * `void ctx.sessionManager` inside a `try`, so a throwing getter means
   * STALE), `getSessionId`/`getCwd`/`getEntries` must answer, and
   * `getContextUsage` gates the automatic pre-park compaction. `entries`
   * defaults to empty, which the extension classifies as a first boot rather
   * than a resume — the quieter of the two paths for a fixture.
   */
  function extensionContext(contextOptions: {
    sessionId?: string
    entries?: readonly unknown[]
    contextUsagePercent?: number
  }): never {
    const sessionManager = {
      getSessionId: () => contextOptions.sessionId ?? 'tool-contract-session',
      getCwd: () => options.organizationDir,
      getEntries: () => contextOptions.entries ?? [],
      getLeafId: () => undefined
    }
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // Same reasoning as the `pi` recorder above: Pi's `ExtensionContext` is
    // a large concrete surface and the extension reads a handful of it.
    return {
      sessionManager,
      getContextUsage: () => ({ percent: contextOptions.contextUsagePercent ?? 0 }),
      isIdle: () => true,
      hasPendingMessages: () => false
    } as never
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
  }

  return {
    tools,
    messages,
    /**
     * Fire `session_start` with a stand-in `ExtensionContext`.
     *
     * The context only has to satisfy what the extension actually reads off
     * it: `sessionManager` must be a plain property (the staleness probe is a
     * `void ctx.sessionManager` inside a `try`, so a throwing getter means
     * STALE), and `getSessionId`/`getCwd`/`getEntries` must answer. `entries`
     * defaults to empty, which the extension classifies as a first boot
     * rather than a resume — the quieter of the two paths for a fixture.
     */
    async startSession(sessionOptions) {
      const ctx = extensionContext({
        sessionId: sessionOptions?.sessionId,
        entries: sessionOptions?.entries
      })
      for (const handler of lifecycle.get('session_start') ?? []) {
        await handler(undefined, ctx)
      }
    },
    /**
     * Fire `agent_settled` with the same stand-in context, plus the context
     * usage the automatic pre-park compaction reads.
     *
     * This is the only reachable path to `processMaintenance` (the
     * session-maintenance claim ladder) and to `queueAutomaticParkCompaction`
     * (the only reader of `org activity status`). Neither has a tool.
     */
    async settle(settleOptions) {
      const ctx = extensionContext({
        contextUsagePercent: settleOptions?.contextUsagePercent
      })
      for (const handler of lifecycle.get('agent_settled') ?? []) {
        await handler(undefined, ctx)
      }
    },
    async call(name, params) {
      const tool = tools.get(name)
      if (!tool) {
        throw new Error(
          `tool '${name}' is not registered for '${options.personId}'. ` +
            `Registered: ${[...tools.keys()].sort().join(', ')}`
        )
      }
      // Pi's own five-argument contract. `execute` is invoked exactly as the
      // agent loop invokes it — that is the entire point of this fixture.
      const raw = await tool.execute(
        `tool-call-${randomUUID()}`,
        params,
        new AbortController().signal,
        () => undefined,
        {
          abort() {
            /* the per-run circuit breakers; not this fixture's subject */
          },
          isIdle: () => true,
          hasPendingMessages: () => false,
          sessionManager: { getSessionId: () => 'tool-contract-session' }
        }
      )
      return unwrap(raw)
    }
  }
}
