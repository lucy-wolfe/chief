/**
 * The adapter between Pi's `ExtensionAPI` and this host's flat `AgentTool[]`.
 *
 * # The gap this closes
 *
 * chiefd grants a CEO 60 tools. This host built 7 of them — the coding tools
 * Pi exports as constructors — and reported the other 53 as unavailable per
 * person. The reason was never that the `org_*` family could not run here; it
 * was a SHAPE disagreement. `packages/piing/extensions/*` install into Pi's
 * `ExtensionAPI` and call `registerTool`, while `AgentHarness` takes an array
 * of `AgentTool`. Nothing stood between the two, so a hosted agent could talk
 * and could not hire.
 *
 * # One list, and it is the registration
 *
 * The defect this file must not become is a SECOND source of truth. There is
 * exactly one statement of what tools a person has: the `registerTool` calls
 * the extensions make. This module writes down no tool name, no schema and no
 * handler. It installs the same extension modules the tmux pane loads, records
 * what they register, and re-shapes each recorded definition into the
 * `AgentTool` the harness wants. A tool added to an extension appears here on
 * its own; a tool removed disappears on its own.
 *
 * The two shapes are near-identical, which is why the adapter is small:
 * `name`, `label`, `description`, `parameters`, `prepareArguments` and
 * `executionMode` carry across unchanged, and `execute` differs by exactly one
 * argument — Pi passes an `ExtensionContext` fifth, the harness does not.
 * `renderCall`/`renderResult` are dropped on purpose: they build `pi-tui`
 * components for a terminal, and this host has no terminal.
 *
 * # What is installed here, and what is not
 *
 * `organization-intercom`. It is pure at install time — it registers tools
 * and returns — and it takes its identity from an explicit environment rather
 * than the ambient one this shared server runs under, which is the
 * precondition for hosting two companies in one process at all.
 *
 * # The lifecycle is DRIVEN, and the reactive channel is left switched on
 *
 * The extensions also register `on(...)` handlers, and this adapter used to
 * accept them into an empty function while installing the intercom with
 * `pollIntervalMs: 0`. Both halves of that were the same mistake. `0` does not
 * mean "do not poll" — since #827 there is no poll floor to disable and no
 * poll-only mode to configure; it is a test seam meaning "construct NO
 * `SseWatcher`". Passing it switched off the person's only wake path, and the
 * dead handlers guaranteed that nothing could have been delivered even if a
 * frame had arrived: `deliveryReady` becomes true in exactly two handlers.
 *
 * So the options are gone and the registrations are recorded.
 * `server/ExtensionLifecycle` fires them from `AgentHarness`'s own event
 * surface plus the two boundaries the host owns, and a hook it will not drive
 * is refused BY NAME rather than accepted and dropped.
 */
import { installOrganizationIntercom } from '@chief/piing/extensions/organization-intercom'
import type { AgentHarness, AgentTool } from '@earendil-works/pi-agent-core'
import type { ExtensionContext, ToolDefinition } from '@earendil-works/pi-coding-agent'

import {
  driveLifecycle,
  DRIVEN_HOOKS,
  REFUSED_HOOKS,
  unclassifiedHookReason
} from '@/server/ExtensionLifecycle'
import type { AgentProfile } from '@/types/AgentHost'
import type { HostedLifecycle, LifecycleSubject, RecordedHandler } from '@/types/ExtensionLifecycle'
import type { ExtensionInstaller, ExtensionToolSet } from '@/types/ExtensionTools'
import { isNullish } from '@/utils/Nullish'

/**
 * The extensions a hosted person is installed with, in the order chiefd
 * materializes them into a Pi home.
 *
 * A LIST OF INSTALLS, never a list of tools. What each one registers is its
 * own to say.
 */
export const HOSTED_EXTENSIONS: readonly ExtensionInstaller[] = [
  // The environment, and NOTHING else. Every cadence option this call used to
  // carry was a zero, and every one of them switched off a mechanism this host
  // now drives: `pollIntervalMs: 0` refuses to construct the `SseWatcher` that
  // is the person's only wake path, and `turnWatchdogIntervalMs: 0` disarms
  // the deadline that ends a turn stuck with an HTTP request waiting on it.
  // The watchdog's interval is armed at `turn_start` and cleared the moment
  // the turn settles, so an idle company runs it zero times.
  (pi, environment) => installOrganizationIntercom(pi, { environment })
]

/**
 * The `ExtensionContext` a tool gets before the person is bound.
 *
 * Unreachable in production and kept anyway, because the alternative is an
 * `undefined` fifth argument: a tool is called by the model, and the model
 * runs on the harness whose construction binds the real context. It answers
 * the one member a tool reads without a session — `abort()`, the
 * empty-`org_send` circuit breaker's way of ending a turn the model has got
 * stuck in.
 */
function unboundToolContext(harness: () => AgentHarness | undefined): ExtensionContext {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionContext` is a large concrete surface and a tool called
  // before its person is bound reads one member of it.
  return {
    abort: () => {
      const live = harness()
      if (!isNullish(live)) void live.abort()
    }
  } as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
}

/** One registered definition as the harness's own tool.
 *
 * A straight field carry plus one argument. Anything computed here would be
 * this host having its own opinion about a tool the extension defined.
 *
 * The context is read at CALL time from a holder rather than captured, because
 * Pi gives a tool and a lifecycle handler the SAME `ExtensionContext` object
 * and this host must too: a tool holding a thinner context than the handlers
 * would be a second answer to "which session is this". */
function agentTool(definition: ToolDefinition, holder: { current: ExtensionContext }): AgentTool {
  return {
    name: definition.name,
    label: definition.label,
    description: definition.description,
    parameters: definition.parameters,
    ...(isNullish(definition.prepareArguments)
      ? {}
      : { prepareArguments: definition.prepareArguments }),
    ...(isNullish(definition.executionMode) ? {} : { executionMode: definition.executionMode }),
    // Pi's five-argument contract narrowed to the harness's four. The
    // definition's own `execute` is called — never wrapped, never re-ordered,
    // never given a substitute result — so a tool's result path reaches the
    // model exactly as the extension built it.
    execute: (toolCallId, params, signal, onUpdate) =>
      definition.execute(toolCallId, params, signal, onUpdate, holder.current)
  }
}

/** The readable text of a Pi custom message, which may be a string or blocks. */
function messageText(content: string | readonly { type: string; text?: string }[]): string {
  if (typeof content === 'string') return content
  return content
    .filter((part) => part.type === 'text')
    .map((part) => part.text ?? '')
    .join('')
}

/**
 * Install one person's extensions and collect what they registered.
 *
 * `profile.env` is chiefd's own environment for this person — it carries the
 * company slug, the person id and the org directory — and it is the ONLY thing
 * that decides which company this install talks to.
 * Nothing here reads the ambient environment, so two people from two companies
 * installed in this one process reach two daemons.
 */
export async function installExtensions(
  profile: AgentProfile,
  installers: readonly ExtensionInstaller[] = HOSTED_EXTENSIONS
): Promise<ExtensionToolSet> {
  const definitions = new Map<string, ToolDefinition>()
  const handlers = new Map<string, RecordedHandler[]>()
  const refused = new Map<string, string>()
  let harness: AgentHarness | undefined
  let lifecycle: HostedLifecycle | undefined

  const recorder = {
    registerTool(definition: ToolDefinition): void {
      // A duplicate name is a real defect — two extensions claiming one tool —
      // and chiefd's materializer refuses it upstream. Refusing here too means
      // this host can never be the place where one silently wins.
      if (definitions.has(definition.name)) {
        throw new Error(`two extensions registered a tool named "${definition.name}"`)
      }
      definitions.set(definition.name, definition)
    },
    /** Terminal card renderers. This host has no terminal. */
    registerMessageRenderer(): void {},
    /** The same, for custom ENTRY renderers. */
    registerEntryRenderer(): void {},
    /**
     * A custom entry is a TUI transcript append — the delivery a pane-failure
     * card uses because it needs no turn. This host has no transcript of that
     * kind and no surface for one yet, so the append is dropped rather than
     * routed through `deliver`: `deliver` hands the text to a turn, which is
     * exactly the delivery a pane-failure card exists to avoid. Surfacing these
     * to a hosted person is its own piece of work.
     */
    appendEntry(): void {},
    /**
     * Pi's reactive surface, recorded per hook and classified on the spot.
     *
     * A driven hook is recorded in registration order, which is the order Pi
     * runs it in. A refused hook is NOT recorded — the handler is never held
     * and never called — and its name is kept so the host can report it. An
     * unclassified hook is refused for the same reason and reported with a
     * different sentence, because it means somebody added a lifecycle hook to
     * an extension and nobody here decided what this host does about it.
     */
    on(event: string, handler: RecordedHandler): void {
      if (DRIVEN_HOOKS.has(event)) {
        const registered = handlers.get(event)
        if (isNullish(registered)) handlers.set(event, [handler])
        else registered.push(handler)
        return
      }
      const stated = REFUSED_HOOKS.get(event)
      refused.set(event, isNullish(stated) ? unclassifiedHookReason(event) : stated)
    },
    /** A message the extension pushes at its own agent.
     *
     * Handed to the lifecycle rather than to a queue, because `deliverAs`
     * names a queue that only exists while a turn is RUNNING, and the case
     * that matters most here is the idle one — a mailbox drain fires when a
     * person is quiet. `HostedLifecycle.deliver` owns that decision and can
     * make it, because it is the thing that knows whether a turn is in
     * flight. Fire-and-forget, because `ExtensionAPI.sendMessage` is: it
     * returns void and its callers treat a card as best-effort.
     *
     * Before the person is bound there is nothing to deliver to, and nothing
     * can reach here: the extensions push messages from lifecycle handlers and
     * tools, and neither runs before `bind`. */
    sendMessage(
      message: { content: string | readonly { type: string; text?: string }[] },
      options?: { deliverAs?: 'steer' | 'followUp' | 'nextTurn' }
    ): void {
      const live = lifecycle
      if (isNullish(live)) return
      live.deliver(messageText(message.content), options?.deliverAs ?? 'nextTurn')
    }
  }

  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // Pi's `ExtensionAPI` is a large concrete class surface; the extensions call
  // exactly the four methods above, which is what Pi's own loader ends up
  // handing them. Structurally implementing the whole interface would be a
  // second, always-stale copy of a type nothing here uses.
  const pi = recorder as never
  /* eslint-enable @typescript-eslint/consistent-type-assertions */

  for (const install of installers) await install(pi, profile.env)

  const holder = { current: unboundToolContext(() => harness) }
  const tools = new Map<string, AgentTool>()
  // EVERY recorded definition, with no filter of any kind. A `continue` in
  // this loop would be the second source of truth this file exists to avoid.
  for (const [name, definition] of definitions) tools.set(name, agentTool(definition, holder))

  return {
    tools,
    refusedHandlers: [...refused.keys()].sort(),
    bind(subject: LifecycleSubject, transcriptPath: string): HostedLifecycle {
      harness = subject.harness
      lifecycle = driveLifecycle(handlers, subject, transcriptPath)
      // The tools and the handlers share ONE context from here on, exactly as
      // they do under Pi's own runner.
      holder.current = lifecycle.context
      return lifecycle
    }
  }
}
