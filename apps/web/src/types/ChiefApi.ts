/**
 * zod schemas + inferred types for every apps/api response
 * `ChiefApiClientService` consumes.
 *
 * THE CONTRACT IS apps/api's OWN TYPES, NOT A PROSE ROUTE TABLE. Every
 * schema below is transcribed from the interface the handler actually
 * returns — `apps/api/src/types/CompanyTypes.ts`, `…/AgentTalkTypes.ts` — and was
 * re-verified against a live `curl` of each route. This file used to be
 * transcribed from the E5 epic's prose `## Contract` section (#805) instead,
 * and that section describes a service apps/api never built: it named an
 * envelope on three routes that serve bare arrays, and named twelve fields
 * across `/companies`, `/companies/:companyKey` and `/people` that no handler has
 * ever populated. Nobody ran the two together until the divergence was
 * found, so a mismatch here is a bug in THIS file — never a reason to
 * invent a field or add an optional fallback (mandate 0).
 *
 * Response bodies are validated at the boundary (a wrong shape from
 * apps/api is a bug worth failing loudly on, not silently rendering).
 * Request bodies this service constructs itself are plain TypeScript types
 * — there is nothing to validate crossing a boundary we do not control the
 * far side of in the same way.
 *
 * Two payloads are deliberately left opaque rather than modeled: a person's
 * live `session` snapshot and a transcript's `entries` are Pi's own shapes
 * (`RpcSessionState` / `SessionEntry`), and apps/web has no Pi dependency
 * (E3's interface table). The web is a tolerant reader of those — S6 (the
 * agent pane) narrows what it actually renders from them; this file only
 * proves the ENVELOPE shape.
 */
import { z } from 'zod'

import type { FetchImpl } from '@/types/Fetch'

/* Every optional field below is `.nullish()`, not `.optional()`: the house
 * rule `lucy/no-optional-nullable` allows only that spelling. It is one
 * value wider than the wire — apps/api OMITS an absent field (Hono's
 * `context.json` runs `JSON.stringify`, which drops `undefined`) and never
 * sends an explicit `null` for one — so nothing reads a `null` here as
 * meaningful. The single exception is a person's `session`, where `null` IS
 * apps/api's answer and is documented at its own declaration. */

// ---- shared vocabularies --------------------------------------------------

const ErrorEnvelopeInnerSchema = z.object({
  code: z.string(),
  detail: z.string()
})
export const ErrorEnvelopeSchema = z.object({ error: ErrorEnvelopeInnerSchema })

// ---- health ----------------------------------------------------------------

/** `GET /health`, exactly as `apps/api`'s `HealthController` builds it.
 *
 * It used to declare `version: z.string()` — a field no chief-api health
 * handler has ever served — and omit `agents`, the one live fact it does
 * serve. */
export const HealthResponseSchema = z.object({
  ok: z.boolean(),
  service: z.string(),
  agents: z.object({ running: z.number() })
})
export type HealthResponse = z.infer<typeof HealthResponseSchema>

// ---- companies ---------------------------------------------------------------

/** apps/api's `CompanyChiefdHealth` — derived entirely from `DocsClient.probe()`.
 * Note what is NOT in here: the chiefd URL. apps/api carries that as a
 * sibling of `chiefd`, not a member of it, and only for a company that
 * registered a location. */
const ChiefdHealthSchema = z.object({
  healthy: z.boolean(),
  httpStatus: z.number().nullish(),
  reason: z.string().nullish(),
  runtimeMode: z.enum(['company', 'docstore-only']).nullish()
})

/** apps/api's `CompanySummary`. ONE type for two routes, because apps/api
 * itself declares one (`export type CompanyStatus = CompanySummary`):
 * `GET /companies` serves a BARE ARRAY of these and `GET /companies/:companyKey`
 * serves exactly one of them.
 *
 * This replaces the former `CompanyListItem`/`CompanyDetail` pair, which
 * between them declared eight fields apps/api has never served — `name`,
 * `hosting`, `peopleCount`, `departmentCount`, `chiefPersonId`,
 * `runningPeople`, and a `chiefd.url`/`chiefd.mode` nesting that belongs to
 * no handler. The identity a company is listed and opened under is its
 * KEY; the human name lives on the root department, which
 * `GET /companies/:companyKey/tree` serves.
 *
 * `url` is absent (not null) for a company that never registered a
 * location — apps/api builds the field conditionally and `JSON.stringify`
 * drops it. It is decoded and handed back to the caller unread: nothing in
 * apps/web may fetch a chiefd address directly (rulings D1/D2). */
export const CompanySummarySchema = z.object({
  /** `sha256(dir)[..12]` — the company's identity, and the handle every route
   * addresses it by. The SLUG below is a display word: two directories may
   * hold companies with the same one, so a link built from it would be
   * ambiguous. */
  key: z.string(),
  /** The directory the company occupies. */
  dir: z.string(),
  slug: z.string(),
  status: z.enum(['running', 'stopped']),
  url: z.string().nullish(),
  chiefd: ChiefdHealthSchema
})
export type CompanySummary = z.infer<typeof CompanySummarySchema>

/** A BARE ARRAY — `CompanyController.list` returns `context.json(summaries)`
 * with no `{companies}` envelope around it. */
export const CompaniesResponseSchema = z.array(CompanySummarySchema)
export type CompaniesResponse = z.infer<typeof CompaniesResponseSchema>

export const StopResponseSchema = z.object({ stopped: z.literal(true) })
export type StopResponse = z.infer<typeof StopResponseSchema>

// ---- company tree --------------------------------------------------------

/** One person as the company tree carries them.
 *
 * This mirrors apps/api's `CompanyTreePerson` EXACTLY. It used to declare
 * `personId`, `employmentState`, `running`, `provider`, `model` and
 * `thinkingLevel` — six fields the tree route has never served — so every
 * company page failed schema validation during hydration, left `ready: false`,
 * and rendered "Loading company…" forever. Runtime state comes from
 * `/people` (see `runningOverrides`), which is the route that actually knows
 * it; the tree is placement and identity.
 *
 * `accent` is absent for a standard Pi identity — apps/api's own documented
 * special case, not a missing value. */
const TreePersonSchema = z.object({
  id: z.string(),
  name: z.string(),
  title: z.string(),
  kind: z.string(),
  // Not optional, and not a bare string. chiefd's projection always sends it,
  // and the union is what lets the rail decide: a `departed` person is still
  // LISTED — the manifest keeps them and the tree places them — but they are
  // no longer somebody an operator can transfer or offboard. Accepting a
  // missing value here would restore the defect quietly, by rendering a
  // departure as an ordinary person again.
  employmentState: z.enum(['active', 'benched', 'departed']),
  accent: z.string().nullish()
})
export type TreePerson = z.infer<typeof TreePersonSchema>

/** Recursive department node. Hand-written interface (zod v3's `z.lazy`
 * needs an explicit `z.ZodType<T>` annotation to break the inference cycle —
 * this is a type ANNOTATION, not `z.infer<typeof …>` in a signature, so it
 * does not trip `lucy/no-inline-zod-infer`). */
export interface DepartmentNode {
  id: string
  name: string
  headPersonId: string
  state: 'active' | 'paused'
  people: TreePerson[]
  children: DepartmentNode[]
}

const DepartmentNodeSchema: z.ZodType<DepartmentNode> = z.lazy(() =>
  z.object({
    id: z.string(),
    name: z.string(),
    headPersonId: z.string(),
    state: z.enum(['active', 'paused']),
    people: z.array(TreePersonSchema),
    children: z.array(DepartmentNodeSchema)
  })
)

/** `GET /companies/:companyKey/tree`, exactly as apps/api serves it: the
 * company handle, the root department's id, and the departments as a forest.
 * There is no `{tree}` envelope — the api returns the tree itself. */
export const CompanyTreeSchema = z.object({
  /** chiefd's own field name, and NOT a display slug: `POST
   * /v1/org/tree/structured` takes the company key (its `SlugRequest.slug` is
   * documented "the own-company documentKey") and echoes it back here. The
   * spelling is chiefd's to change, so this schema keeps it. */
  slug: z.string(),
  rootDepartmentId: z.string(),
  departments: z.array(DepartmentNodeSchema)
})
export type CompanyTree = z.infer<typeof CompanyTreeSchema>

// ---- people ---------------------------------------------------------------

/** `GET /api/companies/:companyKey/people` — who is up, and who is not.
 *
 * This REPLACES a shape that came from apps/api: an array of people each
 * carrying a `session` object, where `session !== null` meant running. That
 * signal belonged to an RPC child which no longer exists — harnesses are
 * hosted in this server's own process now, so the host answers directly with
 * the roster it converged. */
/** A hosted person running with less than their extensions asked for.
 *
 * Declared rather than stripped: an agent running without the `org_*` family
 * looks perfectly staffed and cannot hire, delegate or create a department,
 * and the operator has no way to learn that from the outside. A schema that
 * silently dropped this field would recreate exactly the invisibility the
 * server added it to end.
 *
 * `refusedHandlers` is the same fact one layer over: a lifecycle hook the
 * extensions registered and this host will not fire. It degrades a person the
 * other way round from a missing tool — not something the agent cannot do, but
 * something that will not happen TO the agent — so it is its own field. */
const DegradedPersonSchema = z.object({
  personId: z.string(),
  missingTools: z.array(z.string()),
  refusedHandlers: z.array(z.string())
})

export const PeopleResponseSchema = z.object({
  hosted: z.array(z.string()),
  degraded: z.array(DegradedPersonSchema)
})
export type PeopleResponse = z.infer<typeof PeopleResponseSchema>

// ---- transcript / mailbox --------------------------------------------------

/** Pi's own `SessionEntry` shape, forwarded opaque (same reasoning as
 * `PersonSessionSnapshot`).
 *
 * `leafId` is NULLABLE: apps/api's `TranscriptResult` declares
 * `leafId: string | null` and relays `RpcClientLike.getEntries`'s own value,
 * which is `null` for a session with no entries yet. This used to be a bare
 * `z.string()`, so the first transcript read of a freshly started agent
 * threw a ZodError instead of rendering an empty pane. */
export const TranscriptResponseSchema = z.object({
  entries: z.array(z.unknown()),
  leafId: z.string().nullish()
})
export type TranscriptResponse = z.infer<typeof TranscriptResponseSchema>

/** One person's mailbox.
 *
 * `pendingCount` is counted on the SERVER against chiefd's bucket vocabulary
 * (`server/Mailbox.ts`). The route used to forward chiefd's raw row read, whose
 * `document` is a serialized JSON string, while this schema declared the shape
 * apps/api used to synthesise — so every mailbox read threw a ZodError. */
export const MailboxResponseSchema = z.object({
  personId: z.string(),
  pendingCount: z.number(),
  envelopes: z.array(z.unknown())
})
export type MailboxResponse = z.infer<typeof MailboxResponseSchema>

// ---- talking to an agent ---------------------------------------------------

export type SayMode = 'prompt' | 'steer' | 'followUp'

/** One message on its way to an agent.
 *
 * The field is `text`, not `message`. It used to be `message` here and `text`
 * on the route, and neither half's tests could see the other: every message an
 * operator typed came back `422 empty-message` while both suites stayed
 * green. */
export interface SayInput {
  text: string
  mode?: SayMode
}

/** What the server says one message DID.
 *
 * Shaped for a server that AWAITS the turn. It used to expect
 * `{queued: true}` — apps/api's fire-and-forget acknowledgement —
 * so a successful turn threw a ZodError in the client while the route returned
 * 200 with the agent's actual words in it.
 *
 * `reply` is optional because only `prompt` has one: `steer` and `followUp`
 * join a turn that is already running. */
export const SayResponseSchema = z.object({
  personId: z.string(),
  mode: z.enum(['prompt', 'steer', 'followUp']),
  // `nullish` rather than `optional` per `lucy/no-optional-nullable`: a queued
  // mode carries no reply, and an absent field and a null one must not be two
  // different answers to "did the agent say anything".
  reply: z.string().nullish()
})
export type SayResponse = z.infer<typeof SayResponseSchema>

/** What abort THREW AWAY.
 *
 * The queued messages the harness discarded, which is the part an operator
 * needs: somebody who steered three messages and then stopped must know those
 * three are gone rather than pending. It used to expect `{aborted: true}`, a
 * field the route has never sent. */
export const AbortResponseSchema = z.object({
  clearedSteer: z.number(),
  clearedFollowUp: z.number()
})
export type AbortResponse = z.infer<typeof AbortResponseSchema>

/* THREE RESPONSE SCHEMAS ARE GONE, AND SO ARE THEIR ROUTES AND BUTTONS:
 * `StartPersonResponse` (`…/people/:id/start`), `NewSessionResponse`
 * (`…/session/new`) and `CompactResponse` (`…/session/compact`).
 *
 * None of the three routes has ever existed in this server, so each schema
 * described a body nothing could send and each button 404'd on click — a
 * control that cannot work, which is worse than its absence. They are not
 * oversights waiting for a handler:
 *
 * - STARTING A PERSON is chiefd's roster decision. chiefd converges the host
 *   from the durable roster, and a browser that could start one person out of
 *   band would be a second opinion about who is up.
 * - A FRESH SESSION and a COMPACTION are durable maintenance protocols in
 *   chiefd, not single calls. Modelling either as one POST is what made them
 *   look like missing handlers in the first place.
 *
 * Forward-only: nothing here is disabled, deprecated or shimmed. If any of the
 * three ever becomes this server's business, it arrives as a route first and a
 * schema transcribed from that route second — the order this whole file
 * exists to enforce. */

// ---- staffing (the org-structure verbs) -----------------------------------
//
// WHAT THE BROWSER SENDS, AND WHAT IT DOES NOT
//
// These request types carry INTENT only — who, where, what they are for. That
// is the mandate-3 line: the browser states what the operator asked for, and
// the layers below decide everything structural. There is no route in any of
// them, because there is no route to choose: every agent boots as plain Pi on
// the operator's own defaults.

/** `POST /companies/:companyKey/departments`. `parentId` absent means the root.
 *
 * A plain type, not a zod schema — exactly what this file's header says a
 * request body this service CONSTRUCTS should be. It carried a schema that
 * nothing ever called `.parse` on; the object existed only to be read back
 * out by `z.infer`, so its `.min(1)` rules validated nothing at runtime and
 * read as a promise the code does not keep. */
export interface CreateDepartmentRequest {
  name: string
  purpose: string
  /** Required, not nullish: chiefd's create takes a parent, and a department
   * with none is not a shape it will build. */
  parentId: string
  /** The department's head, hired with it. A department without a head is not
   * a shape chiefd will create.
   *
   * No `title`: chiefd derives it as `Head of <name>` from the unit's name
   * (`mint_department_create_ids`), and a title typed here could only disagree
   * with the one every other head in the company got. */
  head: {
    name: string
    mandate: string
  }
}

export const CreateDepartmentResponseSchema = z.object({
  applied: z.literal(true),
  departmentId: z.string()
})
export type CreateDepartmentResponse = z.infer<typeof CreateDepartmentResponseSchema>

/** `POST /companies/:companyKey/people/hire`. A plain type for the same reason
 * `CreateDepartmentRequest` is one. */
export interface HirePersonRequest {
  departmentId: string
  name: string
  title: string
  mandate: string
}

export const HirePersonResponseSchema = z.object({ applied: z.literal(true) })
export type HirePersonResponse = z.infer<typeof HirePersonResponseSchema>

/** The shape every staffing verb answers with. */
export const StaffingAppliedSchema = z.object({ applied: z.literal(true) })
export type StaffingApplied = z.infer<typeof StaffingAppliedSchema>

/** `POST …/departments/:departmentId/pause` and `…/resume`.
 *
 * A UNION, because chiefd's answer is one. Both routes are pass-throughs —
 * `routeResult(() => chiefd.staffing.pauseDepartment(companyKey, departmentId))` —
 * and `Staffing.pauseDepartment`/`resumeDepartment` resolve `chiefing`'s
 * `AtomicDirectOutcome`, which is `{applied: true} | {refused, detail}`. So a
 * REFUSAL ARRIVES AS A 200 WITH A BODY, not as a thrown error and not as a
 * non-2xx status: `directOutcome` decodes chiefd's own 422 into a value
 * (`decodeRefusal`, "never thrown, never retried") and `NextResponse.json`
 * serializes that value with a 200.
 *
 * Reusing `StaffingAppliedSchema` here would therefore have thrown a ZodError
 * on every refusal the operator most needs to read — pausing the executive
 * root answers `exec-root-protected`, an unknown department id answers its own
 * code — and the resulting message would have named zod, not chiefd. That is
 * the standing failure mode this file was rewritten to end: a client schema
 * that disagrees with its route passes both halves' tests and fails only in a
 * browser.
 *
 * Note what this does NOT do: fold the refusal into the error taxonomy at the
 * client. `ChiefApiClientService` reports what the route said; the caller
 * decides what a refusal means for its own surface — see `StructureRail`. */
export const DepartmentStateChangeResponseSchema = z.union([
  z.object({ applied: z.literal(true) }),
  z.object({ refused: z.string(), detail: z.string() })
])
export type DepartmentStateChangeResponse = z.infer<typeof DepartmentStateChangeResponseSchema>

// ---- client construction ---------------------------------------------------

/** `ChiefApiClientService`'s constructor options. Base URL and token
 * provider are injected — the service reads no environment variables itself
 * and never constructs any address but this one (rulings D1/D2). */
export interface ChiefApiClientOptions {
  /** apps/api's base URL — `publicChiefApiUrl()`/`chiefApiUrl()` at the call
   * site. The only address this service ever holds. */
  baseUrl: string
  /** In-memory token getter. Absent/`null` sends no Authorization header
   * (auth-off mode). */
  accessToken?: () => string | null
  /** Test seam (FakeChiefApi). */
  fetchImpl?: FetchImpl
  /** Invoked once on a 401 before the single retry; typically the session
   * provider's `refresh()`. Absent → a 401 surfaces immediately. */
  onUnauthorized?: () => Promise<void>
}

/** `POST /api/session`'s response body — what `SessionClientService`
 * returns to `ApiSessionProvider`. */
export interface SessionAcquireResult {
  /** `null` in auth-off mode (no operator key configured). */
  token: string | null
  identityId: string
}
