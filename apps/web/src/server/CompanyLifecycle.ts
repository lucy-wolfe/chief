/**
 * Boot and stop a company, through chiefd's own lifecycle surface.
 *
 * # Why these routes exist now when they did not before
 *
 * `ClientPathsAreServed`'s `UNSERVED` register carried a row saying company
 * lifecycle "belongs to chief create/attach" and that a web route killing a
 * process "would be a second lifecycle". The first half is right and the
 * conclusion was wrong: `chief host` is a resident, per-box HTTP surface —
 * `POST /v1/company/create|boot|stop` — written precisely so a caller can ask
 * chiefd to do these things. Serving them here is a pass-through to that
 * surface, not a second lifecycle. The reason nothing spawns a process here is
 * that nothing needs to.
 *
 * So the browser's "Boot" and "Stop" buttons, and `BootPhaseConsole`, and the
 * whole phase-narration path in `UseCompanyDirectory` — all of it already
 * written — were waiting on two thin route files.
 *
 * # Create is here now, and holds no credential
 *
 * It was not, and the reason was real: `POST /v1/company/create` required a
 * `bootstrap` — the exact model route of a live Founder session plus a
 * refreshed same-provider observation — which is a provider credential and a
 * Pi session, and a browser has neither. The wrong repair would have been to
 * give THIS server one: a company's model route is chiefd's own fact, and
 * putting a key here to re-derive it would spread the credential to a third
 * place to produce something chiefd can already read.
 *
 * chiefd resolves it itself now (`lifecycle::create::founder_bootstrap`, the
 * same box-level Founder route `chief create` uses), so this route forwards
 * two strings and nothing else.
 *
 * # Phases are re-emitted, not proxied
 *
 * The generator is consumed here and re-encoded into this app's own SSE
 * vocabulary. That keeps ONE parser of chiefd's frames — chiefing's, which
 * refuses a malformed one rather than dropping it — and leaves the browser
 * reading a wire contract this app owns, exactly as the person stream does.
 */
import type { CompanyLifecyclePhase } from '@chief/chiefing'
import { CompanyLifecycleClient, CompanyLifecycleRefusalError } from '@chief/chiefing'

import { chiefdHostUrl } from '@/common/Env'
import { companySummary } from '@/server/CompanyDirectory'
import { RouteRefusalError } from '@/server/RouteRefusal'
import type { CompanyStopped } from '@/types/CompanyLifecycle'
import { isNullish } from '@/utils/Nullish'

/** The resident lifecycle surface for this box. */
function lifecycleClient(): CompanyLifecycleClient {
  return new CompanyLifecycleClient({ hostUrl: chiefdHostUrl() })
}

/** One SSE frame, as `SseClientService` reads it. */
function frame(event: string, data: string): string {
  return `event: ${event}\ndata: ${data}\n\n`
}

/* eslint-disable lucy/no-json-stringify */
// SSE wire payloads for this app's own lifecycle channel — the same seam
// `SseClientService.serializeBody` takes on the way out.
function phaseFrame(phase: { phase: string; slug: string; detail: string }): string {
  return frame('phase', JSON.stringify(phase))
}

function terminalFrame(event: 'created' | 'booted', slug: string): string {
  return frame(event, JSON.stringify({ slug }))
}

function failedFrame(code: string, detail: string): string {
  return frame('failed', JSON.stringify({ error: { code, detail } }))
}
/* eslint-enable lucy/no-json-stringify */

/**
 * One narrated launch, re-emitted as this app's own SSE channel.
 *
 * A failure is a `failed` FRAME rather than a rejected stream. The response
 * headers have already gone out by the time chiefd refuses — a launch runs for
 * minutes — so there is no status code left to change, and the browser's
 * lifecycle reader is built to end on a terminal frame. Tearing the connection
 * down instead would leave `BootPhaseConsole` showing the last phase it saw,
 * forever, with no reason.
 *
 * Create and boot share this because they differ in exactly two things: which
 * generator runs, and what the success frame is called. Writing the loop twice
 * would give the browser two chances to be told about a refusal differently.
 */
function narrate(
  event: 'created' | 'booted',
  launch: AsyncGenerator<{ phase: string; slug: string; detail: string }, { slug: string }>
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder()
  return new ReadableStream<Uint8Array>({
    async start(controller) {
      try {
        for (;;) {
          const step = await launch.next()
          if (step.done === true) {
            controller.enqueue(encoder.encode(terminalFrame(event, step.value.slug)))
            break
          }
          controller.enqueue(encoder.encode(phaseFrame(step.value)))
        }
      } catch (error) {
        // chiefd's own code and detail, carried verbatim. A refusal here names
        // the reason a company would not start; replacing it with "failed"
        // would give the operator a second vocabulary for chiefd's answer.
        const refusal =
          error instanceof CompanyLifecycleRefusalError
            ? { code: error.code, detail: error.detail }
            : {
                code: 'lifecycle-failed',
                detail: error instanceof Error ? error.message : String(error)
              }
        controller.enqueue(encoder.encode(failedFrame(refusal.code, refusal.detail)))
      } finally {
        controller.close()
      }
    }
  })
}

/**
 * Create a company, narrating each phase.
 *
 * `{name, purpose}` is the whole request. The slug is chiefd's answer and
 * arrives on the terminal frame; the model route a new CEO runs on is chiefd's
 * too, resolved and proven inside `chief host` (see this module's header), so
 * nothing needs to be observed, held or forwarded here.
 */
export function createCompany(input: {
  name: string
  purpose: string
}): ReadableStream<Uint8Array> {
  return narrate('created', lifecycleClient().create(input))
}

/**
 * Create a company and wait for it, reporting the slug chiefd chose.
 *
 * The same generator `createCompany` narrates, drained to its return value
 * instead of re-encoded as SSE — because the caller is not a browser. The
 * Founder's launch tool is a TOOL: the model gets one result, and a stream of
 * phases is not something it can act on. What it must never get is a partial
 * signal read as success, so a refusal throws here rather than arriving as a
 * frame the tool would have to interpret.
 *
 * There is exactly one lifecycle client and one create call in this app, and
 * this is deliberately not a second one: both entry points go through
 * `lifecycleClient().create`, so a company created by talking to Founder and
 * one created by `POST /api/companies` are the same company genesis.
 */
export async function launchCompany(input: {
  name: string
  purpose: string
}): Promise<{ slug: string; key: string }> {
  const launch = lifecycleClient().create(input)
  for (;;) {
    const step = await launch.next()
    // The KEY comes back too, because the caller's next act is to link to the
    // company it just made, and `/c/[companyKey]` resolves by key. Returning
    // only the slug left that link addressing a company by its display name.
    if (step.done === true) return { slug: step.value.slug, key: step.value.key }
  }
}

/**
 * The display slug for one company key, for the two verbs below.
 *
 * **This is the last slug-keyed hop in this app, and it is not ours.** Every
 * chiefd route resolves a company by `sha256(dir)[..12]`, and so does every
 * other route handler here. The `chief host` company-lifecycle wire
 * (`/v1/company/{create,boot,stop}`) still names companies by slug, so a boot
 * or a stop has to be translated back on the way out. When that wire is
 * re-keyed by directory this function is deleted, not adapted — and until it
 * is, two directories holding companies with the same name are ambiguous TO
 * `chief host`, exactly as they were before, which is why the translation
 * cannot make it worse and cannot make it right either.
 */
async function displaySlugFor(companyKey: string): Promise<string> {
  const summary = await companySummary(companyKey)
  if (isNullish(summary)) {
    throw new CompanyLifecycleRefusalError({
      code: 'unknown-company',
      detail: `no company is registered under key "${companyKey}"`
    })
  }
  return summary.slug
}

/** Boot an already-created company, narrating each phase. */
export function bootCompany(companyKey: string): ReadableStream<Uint8Array> {
  async function* boot(): AsyncGenerator<CompanyLifecyclePhase, { slug: string }> {
    return yield* lifecycleClient().boot(await displaySlugFor(companyKey))
  }
  return narrate('booted', boot())
}

/**
 * Stop a company's runtime, preserving every byte of durable state.
 *
 * Not a stream: chiefd answers it with one object and there is no step a
 * caller could act on differently.
 */
export async function stopCompany(companyKey: string): Promise<CompanyStopped> {
  try {
    const result = await lifecycleClient().stop(await displaySlugFor(companyKey))
    return {
      slug: result.slug,
      mode: result.mode,
      sessionStopped: result.sessionStopped,
      daemonStopped: result.daemonStopped
    }
  } catch (error) {
    if (error instanceof CompanyLifecycleRefusalError) {
      // A refusal is the caller's problem to act on, not an upstream fault.
      throw new RouteRefusalError({ status: 409, code: error.code, message: error.detail })
    }
    throw error
  }
}
