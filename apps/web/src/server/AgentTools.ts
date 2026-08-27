/**
 * The tools a hosted agent may call, built from the ids chiefd named.
 *
 * # The gap this closed
 *
 * `ApiHostLaunchProfile` carries `tools` — 60 ids for a CEO, from `read` and
 * `bash` through the whole `org_*` family — and this host could build 7 of
 * them. An agent hosted by this server could talk and could do nothing: asked
 * what company it ran, it answered "I don't run any company — I'm Claude, an
 * AI assistant created by Anthropic." A chat box wearing a CEO's name.
 *
 * The 53 were not missing for want of a daemon or a credential. They were
 * missing because of a SHAPE: the organization extension registers its tools
 * into Pi's `ExtensionAPI` and this host builds a flat `AgentTool[]`, and no
 * adapter stood between the two. `server/ExtensionTools` is that adapter, and
 * this module now asks it for the `org_*` family exactly as it asks
 * `pi-coding-agent` for `read`.
 *
 * # Pi's own tools, selected by chiefd's own ids
 *
 * Each tool is Pi's or an extension's, built by its own code; this file only
 * decides WHICH of them a person gets, and it takes that decision from
 * chiefd's list. Nothing here invents a tool, grants one, or writes down a
 * schema or a handler.
 *
 * The seven coding tools are written out rather than reached through a
 * wildcard because `@earendil-works/pi-coding-agent` exports the constructors
 * individually and not a map. The extension tools are the opposite and must
 * stay that way: they are whatever the extensions registered, enumerated at
 * install, never listed here. A second list of org tool names in this file
 * would be the exact defect the adapter exists to avoid.
 *
 * # What is still NOT available, and why it is reported rather than dropped
 *
 * NOTHING is unbuildable in this process any more: a converged CEO's whole
 * grant is built here. The reporting path below is kept, and it stays the
 * point — an id no extension registers is named on the roster rather than
 * silently dropped, which is what made the original 53-tool gap invisible.
 *
 * `org_set_assignment_blocked` was the last of the gap, and it was missing for
 * a reason that was never about this host: the intercom registered it only
 * behind a fence on
 * `ORG_LAUNCHER_RUNTIME_SOCKET`/`ORG_LAUNCHER_RUNTIME_SESSION` — a TMUX
 * identity chiefd deliberately does not publish on the API-host profile
 * (AC6). Nothing was added here to make it happen.
 *
 * Silently dropping an unbuildable id is what made the original gap invisible:
 * an agent that looks staffed and cannot message is worse than one that says
 * it cannot. `refusedHandlers` travels beside it for the same reason: a
 * lifecycle hook this host will not drive is reported by name rather than
 * accepted into an empty function.
 */
import type { AgentTool } from '@earendil-works/pi-agent-core'
import {
  createBashTool,
  createEditTool,
  createFindTool,
  createGrepTool,
  createLsTool,
  createReadTool,
  createWriteTool
} from '@earendil-works/pi-coding-agent'

import { installExtensions } from '@/server/ExtensionTools'
import type { AgentProfile } from '@/types/AgentHost'
import type { ToolSelection } from '@/types/AgentTools'
import type { ExtensionInstaller } from '@/types/ExtensionTools'
import { isNullish } from '@/utils/Nullish'

/** The tools this host builds itself, by the id chiefd uses for each. */
function codingTools(cwd: string): ReadonlyMap<string, AgentTool> {
  return new Map<string, AgentTool>([
    ['read', createReadTool(cwd)],
    ['bash', createBashTool(cwd)],
    ['edit', createEditTool(cwd)],
    ['write', createWriteTool(cwd)],
    ['grep', createGrepTool(cwd)],
    ['find', createFindTool(cwd)],
    ['ls', createLsTool(cwd)]
  ])
}

/**
 * The tools for one person, plus the ids this host could not supply.
 *
 * `profile.cwd` scopes every file and shell tool to the person's own
 * materialized workspace, which is the only isolation a shared process can
 * offer them — and the same boundary the tmux pane has, since chiefd spawns
 * that pane there too. `profile.env` scopes every extension tool to the
 * person's own company and daemon, which is the same isolation one layer up.
 *
 * `installers` is a seam for this module's own tests and nothing else:
 * production always installs the extensions a materialized Pi home carries.
 */
export async function selectTools(
  profile: AgentProfile,
  installers?: readonly ExtensionInstaller[]
): Promise<ToolSelection> {
  const extensions = await installExtensions(profile, installers)
  const available = new Map<string, AgentTool>(codingTools(profile.cwd))
  for (const [name, tool] of extensions.tools) {
    // A collision is a real defect — an extension shadowing a coding tool —
    // and chiefd's materializer refuses one upstream (`PROTECTED_TOOL_SHADOWED`).
    // Refusing here too means this host can never be where one silently wins.
    if (available.has(name)) throw new Error(`extension tool "${name}" shadows a built-in tool`)
    available.set(name, tool)
  }

  const tools: AgentTool[] = []
  const unavailable: string[] = []
  for (const id of profile.tools) {
    const found = available.get(id)
    if (isNullish(found)) unavailable.push(id)
    else tools.push(found)
  }
  return {
    tools,
    unavailable: unavailable.sort(),
    refusedHandlers: extensions.refusedHandlers,
    bind: extensions.bind
  }
}
