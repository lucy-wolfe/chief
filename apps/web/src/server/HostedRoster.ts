/**
 * chiefd's roster decision, made live in this process.
 *
 * # One roster, and it is chiefd's
 *
 * `/v1/org/api-host-launch-profile/read` returns a profile for exactly the
 * people chiefd's own `desired_topology` wants running — not everybody in the
 * manifest. This module launches those and drops everybody else. It decides
 * NOTHING about who should run.
 *
 * That is not a style preference. apps/api answered the same question in
 * TypeScript (`desiredOrganizationPeople`, reading `activity.people[id].active`
 * when chiefd writes `lastDesiredActive`), read `undefined`, defaulted to
 * false, and concluded nobody was ever desired. No agent ever launched, and
 * every attempt to talk to one answered 409 `person-not-running` — while every
 * suite stayed green, because the suites tested the reimplementation against
 * itself. Two implementations of one rule in two languages is what mandate 3
 * forbids, and this is the shape of the bug it forbids.
 *
 * # There is ONE route, and it is the operator's own Pi
 *
 * Every hosted person and the Founder run on the same catalog and the same
 * model: whatever the operator's own Pi resolves for a session that names none
 * (`server/OperatorPi`). So a route failure is never one person's — it is the
 * box's, and it refuses the whole convergence by name instead of marking each
 * person unroutable in turn. The per-person `unroutable` list went with the
 * per-person route it described.
 */
import type { ApiHostActuation, ApiHostLaunchProfile } from '@chief/chiefing'
import type { AgentHarness } from '@earendil-works/pi-agent-core'

import {
  hostAgent,
  hostedAgent,
  refusedHandlers,
  retainDesired,
  unavailableTools
} from '@/server/AgentHost'
import { companyChiefd, CompanyUnavailableError } from '@/server/CompanyChiefd'
import { operatorRoute } from '@/server/OperatorPi'
import type { AgentProfile } from '@/types/AgentHost'
import type { DegradedPerson, RosterConvergence } from '@/types/HostedRoster'
import { isNullish } from '@/utils/Nullish'

/** Whether this server may host the company's agents.
 *
 * chiefd used to answer this by REFUSING the profile read outside shadow mode,
 * and this module recognised the refusal by its code. chiefd now publishes the
 * three facts the refusal carried and leaves the decision here, which is where
 * it always belonged: chiefd cannot know whether its caller is about to launch
 * anything, and the same read is the launch half of the contract for a tmux
 * client, which runs under `apply` by definition.
 *
 * The decision itself is unchanged and so is its consequence: a company
 * actuating tmux panes is not hosted here, because a host launching agents
 * beside that would be the second roster this whole layer exists to prevent.
 * The EFFECTIVE mode is what decides — a tripped breaker forces shadow, and
 * under a tripped breaker chiefd is not actuating anything either. */
function hostsAgentsHere(actuation: ApiHostActuation): boolean {
  return actuation.effectiveMode === 'shadow'
}

/** The sentence an operator gets when the company runs in tmux.
 *
 * Composed HERE from chiefd's facts, not forwarded from chiefd. chiefd used to
 * supply the whole message, including a `chiefd set-actuation-config` command
 * line and a restart instruction — a backend describing a CLI to a browser.
 * The facts are chiefd's; the phrasing is this server's. */
function notApiHostedDetail(companyKey: string, actuation: ApiHostActuation): string {
  const breaker = actuation.breakerTripped ? ', and the converge breaker is TRIPPED' : ''
  return (
    `company "${companyKey}" runs its agents in tmux, so this server does not host them. ` +
    `Its effective actuation mode is ${actuation.effectiveMode}, configured ` +
    `${actuation.configuredMode}${breaker}. Agents are hosted here only in shadow mode.`
  )
}

/** chiefd's wire profile as the host's own profile.
 *
 * A straight field carry, which is the point: the two shapes agree because
 * chiefd decides both. Anything computed here would be a second opinion. */
function agentProfile(plan: ApiHostLaunchProfile): AgentProfile {
  return {
    personId: plan.personId,
    cwd: plan.cwd,
    env: plan.env,
    // Straight field carries. These were the two facts being DROPPED: the
    // agent got no tools and no identity, so it could talk and do nothing,
    // and it answered as a general-purpose assistant rather than as the
    // person chiefd staffed.
    tools: plan.tools,
    displayName: plan.displayName,
    ...(isNullish(plan.sessionFile) ? {} : { sessionFile: plan.sessionFile })
  }
}

/**
 * Bring this process in line with chiefd's roster for one company.
 *
 * Returns who is now hosted, and who is hosted with less than their extensions
 * asked for. Silence would leave the operator looking at an agent that appears
 * fine and never answers — the failure this whole program keeps reproducing.
 */
export async function convergeRoster(companyKey: string): Promise<RosterConvergence> {
  const chiefd = await companyChiefd(companyKey)
  const { actuation, plans } = await chiefd.apiHostLaunchProfile.read(companyKey)
  // Same refusal, same code, same status as when chiefd raised it — a 409
  // naming the state, never a 502, because nothing is broken. Reported as an
  // upstream fault it would read as "the daemon is down" about a daemon that
  // is working correctly and reporting its own mode.
  if (!hostsAgentsHere(actuation)) {
    throw new CompanyUnavailableError({
      status: 409,
      code: 'company-not-api-hosted',
      message: notApiHostedDetail(companyKey, actuation)
    })
  }

  // Drop the undesired FIRST, and AWAIT it. A person chiefd no longer wants
  // running must stop being hosted even if every launch below fails; deferring
  // it to the end would keep an offboarded person alive on any error path.
  // Awaited because dropping is no longer only a map delete: it fires
  // `session_shutdown`, which is what closes that person's SSE subscription.
  await retainDesired(
    companyKey,
    plans.map((plan) => plan.personId)
  )

  // ONE resolution for the whole company, and a refusal when this box's Pi
  // names no default model. Refusing by name beats hosting agents that would
  // each fail at their first turn: an unrouted agent looks healthy and answers
  // nothing, which is the failure this program keeps reproducing.
  const route = await operatorRoute()
  if (isNullish(route)) {
    throw new CompanyUnavailableError({
      status: 409,
      code: 'operator-route-unset',
      message:
        "This box's own Pi resolves no model for a session that names none, so there is " +
        'nothing for its agents to run on. Choose a model in Pi, which writes its own ' +
        'settings.json, then reload. Nothing is defaulted here: a company on a route nobody ' +
        'chose would answer as somebody else.'
    })
  }

  const hosted: string[] = []
  const degraded: DegradedPerson[] = []
  for (const plan of plans) {
    await hostAgent(companyKey, agentProfile(plan), route)
    hosted.push(plan.personId)
    const missingTools = unavailableTools(companyKey, plan.personId)
    const refusedHooks = refusedHandlers(companyKey, plan.personId)
    if (missingTools.length > 0 || refusedHooks.length > 0) {
      degraded.push({ personId: plan.personId, missingTools, refusedHandlers: refusedHooks })
    }
  }
  return { hosted, degraded }
}

/**
 * The live harness for one person, converging the roster first if needed.
 *
 * A talk verb calls this rather than `hostedAgent` directly: the first request
 * after a restart finds an empty registry, and answering it "not running" when
 * chiefd plainly wants the person running would be this process disagreeing
 * with the roster it does not own.
 */
export async function agentFor(
  companyKey: string,
  personId: string
): Promise<AgentHarness | undefined> {
  const existing = hostedAgent(companyKey, personId)
  if (!isNullish(existing)) return existing
  await convergeRoster(companyKey)
  return hostedAgent(companyKey, personId)
}
