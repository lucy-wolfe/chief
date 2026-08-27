import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { truncateToWidth, visibleWidth } from "@earendil-works/pi-tui";
import {
  CARD_EXPAND_HINT_TEXT,
  domainIcon,
  renderCard as renderSharedCard,
  type CardSpec,
  type CardTheme,
  type RenderCardOptions,
} from "./card-style";
import {
  ACTIVITY_STATUS_KEY,
  WORKING_LABEL,
  createActivityStatusLine,
} from "./organization-activity-status";
import {
  paneChiefdTransport,
  paneTokenManager,
  postOrgRoute,
  readDaemonRendezvous,
  subscribeSse,
  FetchTransport,
  type PaneIdentity,
  type SseChannelState,
  type SseDocChangeEvent,
  type SseWatcherOptions,
} from "@chief/chiefing/extension-runtime";
import { AttachedInputTracer } from "./attached-input-observability";

/** The minimal `SseWatcher` surface this file itself calls — a test seam can substitute anything shaped like this. */
export interface SseWatcherLike {
  close(): void;
}

export interface TeamUiOptions {
  /** Test seam: substitutes the real `SseWatcher` construction (#276) with a fake conforming to {@link SseWatcherLike}. Production omits this and gets a real `new SseWatcher(...)`. */
  createSseWatcher?: (watcherOptions: SseWatcherOptions) => SseWatcherLike;
  /** Test seam: clock for the #336 schedule countdowns/fire-detection recency check and the #337 settle/shutdown countdown. Defaults to `Date.now`. */
  now?: () => number;
  /** #361: schedules the footer's slow safety-net floor tick (replaces the plain path's old blind 1s `setInterval`). Defaults to `setInterval`.
   * The opaque return is a timer handle (`NodeJS.Timeout`/`number` by runtime)
   * retained only for the matching `clearFloorTimer` call below. */
  // eslint-disable-next-line lucy/no-unknown-callback-return -- opaque timer handle consumed by clearFloorTimer, not a discarded promise.
  createFloorTimer?: (fn: () => void, ms: number) => unknown;
  /** #361: cancels a scheduled floor tick. Defaults to `clearInterval`. */
  clearFloorTimer?: (handle: unknown) => void;
  /** Test seam: how long `session_start` waits for the install-time org-activity gather before letting the footer paint provisionally. Defaults to {@link FOOTER_FIRST_FRAME_PRIME_BUDGET_MS}. */
  firstFramePrimeBudgetMs?: number;
}


/** The Team UI's single card boundary. The footer itself remains a separate
 * status composition surface; only Pi message/entry cards enter here. */
export function renderTeamUiCard(
  theme: CardTheme,
  spec: CardSpec,
  options: RenderCardOptions = {},
) {
  return renderSharedCard(theme, spec, options);
}

const renderCard = renderTeamUiCard;

export interface TeamIdentity {
  team: string;
  /**
   * The FUNCTIONAL key. For an organization pane this is the person's kebab
   * id, and document-store paths, activity and the mailbox are all addressed
   * by it. Never render it.
   */
  role: string;
  /**
   * The USERNAME, and the only thing the footer shows. Present for an
   * organization identity, absent for the Founder, whose `role` is already a
   * handle rather than a key.
   */
  handle?: string;
}

interface OrganizationRosterPerson {
  id?: string;
  departmentId?: string;
  employmentState?: string;
}

interface OrganizationRosterDepartment {
  parentDepartmentId?: string;
  headPersonId?: string;
}

export interface SessionUsageTotals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cost: number;
  cacheHitRate?: number;
}

/**
 * #378: renders a remaining-time span as a live-ticking clock — `Xm YYs`
 * once a minute is on the clock, plain `Ys` under a minute, seconds always
 * zero-padded to two digits so the display reads like a real countdown
 * clock rather than jittering width every tick. `remainingMs <= 0` renders
 * `pastDueLabel` instead of a misleading negative countdown.
 *
 * No minute cap: an hours-long remainder (e.g. 90m) still renders as plain
 * minutes (`90m 00s`) rather than rolling into an `h` bucket — schedules on
 * this footer are always minutes-to-low-hours, so a third unit would add
 * width without adding legibility.
 */
function formatCountdownClock(remainingMs: number, pastDueLabel: string): string {
  if (remainingMs <= 0) return pastDueLabel;
  const totalSeconds = Math.ceil(remainingMs / 1_000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}m ${String(seconds).padStart(2, "0")}s` : `${seconds}s`;
}

/**
 * Read a durable org document straight from chiefd's typed docstore.
 *
 * #121 moved every durable org document into the SQL durable store and stopped
 * writing `state/supervision.json`; reading that dead file returned `undefined`
 * forever, so the footer's reminder count silently went
 * blank in production while their tests stayed green against a file they wrote
 * themselves. The authority is now normalized SQL rows served by chiefd's
 * `/v1/org/*` routes, and this reads it.
 *
 * team-ui.ts is COPIED verbatim into every person's pi-home (see
 * `launcherExtensionSources`), so — like `organization-intercom.ts` — it may not
 * import launcher `src/`. This reader is therefore a self-contained twin of the
 * ones in `organization-intercom.ts` and `src/organization/org-durable-store.ts`;
 * the key derivation MUST stay byte-compatible across all three.
 */

/**
 * Thrown when the call carries no chiefd base URL — that is, the footer was
 * installed without {@link resolveTeamUiCompany} having answered for THIS
 * pane's company.
 *
 * There is no fixed-port fallback and no ambient re-read (ruling D0/D1):
 * guessing an address, or picking up whatever address the process happens to
 * hold, risks talking to another company's daemon — which answers.
 */
export class OrgChiefdUrlUnsetError extends Error {
  readonly name = "OrgChiefdUrlUnsetError" as const;
  constructor() {
    super(
      "this footer carries no chiefd base URL; it must be read from its own company directory's .chief/run/daemon.json before any docstore call can be made",
    );
  }
}

/** THIS pane's company: where its daemon is listening, and what that company
 * is called on the wire. Both come from ONE local read, so a footer can never
 * hold an address for one company and a key for another. */
export interface TeamUiCompany {
  /** The company daemon's own bound base URL. */
  readonly url: string;
  /** `sha256(<dir>)[..12]` — the `slug` every chiefd route resolves by. */
  readonly key: string;
}

/**
 * THE ONE PLACE THIS EXTENSION LEARNS WHERE ITS COMPANY'S DAEMON IS, AND WHAT
 * ITS COMPANY IS CALLED ON THE WIRE.
 *
 * The address first arrived as `ORG_CHIEFD_URL`, one process-global
 * environment variable stamped into a tmux pane by the chiefd that spawned it.
 * That is right for exactly one deployment — one Pi process per pane, one
 * company per process — and has no correct value at all in a host that serves
 * several companies from ONE process, where the failure is SILENT: a wrong
 * daemon ANSWERS and the footer paints another company's reminder count as this
 * person's own.
 *
 * It then became a beacond lookup by SLUG, which fixed that and kept a subtler
 * version of it: two directories may hold companies with the same display
 * word, and the registry had one answer for the word.
 *
 * A pane's cwd IS its company directory, and a directory knows where its own
 * daemon is: `chiefd` publishes `<dir>/.chief/run/daemon.json` carrying the
 * URL it bound AND the key it serves. One local file read — no registry on the
 * path between a pane and its own company, and no question whose answer could
 * be about a different one. The file names the directory it describes, so a
 * copied project's rendezvous is refused rather than followed
 * (`parseDaemonRendezvous`).
 *
 * Resolved once per footer install and then CARRIED into every read below,
 * because a footer belongs to one company for its whole life; per-company is
 * the property that was missing, not per-call.
 *
 * A PLAIN (non-organization) pane has no company at all — no identity, no
 * company dir — so it resolves to `undefined` and reads nothing. So does a
 * pane whose company has no daemon running. Neither is a fallback: it is the
 * absence of a subject, and any read attempted from that state refuses with
 * {@link OrgChiefdUrlUnsetError}.
 */
export function resolveTeamUiCompany(
  environment: Record<string, string | undefined> = process.env,
): TeamUiCompany | undefined {
  const identity = organizationFooterIdentity(environment);
  const companyDir = environment.ORG_LAUNCHER_ORG_DIR?.trim();
  if (!identity || !companyDir) return undefined;
  const rendezvous = readDaemonRendezvous(companyDir);
  return rendezvous ? { url: rendezvous.url, key: rendezvous.key } : undefined;
}

/** The address a read travels on, or a refusal — never a guess. */
function requiredChiefdUrl(chiefdUrl: string | undefined): string {
  const value = chiefdUrl?.trim();
  if (!value) throw new OrgChiefdUrlUnsetError();
  return value;
}

/**
 * The pane this footer is painted in, as a chiefd identity — or `undefined`
 * when this is not a company pane (a Founder or plain Pi, which has no durable
 * footer state to read and therefore nothing to authenticate for).
 *
 * The launch catalog also carries `ORG_LAUNCHER_IDENTITY_DIR`, because the
 * Chief's key is directly under `.chief` while agents keep theirs in their
 * agent directories.
 */
export function teamUiPaneIdentity(
  url: string,
  environment: Record<string, string | undefined> = process.env,
): PaneIdentity | undefined {
  const identity = organizationFooterIdentity(environment);
  const organizationDir = environment.ORG_LAUNCHER_ORG_DIR?.trim();
  const identityDir = environment.ORG_LAUNCHER_IDENTITY_DIR?.trim();
  if (!identity || !organizationDir || !identityDir) return undefined;
  return { url, personId: identity.role, organizationDir, identityDir };
}

/**
 * A4: the footer's reads used to travel on a bare `new FetchTransport(url)` —
 * no credential at all — although this extension runs INSIDE a pane that
 * already holds a working identity key and beside an extension that was
 * already using it. It now routes through the ONE shared pane acquirer
 * (`@chief/chiefing`'s `paneChiefdTransport`), inheriting the key, the token
 * cache and the re-acquire-on-401 retry rather than growing a second copy of
 * any of the three.
 *
 * A pane that is not a company pane still gets a plain transport: it has no
 * person, so there is no identity to sign as, and inventing one would present
 * an unenrolled credential instead of no credential.
 */
function chiefdTransport(url: string): FetchTransport {
  const identity = teamUiPaneIdentity(url);
  return identity ? paneChiefdTransport(identity) : new FetchTransport(url);
}

/**
 * The credential the footer's SSE reader presents, or `undefined` on a pane
 * with no person to be.
 *
 * The SAME manager the transport above uses — `paneTokenManager` is keyed on
 * (daemon, person, key) and returns the cached instance — so the footer's
 * stream and the footer's reads share one token, and a re-authentication on
 * either side is seen by both.
 */
export function teamUiSseBearer(
  url: string,
  environment: Record<string, string | undefined> = process.env,
): ReturnType<typeof paneTokenManager> {
  const identity = teamUiPaneIdentity(url, environment);
  return identity ? paneTokenManager(identity) : undefined;
}

/**
 * PERF #30 Stage 1: the async transport for every footer/org-doc read in this
 * file. `@chief/chiefing/extension-runtime`'s `FetchTransport` already owns
 * the connect-refusal retry ladder (a chiefd hand-off briefly making its
 * listener unavailable while every Pi pane stays alive) — this file must not
 * layer a second retry on top of it (never double-retry the same failure
 * class). A transport/network failure throws `ChiefdUnavailableError`; a
 * 422/404/400 refusal throws `OrgRowRefusalError` — both from the shared
 * `postOrgRoute` decoder, never a private parser.
 */
export async function chiefdPostJsonAsync<T>(url: string, path: string, body: unknown): Promise<T> {
  return postOrgRoute<T>(chiefdTransport(url), url, path, body);
}

/**
 * The outcome of one normalized live read. The union retains the historical
 * conditional-read shape for the footer cache; dedicated row routes currently
 * return a full aggregate, so normalized readers use the `unchanged:false` arm.
 */
type ConditionalReadResult =
  | { unchanged: true; seq: number }
  | { unchanged: false; value: unknown; seq: number | undefined };

/**
 * Async twin of `chiefdReadDocument` over the dedicated row routes. The
 * `ifSeqNot` argument is the conditional-read cursor;
 * row reads always return their current aggregate and seq.
 */
async function chiefdReadDocumentAsync(
  url: string, key: string, storeName: string, ifSeqNot?: number,
): Promise<ConditionalReadResult> {
  if (storeName === "supervision" || storeName === "activity") {
    return readNamedOrgRowDocumentAsync(
      url,
      `/v1/org/${storeName}/read`,
      { slug: key },
      "ledger",
      ifSeqNot,
    );
  }
  if (storeName.startsWith("mailbox/")) {
    return readNamedOrgRowDocumentAsync(
      url,
      "/v1/org/mailbox/read-person",
      { slug: key, personId: storeName.slice("mailbox/".length) },
      "mailbox",
      ifSeqNot,
    );
  }
  throw new Error(`team-ui has no normalized reader for '${storeName}'`);
}

/** Async twin of `readDurableDocument`: absence → `undefined`, unreachable → throws. */
async function readDurableDocumentAsync(
  chiefdUrl: string | undefined, companyKey: string, storeName: string, ifSeqNot?: number,
): Promise<ConditionalReadResult> {
  return chiefdReadDocumentAsync(requiredChiefdUrl(chiefdUrl), companyKey, storeName, ifSeqNot);
}

/** Read one normalized aggregate, mapping its named payload field into the
 * footer's `ConditionalReadResult`.
 * Absence -> `value: undefined`; the `seq` is the cache cursor. */
async function readNamedOrgRowDocumentAsync(
  url: string,
  path: string,
  body: Record<string, unknown>,
  payloadField: "document" | "ledger" | "mailbox",
  ifSeqNot?: number,
): Promise<ConditionalReadResult> {
  const parsed = await chiefdPostJsonAsync<Record<string, unknown> & {
    found: boolean;
    seq?: number;
    unchanged?: boolean;
  }>(
    url,
    path,
    ifSeqNot === undefined ? body : { ...body, ifSeqNot },
  );
  if (parsed.unchanged && typeof parsed.seq === "number") {
    return { unchanged: true, seq: parsed.seq };
  }
  const payload = parsed[payloadField];
  if (!parsed.found || typeof payload !== "string") {
    return { unchanged: false, value: undefined, seq: parsed.seq };
  }
  return { unchanged: false, value: JSON.parse(payload), seq: parsed.seq };
}

/**
 * Read the normalized structural manifest directly — there is no on-disk
 * fallback. A read failure must never turn a live SQL company into an
 * apparently healthy zero-reminder footer.
 */
async function readOrganizationManifestAsync(
  chiefdUrl: string | undefined,
  companyKey: string,
): Promise<unknown> {
  const wire = await chiefdPostJsonAsync<{ found: boolean; manifest?: string }>(
    requiredChiefdUrl(chiefdUrl), "/v1/org/manifest/read", { slug: companyKey },
  );
  if (!wire.found || wire.manifest === undefined) return undefined;
  return JSON.parse(wire.manifest);
}

/**
 * #34: last-good value per store, so an SSE-driven refresh re-reads ONLY the
 * store whose `doc-change` event woke it.
 *
 * The footer gather reads three documents (supervision, activity,
 * mailbox/<self>); before this, ANY doc-change on ANY of them re-read all
 * three — two wasted round trips (and two full document bodies, the
 * supervision ledger being the multi-megabyte one) per event, on every staffed
 * pane. A refresh that names the changed store now serves the other two from
 * this cache.
 *
 * A refresh that names NOTHING (mount prime, the poll floor, a `reorg` resync,
 * a channel going dead) still re-reads everything — the cache is an
 * optimization on a KNOWN-narrow wake, never a substitute for a full resync.
 * A failed read leaves the previous value cached rather than poisoning it with
 * `undefined`, matching the gather's own last-good-snapshot behaviour.
 */
const footerStoreDocumentCache = new Map<string, { value: unknown; readAt: number; seq: number | undefined }>();

/**
 * The cache's staleness ceiling. #827 deleted the SSE fallback poll floor
 * this used to share its budget with (`ORG_SSE_POLL_FLOOR_MS`); the value is
 * kept as an independent literal — deleting the floor does not change the
 * reasoning below for why a store-scoped read cache still needs a ceiling.
 *
 * Store-scoped reads are correct only while every change produces an event we
 * actually receive. A dropped frame, a reconnect gap, or a store that simply
 * stops emitting would otherwise pin a document to its last-good value
 * FOREVER, and the footer would render a silently-wrong deadline — the #63
 * failure mode (a countdown frozen at "due" while everything looks healthy).
 * Before #34 that was repaired by ACCIDENT: any event on any store re-read all
 * five. So the guarantee is made explicit instead — a cached document older
 * than this is re-read on the next refresh whatever the event named, with no
 * timer of its own and without giving up the win (a busy channel still
 * re-reads exactly one store per event).
 *
 * NOTE (#827 plan, flagged rather than silently resolved): the issue body's
 * step 3 says this constant "stops being defined as the poll floor and
 * becomes FOOTER_STALE_AFTER_MS ... used only to decide whether to render
 * the stale marker." That does not match this file's actual "⚠ stale" tag,
 * which is driven entirely by `supervisionStale`/`supervisionStaleAfterMs`/
 * `SUPERVISION_STALE_MS` (a distinct 30-minute constant, see below) and was
 * never wired to this one. Renaming without rewiring is what is done here;
 * rewiring this value into the visible stale-marker decision would be new
 * behavior this plan cannot verify without a working build. Flagged for
 * architect2.
 */
const FOOTER_STALE_AFTER_MS = 60_000;

/** TEST-ONLY seam: the clock the cache ages its entries against. */
let footerStoreClock: () => number = () => Date.now();

/** TEST-ONLY: drives cache ageing without waiting a real minute. */
export function setFooterStoreClockForTest(clock: (() => number) | undefined): void {
  footerStoreClock = clock ?? (() => Date.now());
}

export async function readFooterStoreDocument(
  chiefdUrl: string | undefined,
  companyKey: string,
  storeName: string,
  changedStores: ReadonlySet<string> | undefined,
): Promise<unknown> {
  // Keyed by COMPANY and store. The company half used to be `slug|dataRoot`,
  // the two inputs the composite key was built from; one served key says the
  // same thing and cannot be assembled two ways.
  const key = `${companyKey}|${storeName}`;
  const cached = footerStoreDocumentCache.get(key);
  const fresh = cached !== undefined && footerStoreClock() - cached.readAt < FOOTER_STALE_AFTER_MS;
  if (changedStores && !changedStores.has(storeName) && fresh) {
    return cached.value;
  }
  // #149/#10: probe with the seq we already hold so an unchanged live
  // document (the multi-MB supervision/activity ledgers) is a cheap server-side
  // seq probe, not a re-serialize. Do this ONLY on a BLIND resync (no
  // named change set — the mount prime, the poll floor, a `reorg`) and only
  // within the staleness bound. An explicit doc-change for THIS store, or a
  // stale entry, always full-reads: the probe must never override a KNOWN
  // change, and — because a row seq can RESET on a drop+recreate — a
  // stale-but-equal seq could otherwise serve a stale value. Bounding
  // the probe to a fresh, unnamed resync caps that risk at the same max-age the
  // skip branch above already allows. (The idle-CPU win lives on exactly this
  // blind poll-floor path.)
  const probeSeq = (!changedStores && fresh) ? cached?.seq : undefined;
  const result = await readDurableDocumentAsync(chiefdUrl, companyKey, storeName, probeSeq);
  if (result.unchanged) {
    footerStoreDocumentCache.set(key, { value: cached!.value, readAt: footerStoreClock(), seq: result.seq });
    return cached!.value;
  }
  footerStoreDocumentCache.set(key, { value: result.value, readAt: footerStoreClock(), seq: result.seq });
  return result.value;
}

/** Count the pending VIEW in chiefd's normalized mailbox snapshot. A row in
 * `delivered` remains pending until the converge-owned terminal settlement,
 * exactly as the intercom and launcher mailbox projections define it. */
export function countPendingMailboxEntries(snapshot: unknown): number {
  const entries = (snapshot as { entries?: unknown } | undefined)?.entries;
  if (!Array.isArray(entries)) return 0;
  return entries.filter((entry) => {
    const state = (entry as { state?: unknown } | undefined)?.state;
    return state === "pending" || state === "delivered";
  }).length;
}

/** TEST-ONLY: drops every cached store document (a fresh pane starts empty). */
export function resetFooterStoreDocumentCache(): void {
  footerStoreDocumentCache.clear();
}

/** chiefd typed read of one document key: parsed blob, or `undefined` when absent. */
async function chiefdReadDocument(url: string, key: string, storeName: string): Promise<unknown> {
  if (storeName !== "supervision") {
    throw new Error(`team-ui has no normalized reader for '${storeName}'`);
  }
  const parsed = await postOrgRoute<{ found: boolean; ledger?: string }>(
    chiefdTransport(url), url, "/v1/org/supervision/read", { slug: key },
  );
  if (!parsed.found || parsed.ledger === undefined) return undefined;
  return JSON.parse(parsed.ledger);
}

/**
 * The current durable document, `undefined` when the company has no such row.
 * A genuine absence returns `undefined`; an unreachable docstore THROWS — the
 * two must stay distinguishable so absence never masquerades as a healthy
 * failure. chiefd's typed read returns the whole blob in one call and has no
 * cheap seq-probe route, so this reads and parses each call.
 */
async function readDurableDocument(chiefdUrl: string | undefined, companyKey: string, storeName: string): Promise<unknown> {
  return chiefdReadDocument(requiredChiefdUrl(chiefdUrl), companyKey, storeName);
}

/** Read only durable schedules that belong to this Pi; never infer work from a footer count. */
interface OrganizationFooterActivity {
  /**
   * This viewer's own ARMED durable reminders, counted off the supervision
   * ledger. `undefined` when the authority could not be read — never a
   * healthy-looking 0.
   *
   * This is the one scheduled-work figure the footer shows, and it is read
   * from exactly one authority: chiefd's supervision ledger. It is never
   * merged with a count from anywhere else — an operator who sees this number
   * drop must be able to name what vanished, which a figure summed from two
   * authorities can never tell them.
   */
  reminders?: number;
  /**
   * #529: the supervision authority the reminder count derives
   * from is DETECTABLY STALE — its `updatedAt` lags the expected protected-
   * schedule cadence (see `SUPERVISION_STALE_MS`). This is the frozen/off
   * org_documents-mirror shape that rendered a live count for 3 days off a
   * dead doc. When set, the footer renders that field
   * NOT-FRESH (dimmed + a "⚠ stale" tag), never as a healthy live value —
   * "never present stale data as fresh".
   */
  supervisionStale?: boolean;
  /**
   * The last native supervision authority stamp observed by the async gather.
   * Kept with the rendered snapshot so the synchronous footer can continue to
   * age it even while a connected-but-frozen SSE reader emits no event.
   */
  supervisionUpdatedAt?: string;
  /** When this viewer's pane goes away: `idleSince` + the whole settle window,
   * while it is idle and still desired-active. ONE deadline — settling and
   * shutting down are the same countdown now, so the number the operator reads
   * is the number of seconds the agent has left. */
  settleShutdownDeadlineAt?: string;
  /** #44: how many envelopes sit in this viewer's own `mailbox/<personId>`
   * pending bucket right now. 0 (absent mailbox, or an unreachable/corrupt
   * read) hides the footer's 📬 segment entirely — it never shows a stale or
   * guessed count. */
  pendingMailboxCount?: number;
}

/**
 * #529: how old the supervision authority's `updatedAt` may be before the
 * footer treats its reminder count as STALE (frozen/off mirror)
 * rather than live. 2× the 15-minute protected-schedule cadence
 * (`managerCheckInIntervalMs`): a healthy company re-arms — and thus rewrites —
 * the ledger at least once per interval, so a doc untouched for 30 minutes is
 * the frozen mirror, not a legitimately idle one. Deliberately generous so an
 * ordinarily-idle CEO is never mislabeled; it exists to catch the days-stale
 * live bug, not to police normal quiet.
 */
const SUPERVISION_STALE_MS = 30 * 60_000;
const TEAM_LAUNCHER_E2E_ENV = "TEAM_LAUNCHER_E2E";
const TEAM_LAUNCHER_E2E_SUPERVISION_STALE_MS_ENV = "TEAM_LAUNCHER_E2E_SUPERVISION_STALE_MS";
const TEAM_LAUNCHER_E2E_FOOTER_REPAINT_MS_ENV = "TEAM_LAUNCHER_E2E_FOOTER_REPAINT_MS";

type FooterEnvironment = Readonly<Record<string, string | undefined>>;

/**
 * The short horizon is deliberately unavailable to normal processes. A real
 * pane only accepts it when the E2E harness supplies both the explicit test
 * sentinel and a bounded integer; production therefore retains the 30-minute
 * safety boundary even if an operator happens to carry the duration variable.
 */
function e2eDurationOverride(
  environment: FooterEnvironment,
  variable: string,
  maximumMs: number,
): number | undefined {
  if (environment[TEAM_LAUNCHER_E2E_ENV] !== "1") return undefined;
  const raw = environment[variable];
  if (raw === undefined || !/^\d+$/.test(raw)) return undefined;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= 1_000 && value <= maximumMs ? value : undefined;
}

/** The production 30-minute boundary, or a sentinel-gated E2E horizon. */
export function supervisionStaleAfterMs(environment: FooterEnvironment = process.env): number {
  return e2eDurationOverride(environment, TEAM_LAUNCHER_E2E_SUPERVISION_STALE_MS_ENV, SUPERVISION_STALE_MS)
    ?? SUPERVISION_STALE_MS;
}

/**
 * A matching E2E-only repaint cadence keeps the real Pi proof near its
 * one-minute stale horizon. Normal panes retain the shared 60-second
 * render-clock tick (#827: zero I/O — see the org-pane branch below); the
 * override changes no fetch policy — healthy SSE still only redraws the
 * cached footer. Name kept as-is (not renamed to match #827's
 * FOOTER_STALE_AFTER_MS) to avoid an unforced dangling-import churn against
 * tests/footer-supervision-staleness.test.ts, which imports this symbol and
 * is owned by a different story (E9-S4, disposition port:piing) — see
 * the design record.
 */
export function footerRepaintFloorMs(environment: FooterEnvironment = process.env): number {
  return e2eDurationOverride(environment, TEAM_LAUNCHER_E2E_FOOTER_REPAINT_MS_ENV, FOOTER_STALE_AFTER_MS)
    ?? FOOTER_STALE_AFTER_MS;
}

/**
 * #529: is the supervision authority the footer just read DETECTABLY STALE — a
 * frozen/off org_documents mirror? True only when it carries a parseable
 * `updatedAt` that lags `nowMs` by more than `SUPERVISION_STALE_MS`. A doc that
 * could not be read (undefined) or whose `updatedAt` is missing/malformed is
 * NOT stale here — that is the separate "unknown" treatment; this predicate
 * fires only on a doc that is present, readable, and provably old. Pure +
 * exported so the freshness contract is unit-lockable without the two-store
 * harness a real frozen mirror needs (see #411).
 */
export function supervisionDocIsStale(
  ledger: unknown,
  nowMs: number,
  staleAfterMs = supervisionStaleAfterMs(),
): boolean {
  const updatedAt = (ledger as { updatedAt?: unknown } | undefined)?.updatedAt;
  const updatedAtMs = typeof updatedAt === "string" ? Date.parse(updatedAt) : Number.NaN;
  return Number.isFinite(updatedAtMs) && nowMs - updatedAtMs > staleAfterMs;
}

/**
 * #639: footer snapshots refresh reactively. A frozen reader can keep its SSE
 * connection healthy forever while serving the same supervision timestamp, so
 * no new gather arrives to recompute the stale bit. Preserve the observed
 * timestamp and re-evaluate its age at render time, alongside the countdowns
 * that already tick from the render clock. The fallback retains the gathered
 * result for a legacy/test snapshot that has no timestamp field.
 */
export function supervisionSnapshotIsStale(
  gatheredStale: boolean | undefined,
  updatedAt: string | undefined,
  nowMs: number,
  staleAfterMs = supervisionStaleAfterMs(),
): boolean {
  return updatedAt === undefined
    ? gatheredStale === true
    : supervisionDocIsStale({ updatedAt }, nowMs, staleAfterMs);
}

/**
 * THE WHOLE SETTLE WINDOW, mirroring chiefd `store/activity.rs`
 * `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`: from `idleSince` to the pane being
 * gone, and nothing is added after it. The operator's "2min max from settle".
 *
 * THIS COPY WAS WRONG, and was the whole behaviour rather than one term in a
 * sum: it said 60s -- half the Rust authority -- under a comment that asserted
 * 60s, cited a TypeScript module that no longer exists, and instructed the
 * reader to keep the two in sync by hand. It is now the authority's value, and
 * the agreement is no longer a promise: `scripts/test/
 * settle-budget-single-definition.test.mjs` reads BOTH definitions and fails if
 * they ever disagree.
 *
 * TOMBSTONE: `SETTLE_HANDOFF_GRACE_MS` and `SETTLE_FORCE_KILL_GRACE_MS` stood
 * beside this, and a second countdown added one of them to the transition's own
 * `handoffDeadlineAt`. That grace is deleted in chiefd -- a routine idle park is
 * minted already terminal -- so there is no second deadline to render and no
 * constant left at zero to be "fixed" back to two minutes.
 */
// 2026-08-24, operator ruling: *"lets bump the 2mins to a 5mins."* The Rust
// authority moved and this copy moves with it in the same commit --
// `settle-budget-single-definition.test.mjs` reads both and fails on
// disagreement, which is why this is a value and not a promise.
const SETTLE_QUIET_LEASE_MS = 300_000;

/**
 * The settle countdown -- ticks purely from the footer's own repaint
 * cadence (no new timer, no new disk read), exactly like #336/#378's schedule
 * countdowns, and shares the same `Xm YYs` live-clock formatting. Clamped:
 * past the deadline renders "now", never a negative countdown (the local
 * pane does not force-kill itself; the next reconcile pass does).
 */
export function formatSettleCountdown(deadlineIso: string | undefined, nowMs: number, pastLabel = "now"): string | undefined {
  if (!deadlineIso) return undefined;
  const deadlineMs = Date.parse(deadlineIso);
  if (!Number.isFinite(deadlineMs)) return undefined;
  return formatCountdownClock(deadlineMs - nowMs, pastLabel);
}

/** One render reads each displayed authority once, so the reminder count and the mailbox count cannot disagree mid-footer.
 * PERF #30 Stage 1: async so the four durable reads go through in-process
 * `fetch` (`readDurableDocumentAsync`) instead of blocking `spawnSync("curl")`.
 * Runs off the render thread on the reactive refresh cadence; the render
 * callback only ever reads the cached snapshot this produces. */
async function persistedOrganizationFooterActivity(
  company: TeamUiCompany | undefined,
  environment: Record<string, string | undefined>,
  /**
   * #34: when present, only these stores are re-read; every other store is
   * served from its last-good cached document. Absent (mount prime, poll floor,
   * reorg resync, channel-dead) means a full re-read of all five.
   */
  changedStores?: ReadonlySet<string>,
): Promise<OrganizationFooterActivity> {
  const identity = organizationFooterIdentity(environment);
  if (!identity || !company) return {};
  const { url: chiefdUrl, key: companyKey } = company;

  // Supervision is SQL-only since #121; a missing row or an unreachable write
  // service both leave every field derived below unknown, never a
  // healthy-looking default. Read once so the reminder count
  // countdowns, and the #336 fire-card detail all agree with one observation.
  let supervisionLedger: unknown;
  try {
    supervisionLedger = await readFooterStoreDocument(chiefdUrl, companyKey, "supervision", changedStores);
  } catch {
    supervisionLedger = undefined;
  }

  // #529: detect a DETECTABLY-STALE supervision authority — the frozen/off
  // org_documents-mirror shape. A healthy company's protected schedules re-arm
  // (and thus write the ledger) at least every `managerCheckInIntervalMs()`, so
  // an `updatedAt` older than `SUPERVISION_STALE_MS` (2× that cadence) means the
  // served doc is frozen (mirror off / not being written) — the 3-day-stale
  // footer bug. Marked here so the render treats every field derived below as
  // NOT-FRESH rather than painting healthy live values off a dead doc.
  const supervisionUpdatedAt = typeof (supervisionLedger as { updatedAt?: unknown } | undefined)?.updatedAt === "string"
    ? (supervisionLedger as { updatedAt: string }).updatedAt
    : undefined;
  const supervisionStale = supervisionSnapshotIsStale(undefined, supervisionUpdatedAt, footerStoreClock());


  let reminders: number | undefined;
  try {
    // Counted off the SAME supervision read every other field above uses, so
    // reminders agree with the same observation. Only this viewer's own,
    // matching the 📬 mailbox count's per-viewer scoping: a person's footer
    // answers "what is scheduled for ME".
    const ledger = supervisionLedger as {
      reminders?: Record<string, { personId?: unknown; status?: unknown; expiresAt?: unknown }>;
    } | undefined;
    if (ledger?.reminders && typeof ledger.reminders === "object") {
      const now = Date.now();
      reminders = Object.values(ledger.reminders).filter((row) => {
        if (!row || typeof row !== "object") return false;
        if (row.personId !== identity.role || row.status !== "active") return false;
        // An expiry we cannot parse fails CLOSED (not armed), mirroring
        // chiefd's own `Reminder::is_armed`. The two counts must agree, or the
        // footer and the daemon are telling the operator different stories.
        if (typeof row.expiresAt === "string") {
          const expiry = Date.parse(row.expiresAt);
          if (!Number.isFinite(expiry) || expiry <= now) return false;
        }
        return true;
      }).length;
    } else if (supervisionLedger && typeof supervisionLedger === "object") {
      // The ledger read fine and simply has no reminders collection (a company
      // predating the feature, or one where nobody has armed anything). That is
      // an honest zero, distinct from the unreadable case below.
      reminders = 0;
    }
  } catch {
    // Unreadable stays `undefined` and renders "unknown" — the whole point of
    // this feature was that a state nobody could know rendered as a confident 0.
  }

  let settleShutdownDeadlineAt: string | undefined;
  try {
    // THE SETTLE COUNTDOWN, and there is only one of it.
    //
    // The activity ledger's own durable authority for this viewer. `idleSince`
    // is stamped the moment settling begins and the pane is gone one quiet
    // lease later, so this single deadline IS the shutdown instant -- not a
    // phase's remainder, and never a figure above the two-minute cap.
    //
    // TOMBSTONE: a SECOND countdown stood here, reading the person's active
    // transition and rendering `handoffDeadlineAt + SETTLE_FORCE_KILL_GRACE_MS`.
    // That was truthful about a six-minute path (120s lease + 120s handoff
    // window + 120s overdue lease) and is what put `shutting down in 3m 47s` on
    // the operator's screen. chiefd no longer has those phases -- a routine idle
    // park is minted already terminal -- so the whole read of `transitions` goes
    // with them rather than being kept and zeroed.
    const activityDoc = await readFooterStoreDocument(chiefdUrl, companyKey, "activity", changedStores) as {
      people?: Record<string, { idleSince?: unknown; lastDesiredActive?: unknown }>;
    } | undefined;
    const person = activityDoc?.people?.[identity.role];
    // Only while still desired-active: an already-parked/desired-inactive pane
    // is being torn down and needs no countdown.
    if (person && person.lastDesiredActive === true && typeof person.idleSince === "string") {
      const deadlineMs = Date.parse(person.idleSince) + SETTLE_QUIET_LEASE_MS;
      if (Number.isFinite(deadlineMs)) settleShutdownDeadlineAt = new Date(deadlineMs).toISOString();
    }
  } catch {
    // A settle countdown is supplementary presentation; corrupt or absent
    // activity state must not affect Pi.
  }
  let pendingMailboxCount = 0;
  try {
    // #44: this viewer's own pending mailbox count, read the SAME async
    // chiefd path (#30 Stage 1) every other footer field above uses — never a
    // blocking curl fork. The mailbox is SQL-only, store `mailbox/<personId>`
    // (src/organization/org-mailbox-store.ts), one row per person holding a
    // normalized `{entries:[...]}` snapshot. `delivered` is converge-owned but
    // still belongs to the pending VIEW until terminal settlement, matching the
    // intercom/mailbox-store projection. Absence → `undefined` → 0; an
    // unreachable or corrupt read is swallowed below and stays 0, so the footer
    // degrades to hidden rather than showing a stale/wrong count.
    const mailboxDoc = await readFooterStoreDocument(chiefdUrl, companyKey, `mailbox/${identity.role}`, changedStores) as {
      entries?: Array<{ state?: unknown }>;
    } | undefined;
    pendingMailboxCount = countPendingMailboxEntries(mailboxDoc);
  } catch {
    // A down/unreachable store is a refusal, not "no mail" — but the mailbox
    // segment is supplementary presentation, so degrade to hidden (0) rather
    // than crash the pane or show a guessed count.
    pendingMailboxCount = 0;
  }

  return { reminders, supervisionStale, supervisionUpdatedAt, settleShutdownDeadlineAt, pendingMailboxCount };
}


// #366: the footer used to paint its stat fields with 7 hardcoded One-Dark
// truecolor escapes (`footerPalette`/`footerColor`, dual-legible-reshaped by
// hand so each read against pure white AND pure black). That table never
// adapted to the active theme — a light theme still got the dark-tuned
// pastels. Every field now goes through `theme.fg(token, text)` instead, the
// same call every other card in this file already makes, so the footer
// adapts light/dark exactly like everything else (Pi's dark.json/light.json
// each independently tune their tokens for legibility on their own
// background — no manual dual-legible math needed here anymore). Mapping
// from the old 7 hues to the closest distinct semantic token, held constant
// across all call sites so a given field always reads the same color it did
// before:
//   purple -> accent | pink -> customMessageLabel | green -> success
//   blue -> border | yellow -> warning | orange -> error | cyan -> syntaxType
//
// #150: that mapping is now a DECLARED table rather than a per-field choice, so
// the footer's colors read from one auditable place. This is the single
// sanctioned `theme.fg` site outside `card-style.ts` (acceptance #1's footer
// field-table exemption): every footer field paints with `theme.fg(token, …)`
// where `token` comes from here. State-dependent fields
// (goals/reminders/memory) name their per-state variants explicitly; the token
// strings are unchanged from the inline values, so this refactor is
// byte-identical.
const FOOTER_FIELD_TOKENS = {
  team: "accent",
  role: "customMessageLabel",
  separator: "dim",
  cost: "dim",
  provider: "dim",
  model: "warning",
  reasoning: "error",
  context: "syntaxType",
  cacheHit: "warning",
  reminders: "customMessageLabel",
  mailbox: "accent",
  shutdown: "error",
  activity: "accent",
  skill: "syntaxType",
  memoryFailed: "error",
  memoryActive: "accent",
  /** "goals: …" / "goals: unknown" / "reminders: unknown" not-yet-read placeholders. */
  placeholder: "dim",
  /** #529: the "⚠ stale" tag when the supervision authority is frozen/off-mirror. */
  supervisionStale: "warning",
} as const;

/** Resolve the disk-authoritative organization identity injected by the launcher. */
export function organizationFooterIdentity(environment: Record<string, string | undefined>): TeamIdentity | undefined {
  const organization = environment.ORG_LAUNCHER_ORGANIZATION?.trim();
  const person = environment.ORG_LAUNCHER_PERSON?.trim();
  // The USERNAME is REQUIRED for an organization pane, and its absence means
  // this is not one — exactly as a missing person id does.
  //
  // There is deliberately no fall back to rendering `person`. That is what the
  // footer used to do, and it is how the operator came to be shown
  // `@portfolio-management-head`: an internal key presented as a handle, in
  // the one place a reader looks to learn what to call somebody. A pane with
  // no handle is better than a pane teaching the wrong name. Extensions ship
  // from the same release as the daemon that sets this, so there is no version
  // in which one exists without the other.
  const handle = environment.ORG_LAUNCHER_PERSON_NAME?.trim();
  if (!organization || !person || !handle) return undefined;
  return { team: organization, role: person, handle };
}

/**
 * The one pre-company identity, as the footer renders it.
 *
 * There are exactly two things a pane can be: a person inside a company, or
 * the Founder. Anything that is not the former is the latter — a Founder pane
 * is not a degraded company pane and has no other name.
 *
 * `@founder` and not `@operator`: `operator` is the human at the keyboard, not
 * an identity this product ships, and a footer reading `@operator` named
 * nothing the user could find anywhere else. `chiefd` and not `launcher`:
 * "launcher" is the retired pre-company mode, deleted along with its session,
 * its skill and its role string — surfacing the dead name in the one line
 * every Founder session shows is how it kept looking alive.
 */
export const FOUNDER_FOOTER_IDENTITY: TeamIdentity = { team: "chiefd", role: "founder" };

/**
 * Who this pane belongs to.
 *
 * The team-directory and operator-pointer lookup that used to sit here is
 * deleted with the mode it served: nothing sets `TEAM_LAUNCHER_TEAM_DIR` or
 * `TEAM_LAUNCHER_OPERATOR_POINTER` any more, so both branches could only ever
 * fall through to the two hard-coded retired strings they ended on.
 */
export function footerIdentity(environment: Record<string, string | undefined>): TeamIdentity {
  return organizationFooterIdentity(environment) ?? FOUNDER_FOOTER_IDENTITY;
}

function readIdentity(): TeamIdentity {
  return footerIdentity(process.env);
}

/**
 * The identity slot's two fields, exactly as they reach the terminal before
 * the theme paints them.
 *
 * Pure and exported so the strings a Founder actually reads can be asserted
 * without a live Pi footer. The `@` belongs here rather than at the render
 * site: it is part of the handle, and putting it beside the identity is what
 * makes `@founder` checkable in one place.
 */
export function footerIdentityFields(identity: TeamIdentity): { team: string; role: string } {
  // `handle` when there is one, `role` only for the Founder — whose `role` is
  // `founder`, a handle already, not a key.
  return { team: identity.team, role: `@${identity.handle ?? identity.role}` };
}

function formatCost(value: number): string {
  return `$${value.toFixed(3)}`;
}

// Pure, allocation-light compaction of a model's total context window into an
// uppercase unit (128_000 -> "128K", 1_000_000 -> "1.0M"). Any invalid, zero,
// negative, NaN, or infinite value is the explicit unknown "?" and never throws.
export function formatContextCapacity(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) return "?";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  return `${Math.round(value / 1_000)}K`;
}

function formatContextPercent(percent: unknown): string {
  if (typeof percent !== "number" || !Number.isFinite(percent) || percent < 0) return "?";
  return `${percent.toFixed(1)}%`;
}

// Compose the compact used/total context field, e.g. "23.7%/1.0M". The live
// session usage capacity (ctx.getContextUsage().contextWindow) wins whenever it
// is a finite positive number; the live model capacity is used only as a
// documented fallback so a known window can render before usage exists. Unknown
// parts stay explicit: "?/1.0M", "23.7%/?", or "?/?".
export function formatContextField(
  usage: { percent?: number | null; contextWindow?: number | null } | undefined | null,
  modelContextWindow: unknown,
): string {
  const percent = formatContextPercent(usage?.percent);
  const usageCapacity = usage?.contextWindow;
  const capacitySource =
    typeof usageCapacity === "number" && Number.isFinite(usageCapacity) && usageCapacity > 0
      ? usageCapacity
      : modelContextWindow;
  return `${percent}/${formatContextCapacity(capacitySource)}`;
}

function emptySessionUsage(): SessionUsageTotals {
  return { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, cost: 0 };
}

function addSessionEntryUsage(totals: SessionUsageTotals, entry: unknown): void {
  const candidate = entry as { type?: unknown; message?: { role?: unknown; usage?: Record<string, unknown> } };
  const message = candidate?.message;
  if (candidate?.type !== "message" || message?.role !== "assistant" || !message.usage) return;
  const usage = message.usage;
  for (const key of ["input", "output", "cacheRead", "cacheWrite"] as const) {
    const value = usage[key];
    if (typeof value === "number" && Number.isFinite(value)) totals[key] += value;
  }
  const cost = usage.cost as { total?: unknown } | undefined;
  if (typeof cost?.total === "number" && Number.isFinite(cost.total)) totals.cost += cost.total;
}

function finishSessionUsage(totals: SessionUsageTotals): SessionUsageTotals {
  const promptTokens = totals.input + totals.cacheRead + totals.cacheWrite;
  if (promptTokens > 0 && totals.cacheRead > 0) totals.cacheHitRate = (totals.cacheRead / promptTokens) * 100;
  return totals;
}

export function collectSessionUsage(entries: Iterable<unknown>): SessionUsageTotals {
  const totals = emptySessionUsage();
  for (const entry of entries) addSessionEntryUsage(totals, entry);
  return finishSessionUsage(totals);
}

/**
 * Pi session entries are append-only except for the live tail entry. Cache the
 * settled prefix and recompute only that tail, so a one-second footer render
 * remains O(1) as private session history grows. Pi returns a fresh shallow
 * array on every read, so prefix entry identity—not array identity—proves the
 * immutable prefix is shared. A replaced/truncated prefix resets safely.
 */
export function createSessionUsageCollector(): (entries: readonly unknown[]) => SessionUsageTotals {
  let settledCount = 0;
  let firstSettledEntry: unknown;
  let lastSettledEntry: unknown;
  let settled = emptySessionUsage();
  return (entries) => {
    const settledTarget = Math.max(0, entries.length - 1);
    const replacedPrefix = settledCount > 0
      && (entries[0] !== firstSettledEntry || entries[settledCount - 1] !== lastSettledEntry);
    if (settledTarget < settledCount || replacedPrefix) {
      settledCount = 0;
      firstSettledEntry = undefined;
      lastSettledEntry = undefined;
      settled = emptySessionUsage();
    }
    while (settledCount < settledTarget) {
      const entry = entries[settledCount];
      addSessionEntryUsage(settled, entry);
      if (settledCount === 0) firstSettledEntry = entry;
      lastSettledEntry = entry;
      settledCount += 1;
    }
    const totals = { ...settled };
    if (entries.length) addSessionEntryUsage(totals, entries[entries.length - 1]);
    return finishSessionUsage(totals);
  };
}

function normalizedRenderWidth(width: number): number {
  return Number.isFinite(width) ? Math.max(0, Math.floor(width)) : 0;
}

function fitFooterLine(line: string, width: number): string {
  return truncateToWidth(line, normalizedRenderWidth(width), "…");
}

export function joinedLine(left: string, right: string, width: number): string {
  const renderWidth = normalizedRenderWidth(width);
  if (!right) return fitFooterLine(left, renderWidth);
  const clippedRight = fitFooterLine(right, renderWidth);
  const rightWidth = visibleWidth(clippedRight);
  if (rightWidth >= renderWidth) return clippedRight;
  const availableLeft = Math.max(0, renderWidth - rightWidth - 2);
  const clippedLeft = truncateToWidth(left, availableLeft, "…");
  const leftWidth = visibleWidth(clippedLeft);
  // Preserve the usual two-column separation only when some left content
  // survived clipping. With no room for the left side, right alignment may
  // have only one spare column; forcing two was the live 40-into-39 crash.
  const gap = leftWidth > 0
    ? Math.max(2, renderWidth - leftWidth - rightWidth)
    : Math.max(0, renderWidth - rightWidth);
  return fitFooterLine(`${clippedLeft}${" ".repeat(gap)}${clippedRight}`, renderWidth);
}

interface FooterSnapshot {
  identity: TeamIdentity;
  organizationIdentity: TeamIdentity | undefined;
  organizationActivity: OrganizationFooterActivity | undefined;
}

/**
 * Every disk-backed footer field is collected here, never inside `render()`.
 * A footer render runs at spinner frame rate (~18 fps); reading the live
 * organization's multi-megabyte supervision ledger there cost ~50% of a CPU
 * core per idle agent.  This snapshot is refreshed on the same one-second
 * ticker that already drives the redraw, so the footer is exactly as fresh as
 * it was before while the per-frame cost becomes zero.
 */
async function collectFooterSnapshot(company: TeamUiCompany | undefined, changedStores?: ReadonlySet<string>): Promise<FooterSnapshot> {
  const identity = readIdentity();
  const organizationIdentity = organizationFooterIdentity(process.env);
  return {
    identity,
    organizationIdentity,
    // PERF #30 Stage 1: the org-activity read is now an in-process async fetch,
    // awaited here off the render thread — never a blocking curl fork.
    organizationActivity: organizationIdentity
      ? await persistedOrganizationFooterActivity(company, process.env, changedStores)
      : undefined,
  };
}

/**
 * PERF #30 Stage 1: the SYNCHRONOUS half of the snapshot — the pane's identity,
 * a cheap local read — assembled with no chiefd I/O so the very first paint is
 * correct before the async org-activity gather resolves. The org-activity field
 * is filled in by the first async `collectFooterSnapshot` (a plain, non-org pane
 * has no org activity at all, so its shell is already its whole snapshot).
 */
function collectFooterShellSnapshot(): FooterSnapshot {
  return {
    identity: readIdentity(),
    organizationIdentity: organizationFooterIdentity(process.env),
    organizationActivity: undefined,
  };
}

function installFooter(
  pi: ExtensionAPI,
  ctx: ExtensionContext,
  footerRender: { request: () => void },
  /**
   * THIS pane's company — its daemon's URL and its wire key — already read
   * from `<dir>/.chief/run/daemon.json` by {@link resolveTeamUiCompany} and
   * carried into every read this footer makes. `undefined` on a plain
   * (non-organization) pane, which reads nothing.
   */
  company: TeamUiCompany | undefined,
  options: TeamUiOptions = {},
) {
  // Pi emits model_select immediately after its native setModel call. The
  // footer reads ctx.model through Pi's live getter, so request a render on
  // that event instead of making a user wait for the one-second status tick.
  const sessionUsage = createSessionUsageCollector();
  const now = options.now ?? (() => Date.now());
  pi.on("model_select", () => { footerRender.request(); });
  // #30 first-frame fix: the snapshot and its priming gather live HERE, in
  // installFooter scope, not inside the setFooter factory — so the async
  // org-activity read starts the moment the footer is installed (at
  // `session_start`) rather than at the first paint, and `session_start` can
  // await it (bounded) before Pi paints.
  let snapshot: FooterSnapshot = collectFooterShellSnapshot();
  // The in-flight snapshot gather, exposed via `settle()` so a caller (and the
  // test suite) can await the reactive refresh deterministically.
  let pendingRefresh: Promise<void> = Promise.resolve();
  // PERF #30 Stage 1: fill the cached snapshot from the async gather WITHOUT
  // a repaint or fire detection — the mount prime and the theme-change
  // `invalidate`.
  const primeSnapshot = () => {
    const run = (async () => {
      try {
        snapshot = await collectFooterSnapshot(company);
      } catch {
        // A failed gather keeps the last-good snapshot; a docstore blip must
        // never crash the pane.
      }
    })();
    pendingRefresh = run;
    return run;
  };
  const initialPrime = primeSnapshot();
  ctx.ui.setFooter((tui, theme, footerData) => {
    /**
     * #34: the SSE-driven refresh. `changedStores` names exactly the store the
     * `doc-change` event carried, so the gather re-reads that one document and
     * serves the other four from their last-good cache. Every OTHER trigger
     * (mount prime, poll floor, reorg resync, channel-dead) calls
     * `refreshAndRender()` with nothing and still re-reads everything.
     */
    const refreshAndRenderStores = (changedStores?: ReadonlySet<string>): void => {
      const run = (async () => {
        try {
          snapshot = await collectFooterSnapshot(company, changedStores);
        } catch {
          // Keep the last-good snapshot on an unexpected gather failure, then
          // still repaint (matching the old always-repaint behaviour) so live
          // fields — clock-driven countdowns, model, context — stay fresh.
        }
        tui.requestRender();
      })();
      pendingRefresh = run;
    };
    /** A full refresh: every store re-read, no cache shortcuts. Deliberately
     * zero-arg so a caller that passes its own arguments
     * (`footerRender.request`) can never be mistaken for a changed-store hint. */
    const refreshAndRender = (): void => refreshAndRenderStores();
    // #361: re-append the LIVE SCHEDULES card only when the schedule *set*
    // actually changed (a protected schedule added, removed, or redefined) —
    // never on the constant countdown tick. This is the only mutation
    // primitive `appendEntry`/`registerEntryRenderer` offers (there is no
    // update-in-place API); called from the same reactive triggers below,
    // never a timer of its own.
    // Shared so the ephemeral activity line can force an immediate redraw on a
    // live event instead of waiting up to a second for the status ticker.
    footerRender.request = refreshAndRender;
    // The git-branch change subscription that used to live here went with the
    // branch field itself: a change channel for something nothing renders is a
    // re-render for no reason.
    // #276/#361/#827: this used to be a blind `setInterval(refreshAndRender, 1_000)`
    // for every pane, then a 60s poll-floor fallback beside the SseWatcher.
    // SSE is the change channel now with no fallback re-read: an SseWatcher on
    // exactly the stores the snapshot reads, refreshing on a real doc-change
    // event; `onReorg`/`onChannelStateChange("dead")` each drive exactly one
    // catch-up cycle (unchanged from before #827), and the watcher's own
    // heartbeat timeout + reconnect backoff cover "is the channel alive" and
    // "how do I get back". The render clock below survives with its I/O
    // branch deleted: it is now zero-I/O, calling only `tui.requestRender()`
    // so a frozen snapshot ages into its visible `⚠ stale` state — a
    // correctness/UX surface, not a change-detection sample (register entry:
    // scripts/reactive-allowlist.ts, class render-clock). A non-org (plain) pi
    // has no chiefd/SSE at all and needs none: with the Pi `/loop` surface
    // deleted, a plain pane's footer reads NO durable state whatsoever
    // (identity, model, context and session usage are all in-process, and
    // every one of them already pushes its own repaint through
    // `footerRender.request`/`model_select`). It therefore gets no watcher and
    // no timer — the `fs.watch` on `.pi/loops` (#361) went with the loops file
    // it watched, and #827 had already deleted the 60s re-read before it.
    const identity = organizationFooterIdentity(process.env);
    const createFloorTimer = options.createFloorTimer ?? ((fn: () => void, ms: number) => setInterval(fn, ms));
    const clearFloorTimer = options.clearFloorTimer ?? ((handle: unknown) => clearInterval(handle as ReturnType<typeof setInterval>));
    let sseWatcher: SseWatcherLike | undefined;
    let floor: unknown;
    if (identity && company) {
      sseWatcher = (options.createSseWatcher ?? ((watcherOptions: SseWatcherOptions): SseWatcherLike => subscribeSse(watcherOptions)))({
        url: requiredChiefdUrl(company.url),
        // A4: the footer's reader used to send `accept:` alone. It now
        // presents the SAME bearer this pane's org tools present — one token
        // cache per (daemon, person, key), not one per extension — and
        // re-authenticates on every drop instead of replaying a header the
        // daemon may already have stopped honouring.
        bearer: teamUiSseBearer(requiredChiefdUrl(company.url)),
        slug: company.key,
        // #337: "activity" added so the settle/shutdown countdown ticks off a
        // real doc-change event (the same reactive pattern as every other
        // footer field) instead of a new poll.
        // #44: `mailbox/<personId>` added so the 📬 pending-count segment ticks
        // off a real doc-change event (mail arriving/clearing) — the same
        // reactive channel every other footer field uses, never a new poll.
        stores: ["supervision", "activity", `mailbox/${identity.role}`],
        // #34: read ONLY the store this event named; the other four footer
        // documents are unchanged by definition and come from cache. The
        // schedule card is derived from supervision, so it is re-read only
        // when supervision itself moved.
        onEvent: (event: SseDocChangeEvent) => {
          refreshAndRenderStores(new Set([event.store]));
        },
        // Gap/restart-epoch resync: one full snapshot refresh, matching what
        // the old 1s tick always did on every pass. #296: reorg is a resync
        // trigger, not an unhealthy-channel signal — do not fold this into
        // onChannelStateChange.
        onReorg: () => { refreshAndRender(); },
        // #827: one catch-up cycle on dead, no re-arm — the watcher's own
        // reconnect backoff drives the retry from here.
        onChannelStateChange: (state: SseChannelState) => {
          if (state === "dead") { refreshAndRender(); }
        },
      });
      floor = createFloorTimer(() => {
        // #827: zero I/O. This tick exists only so a connected-but-frozen
        // supervision reader visibly ages into `⚠ stale` — see
        // supervisionSnapshotIsStale/SUPERVISION_STALE_MS. All actual reads
        // happen on doc-change/reorg/dead events above, never on this tick.
        tui.requestRender();
      }, footerRepaintFloorMs());
    }
    // #827: the plain (non-org) pane's old unconditional `refreshAndRender()`
    // floor is deleted outright, matching D0 (no fallback re-read left
    // anywhere). A plain pane now has no disk-backed footer field at all, so
    // it has nothing left to re-read on any cadence.
    (floor as { unref?: () => void })?.unref?.();
    return {
      dispose: () => {
        clearFloorTimer(floor);
        sseWatcher?.close();
        footerRender.request = () => {};
      },
      // PERF #30 Stage 1: test/caller seam to await the in-flight async
      // snapshot gather deterministically — resolves once the org-activity
      // fetch in flight (the install-time prime, or a later refresh) lands.
      settle() { return pendingRefresh; },
      // Pi calls this when a component must re-render from scratch (theme
      // change, cell-size probe) — never per frame — so it is the right place
      // to drop the cached footer snapshot as well. Refilled asynchronously
      // (no repaint of its own; Pi repaints after invalidate returns). Pi's
      // callback contract is synchronous; `settle()` exposes the refill to
      // callers that need to await it.
      invalidate() { void primeSnapshot(); },
      render(width: number): string[] {
        const identity = snapshot.identity;
        const fields = footerIdentityFields(identity);
        const team = theme.fg(FOOTER_FIELD_TOKENS.team, fields.team);
        const role = theme.fg(FOOTER_FIELD_TOKENS.role, fields.role);
        const bullet = theme.fg(FOOTER_FIELD_TOKENS.separator, "•");
        const totals = sessionUsage(ctx.sessionManager.getEntries());
        const sessionCost = totals.cost ? theme.fg(FOOTER_FIELD_TOKENS.cost, formatCost(totals.cost)) : "";
        // #504: the identity slot. The working directory used to live here and
        // is gone entirely (every pane's cwd is that person's own workspace —
        // it never carried information); company + person moved up from the
        // top-right.
        //
        // The git branch that used to follow them is gone for the same reason,
        // one step later: it was the branch of whatever checkout the pane's cwd
        // happened to sit in — for a Founder, the ChiefD source clone chiefd
        // starts Bun from, so the line read `… • e2e-main` and named a fact
        // about the installer's working copy. A git branch is not a fact about
        // a company, a person or the Founder, and there is nothing to put in
        // its place, so the field is deleted rather than re-sourced.
        const identityFields = [team, role, sessionCost];

        const model = ctx.model;
        const modelName = model?.id || "no-model";
        const provider = model?.provider ? theme.fg(FOOTER_FIELD_TOKENS.provider, model.provider) : "";
        const modelText = theme.fg(FOOTER_FIELD_TOKENS.model, modelName);
        const reasoning = model?.reasoning ? theme.fg(FOOTER_FIELD_TOKENS.reasoning, pi.getThinkingLevel()) : "";

        const usage = ctx.getContextUsage();
        const context = formatContextField(usage, ctx.model?.contextWindow);
        const statuses = footerData.getExtensionStatuses();
        // Ephemeral activity label (FIVE / task #27), if any is currently set.
        const liveActivity = statuses.get(ACTIVITY_STATUS_KEY) || undefined;
        const organizationIdentity = snapshot.organizationIdentity;
        const organizationActivity = snapshot.organizationActivity;

        // #504: the stale tag renders in the
        // TOP-RIGHT slot, not with the cost/context figures. They are computed
        // here from the same already-gathered async snapshot — moving the slot
        // moves no I/O into render(), which still touches nothing but memory.
        const supervisionStale = supervisionSnapshotIsStale(
          organizationActivity?.supervisionStale,
          organizationActivity?.supervisionUpdatedAt,
          now(),
        );
        // #529: the legible stale tag, shown only when the reminder count it
        // qualifies is itself on screen (so a quiet org stays quiet).
        const staleTag = supervisionStale && (organizationActivity?.reminders ?? 0) > 0
          ? theme.fg(FOOTER_FIELD_TOKENS.supervisionStale, "⚠ stale")
          : "";
        const first = joinedLine(
          identityFields.filter(Boolean).join(` ${bullet} `),
          staleTag,
          width,
        );

        const stats = [
          totals.cacheHitRate === undefined ? "" : theme.fg(FOOTER_FIELD_TOKENS.cacheHit, `CH ${totals.cacheHitRate.toFixed(1)}%`),
          theme.fg(FOOTER_FIELD_TOKENS.context, context),
          // This viewer's ARMED durable reminders — the footer's only
          // scheduled-work figure, read from chiefd's supervision ledger and
          // shown on its own terms. The label is spelled out ("2 reminders")
          // rather than left as a bare number, because an unlabelled count
          // tells the operator a number changed without telling them what it
          // counts.
          organizationIdentity
            ? organizationActivity === undefined
              // The gather has not landed yet (it is started at install and
              // awaited by `session_start`, so this is only ever reachable on a
              // slow/unreachable docstore). Paint nothing rather than a figure:
              // a provisional frame must never be mistaken for a real count.
              ? ""
              : organizationActivity.reminders === undefined
                // Never a confident zero for a state we could not read — that
                // confident zero IS the bug this feature was opened for.
                ? theme.fg(FOOTER_FIELD_TOKENS.placeholder, "reminders: unknown")
                : organizationActivity.reminders > 0
                  ? theme.fg(supervisionStale ? FOOTER_FIELD_TOKENS.placeholder : FOOTER_FIELD_TOKENS.reminders, `${organizationActivity.reminders} reminder${organizationActivity.reminders === 1 ? "" : "s"}`)
                  : ""
            : "",
          // #44: 📬 <count> of this viewer's own pending mailbox envelopes,
          // read off the same async footer gather. Shown only when there is
          // mail waiting; a zero count (empty, or an unreachable/corrupt read
          // degraded to 0) renders nothing at all — no idle 📬 0 clutter.
          (organizationActivity?.pendingMailboxCount ?? 0) > 0
            ? theme.fg(FOOTER_FIELD_TOKENS.mailbox, `📬 ${organizationActivity!.pendingMailboxCount}`)
            : "",
          // "⏻ shutting down in <time>" -- ONE countdown, from the moment
          // settling begins to the moment the pane goes away, ticking purely
          // from the footer's existing repaint cadence (no new timer, no new
          // disk read) and disappearing on its own once the pane is gone (the
          // source stops existing when the person is no longer desired-active).
          //
          // It can never read above the settle window, because it is that
          // window: there is no second phase to add. Past the deadline the pane
          // is being torn down on this very pass, which is what "moments" says
          // -- the local pane does not stop itself, the next reconcile does.
          organizationActivity?.settleShutdownDeadlineAt
            ? theme.fg(FOOTER_FIELD_TOKENS.shutdown, `⏻ shutting down in ${formatSettleCountdown(organizationActivity.settleShutdownDeadlineAt, now(), "moments")}`)
            : "",
          // FIVE / task #27: the ephemeral, event-driven activity verb
          // (working) set through
          // ctx.ui.setStatus. It is a live
          // in-process signal, so it takes precedence over the slower
          // disk-polled fallbacks below; when none are present, nothing
          // renders (empty ≠ broken). Read from the status map only — no
          // filesystem work in render().
          liveActivity ? theme.fg(FOOTER_FIELD_TOKENS.activity, liveActivity) : "",
        ].filter(Boolean).join(` ${bullet} `);
        const second = joinedLine(stats, [provider, modelText, reasoning].filter(Boolean).join(` ${bullet} `), width);

        // A custom Pi renderer must never return a line wider than the width
        // it was given. Keep this final fence even though joinedLine performs
        // its own clipping so future footer fields cannot crash narrow panes.
        return [first, second].map((line) => fitFooterLine(line, width));
      },
    };
  });
  // The install-time gather, so `session_start` can await it (bounded) before
  // the first paint.
  return initialPrime;
}

/**
 * How long `session_start` will wait for the install-time org-activity gather
 * before letting the footer paint provisionally. The gather is a local
 * in-process fetch to chiefd (single-digit ms warm); this ceiling exists only
 * so an unreachable/hung docstore can never hold up a pane. Exceeding it is
 * not an error — the footer paints the provisional "…" marker and the same
 * in-flight gather repaints it the moment it lands.
 */
export const FOOTER_FIRST_FRAME_PRIME_BUDGET_MS = 750;

export default function teamUi(pi: ExtensionAPI, options: TeamUiOptions = {}) {
  // #1208 TOMBSTONE: a guard that existed only as a comment.
  //
  // This block described intercepting malformed submissions and re-submitting
  // them as follow-ups, beside a `let agentBusy` with three writers and ZERO
  // readers and a `followUpUserMessage()` with zero callers. No
  // `pi.on("input", …)` existed anywhere in `packages/`. The mitigation was
  // never wired, so the error it claimed to prevent has been reaching the
  // operator the whole time.
  //
  // It is built for real now, in `organization-intercom`, because the thing
  // that makes a pane busy underneath a bare submission is that extension's own
  // turn-triggering — the guard belongs where the racer lives. The three
  // lifecycle registrations below stay: they drive the activity status line,
  // which is a different job and still has a reader.
  // #645: input text belongs exclusively to Pi's normal session writer. This
  // tracer records only opaque correlation ids and boundary names. Its
  // diagnostic is deliberately in-process: stderr is rendered by Pi as
  // transcript content, so normal input lifecycle observations must never
  // leak a debugging line into an operator's conversation.
  const attachedInputTracer = new AttachedInputTracer(() => {});

  // FIVE / task #27: the ephemeral, event-driven activity status line. It is
  // pure in-process state; its only outputs are ctx.ui.setStatus(...) and a
  // footer redraw. `lastCtx` is the most recent live context so the flash
  // auto-dismiss timer can clear the status without an event of its own; a
  // stale context after a session swap makes setStatus a harmless no-op.
  const footerRender = { request: () => {} };
  // #361: the LIVE SCHEDULES card's last-known composition signature, shared
  // between the initial `session_start` append and the footer's reactive
  // refresh cycle so the latter never re-appends a duplicate of what the
  // former already posted.
  let lastCtx: ExtensionContext | undefined;
  const activity = createActivityStatusLine({
    setStatus: (text) => {
      try { lastCtx?.ui.setStatus(ACTIVITY_STATUS_KEY, text); } catch { /* footer status is presentation-only */ }
      footerRender.request();
    },
  });
  pi.on("tool_execution_start", (event, ctx) => { lastCtx = ctx; activity.toolStart(event.toolCallId); });
  pi.on("tool_execution_end", (event, ctx) => { lastCtx = ctx; activity.toolEnd(event.toolCallId); });

  pi.on("session_start", async (_event, ctx) => {
    lastCtx = ctx;
    // Read THIS pane's own company ONCE, before the footer reads anything, and
    // carry the answer into every read. A plain (non-organization) pane has no
    // company and resolves to `undefined`; it reads no durable state at all,
    // so nothing below needs an address.
    // A rendezvous that cannot be read must not take the pane down with it:
    // the footer is presentation, and its own reads already degrade to the
    // provisional marker when no address is available (they refuse with
    // `OrgChiefdUrlUnsetError`, exactly as an unset variable used to make them
    // do). The reason is reported once rather than swallowed — a rendezvous
    // that is PRESENT but undecodable throws, and that is a build skew worth
    // saying out loud.
    let company: TeamUiCompany | undefined;
    try {
      company = resolveTeamUiCompany(process.env);
    } catch (error) {
      console.error(`[team-ui] this company's daemon rendezvous could not be read; the footer paints without durable state: ${error instanceof Error ? error.message : String(error)}`);
    }
    const primed = installFooter(pi, ctx, footerRender, company, options);
    // First-frame correctness: let the install-time org-activity gather land
    // before this handler resolves, so Pi's first paint reads a snapshot with
    // real reminder counts instead of the provisional marker. Bounded, and
    // never a blocking read — the wait is a resolved promise on the event
    // loop, so a hung docstore costs one timeout and nothing else.
    try {
      await Promise.race([primed, new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, options.firstFramePrimeBudgetMs ?? FOOTER_FIRST_FRAME_PRIME_BUDGET_MS);
        (timer as { unref?: () => void })?.unref?.();
      })]);
    } catch {
      // `primed` already swallows gather failures; this only guards the unexpected.
    }
  });

  // Pi owns the ordering between input dispatch, JSONL persistence, and turn
  // creation. `turn_start` is the first deterministic post-input boundary we
  // can observe without delaying or replaying the user's submission.
  pi.on("turn_start", () => {
    const sessionManager = lastCtx?.sessionManager;
    attachedInputTracer.transcriptChecked(sessionManager?.getEntries?.() ?? []);
    attachedInputTracer.turnStarted();
  });
  pi.on("agent_start", () => { activity.streamingStarted(); });
  pi.on("agent_settled", () => { activity.streamingSettled(); });
  pi.on("session_shutdown", () => { activity.reset(); });
}
