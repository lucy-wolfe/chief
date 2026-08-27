/**
 * Founder Mode: the one agent that exists before any company does.
 *
 * # Why this is not a `HostedRoster` entry
 *
 * Every other agent this server hosts is a PERSON: chiefd's roster says who
 * runs, `AgentHost` keys them `"<companyKey>\0<personId>"`, and their cwd, tools,
 * identity, transcript and credential are all materialized by chiefd before
 * anybody talks to them. Founder has none of that, and cannot: there is no
 * company yet, so there is no roster to be on, no workspace to run in and no
 * `pi-home` to hold a key. A Founder faked into the roster would be this
 * process deciding who runs, which is precisely the second roster mandate 3
 * forbids — and it would need a company key to be keyed by, which is the
 * thing it exists to produce.
 *
 * So Founder is a SINGLETON, deliberately, and the properties that follow from
 * that are the honest ones: one Founder conversation per server, whoever is
 * looking at it.
 *
 * # The session is in memory, and that is a decision rather than a shortcut
 *
 * `AgentHost` writes each person's transcript into their own
 * `pi-home/sessions/` because chiefd and the tmux pane both read it, and a
 * second file would fork one person's history. Nothing reads a Founder's
 * transcript: there is no pane, no daemon and no directory, because the
 * company whose directory it would live in does not exist during the only
 * conversation that matters. `InMemorySessionStorage` is what that fact looks
 * like — and it is also what mandate 5 requires, since a file written here
 * would be durable state chiefd cannot see.
 *
 * # One tool, and it is a different door from the tmux Founder's
 *
 * The tmux Founder's `chiefd_launch_company`
 * (`packages/piing/extensions/founder-launch.ts`) posts to
 * `/v1/founder/launch` with a bootstrap it observes from its own live Pi
 * session — the model route it is actually running on, which is stronger
 * evidence than any file. That extension is unusable here: it dials
 * `CHIEFD_FOUNDER_URL`, the loopback endpoint `chief` binds for the
 * duration of a tmux Founder pane, and it needs a Pi `ExtensionContext` this
 * host does not construct.
 *
 * chiefd wrote a second door for exactly this caller. `POST /v1/company/create`
 * takes `{name, purpose}` and resolves the Founder route itself
 * (`chief-cli/src/create.rs::founder_bootstrap`), and its own doc says why:
 * "its callers are browsers", and requiring a bootstrap "quietly made a
 * provider credential a precondition for creating a company". So this tool
 * carries the same NAME and the same parameter schema as the tmux one — it is
 * the same product action — through the door built for it, which is the one
 * `POST /api/companies` already uses.
 */
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { AgentHarness, InMemorySessionStorage, Session } from '@earendil-works/pi-agent-core'
import { NodeExecutionEnv } from '@earendil-works/pi-agent-core/node'
import { Type } from 'typebox'

import { launchCompany } from '@/server/CompanyLifecycle'
import { founderSystemPrompt } from '@/server/FounderIdentity'
import { operatorRoute } from '@/server/OperatorPi'
import { RouteRefusalError } from '@/server/RouteRefusal'
import type { FounderLaunched } from '@/types/Founder'
import type { OperatorRoute } from '@/types/OperatorPi'
import { isNullish } from '@/utils/Nullish'

/** A Founder this box cannot run, with the reason an operator can act on. */
export class FounderUnavailableError extends RouteRefusalError {}

/** The launch tool's name. The SAME name the tmux Founder's tool has, because
 * it is the same product action; see the module header for the two doors. */
const LAUNCH_TOOL = 'chiefd_launch_company'

/** The launch tool's parameters — the SAME schema the tmux Founder's tool
 * declares, because it is the same product action; see the module header.
 *
 * A named constant rather than an inline object so the tool's own `params` is
 * typed from it. Annotated as the default `AgentTool` instead, `params` is
 * `Static<TSchema>` — `unknown` — and every field read becomes a cast. */
const LAUNCH_PARAMETERS = Type.Object(
  {
    name: Type.String({ minLength: 2, maxLength: 120, description: 'Confirmed company name' }),
    purpose: Type.String({
      minLength: 4,
      maxLength: 2_000,
      description: 'Confirmed company purpose'
    })
  },
  { additionalProperties: false }
)

/** Where the launch tool records what it created.
 *
 * A mutable object rather than a return value because the fact travels the
 * wrong way for one: the TOOL learns the slug, and the ROUTE — which is
 * awaiting a whole turn, not a tool call — is what must report it. The model's
 * prose is not a substitute; a UI parsing a slug out of English would sooner
 * or later link somewhere that does not exist. */
interface LaunchRecord {
  launched?: FounderLaunched
}

/** The live Founder, or nothing when nobody has started one. */
interface HostedFounder {
  readonly harness: AgentHarness
  /** The session the harness is writing to.
   *
   * Held because `AgentHarness.session` is PRIVATE — the same reason
   * `AgentHost` holds each person's. A transcript read has no other way to
   * reach the entries the harness is appending. */
  readonly session: Session
  /** The tools this Founder was built with.
   *
   * Held for the same reason, and for one more: what a Founder may DO is the
   * single most important fact about it, and `AgentHarness.tools` is private,
   * so nothing outside this module could otherwise state it — not a route, not
   * a test, not an operator. */
  readonly tools: readonly AgentTool<typeof LAUNCH_PARAMETERS>[]
  /** The prompt it was built with. Private on the harness, same as above. */
  readonly systemPrompt: string
  readonly route: OperatorRoute
  /** What this conversation has created, if anything. */
  readonly launch: LaunchRecord
}

let founder: HostedFounder | undefined

/**
 * The route this box runs a Founder on, or a refusal naming what is missing.
 *
 * Refusing on the FIRST request rather than at the first turn is the whole
 * point. A host's Pi can name a model it holds no credential for, and an agent
 * built on one looks perfectly healthy right up until it answers nothing — the
 * failure shape this app keeps reproducing.
 *
 * # The route is not configuration, and configuring it was the bug
 *
 * This used to read `CHIEF_FOUNDER_PROVIDER`/`CHIEF_FOUNDER_MODEL`, two
 * variables an operator set once. Founder mode has no route of its own: the
 * Founder PANE takes whatever model its live Pi session is running, and Pi
 * resolves that from its own defaults. So the variables were a second answer to
 * a question Pi had already answered, and on a box whose only provider is
 * custom they answered it wrong. `server/OperatorPi` asks Pi itself, and every
 * hosted company person is asking the same question through the same call.
 */
async function requiredRoute(): Promise<OperatorRoute> {
  const route = await operatorRoute()
  if (isNullish(route)) {
    throw new FounderUnavailableError({
      status: 409,
      code: 'founder-route-unset',
      message:
        "This box's own Pi resolves no model for a session that names none, so there is no " +
        'Founder route to inherit. Pi builds that route from its own agent directory — ' +
        'settings.json (defaultProvider/defaultModel), models.json, models-store.json and ' +
        'auth.json — and between them they either name no model, describe no provider for ' +
        'it, or carry no usable credential. Choose a model in Pi, which writes settings.json ' +
        'for you, then reload. Nothing is defaulted here: a Founder on a route nobody chose ' +
        'would create whole companies on it.'
    })
  }
  return route
}

/**
 * The launch tool, bound to the Founder it reports into.
 *
 * `hosted` is passed rather than looked up so the tool can only ever record a
 * launch against the conversation it was built for. The binding is late for
 * the same reason `AgentHost`'s extension binding is: the tool is constructed
 * beside the harness and cannot be called before that harness exists, because
 * only the model calls it.
 */
function launchTool(record: LaunchRecord): AgentTool<typeof LAUNCH_PARAMETERS> {
  return {
    name: LAUNCH_TOOL,
    label: 'Launch company',
    description:
      'Create a durable company from its confirmed name and purpose, then boot only its CEO. ' +
      'This is the sole company-launch action available to Founder mode.',
    parameters: LAUNCH_PARAMETERS,
    async execute(_toolCallId, params) {
      const name = params.name.trim()
      const purpose = params.purpose.trim()
      try {
        const { slug, key } = await launchCompany({ name, purpose })
        // BOTH, and they are not interchangeable: `key` addresses the company
        // and `slug` is what a person reads. Carrying only the slug is what
        // pointed the "Open <company>" link at a route that resolves by key.
        record.launched = { key, slug, name }
        return {
          content: [
            {
              type: 'text',
              text:
                `✅ Company launched · ${name}\nIts CEO is booted. Tell the operator the ` +
                `company is ready and that they can open it; do not create another.`
            }
          ],
          details: { ok: true, slug }
        }
      } catch (error) {
        // chiefd's genesis is one transaction: it committed or it did not.
        // The model must never infer a launched company from a partial
        // signal, so a refusal says so in the words it will repeat.
        //
        // AND IT SAYS WHAT MAY HAPPEN NEXT. The success path above ends in a
        // prohibition — "do not create another" — and a failure path carrying
        // only a second prohibition and no permission is not neutral: an agent
        // told what it must not do after success and nothing after failure
        // reports the error and stops. That is a dead-ended conversation, and
        // this tool replaced a Retry button, so a dead end here is a recovery
        // path removed and nothing put in its place.
        //
        // The permission is bounded by the causes actually seen on this
        // product: a Pi that named no route and a stale launcher root
        // are both OPERATOR-FIXABLE BETWEEN ATTEMPTS, which is exactly when
        // retrying is the right move rather than a loop. So it is stated as
        // "after the operator has acted", never "try again now" — the second
        // would produce an agent hammering a refusal that cannot change.
        const reason = error instanceof Error ? error.message : String(error)
        return {
          content: [
            {
              type: 'text',
              text:
                `❌ Company launch failed · ${name}\nNo company was created. Do not treat this ` +
                `company as launched or running.\n\n${reason}\n\n` +
                'You MAY call this tool again with the same name and purpose once the cause ' +
                'above has been dealt with — most of these are environment problems the ' +
                'operator can fix without leaving this conversation. Tell them what failed in ' +
                'plain words, say that you can retry once it is fixed, and wait for them. Do ' +
                'not retry immediately or repeatedly: nothing you can do changes the outcome ' +
                'until something outside this conversation changes.'
            }
          ],
          details: { ok: false }
        }
      }
    }
  }
}

/**
 * The live Founder, started on first use.
 *
 * Started rather than converged: there is no roster to obey, because the whole
 * point of this agent is that no company exists yet. What it still obeys is
 * the box — an unset or uncredentialed route is a refusal, never a default.
 */
export async function founderAgent(): Promise<HostedFounder> {
  const route = await requiredRoute()
  const existing = founder
  // A route change is a different Founder. Rebuilding on it rather than
  // answering from the old one means an operator who fixes their environment
  // and reloads gets the route they set, not the one the server started on.
  if (!isNullish(existing) && sameRoute(existing.route, route)) return existing

  const session = new Session(new InMemorySessionStorage())
  const launch: LaunchRecord = {}
  const tools = [launchTool(launch)]
  const systemPrompt = founderSystemPrompt()
  const harness = new AgentHarness({
    // The Founder has no workspace, because it has no company. Nothing it can
    // call touches the filesystem — it holds one tool — so the execution
    // environment's cwd is inert, and this server's own is the only honest
    // answer available.
    env: new NodeExecutionEnv({ cwd: process.cwd() }),
    session,
    models: route.models,
    model: route.model,
    tools,
    systemPrompt
  })
  // `launch` is the SAME object the tool closes over, carried by reference. A
  // copy here would leave every launch invisible to the route reading it.
  founder = { harness, session, tools, systemPrompt, route, launch }
  return founder
}

function sameRoute(a: OperatorRoute, b: OperatorRoute): boolean {
  return a.provider === b.provider && a.model.id === b.model.id
}

/** The live Founder, or nothing when none has been started. */
export function hostedFounder(): HostedFounder | undefined {
  return founder
}

/**
 * Drop the Founder conversation.
 *
 * Exported for the tests, and for the one product reason a Founder is ever
 * discarded: the conversation is over because a company now exists. Nothing
 * durable is lost — the session was never on disk.
 */
export function resetFounder(): void {
  founder = undefined
}
