import type { ExtensionAPI, ExtensionContext, ThemeColor } from "@earendil-works/pi-coding-agent";
import { Box, Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";
import {
  appendFileSync,
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { resolveSendReplay } from "./org-send-replay";
import { spawnSync } from "node:child_process";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  BUILTIN_TOOLS,
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
  ORGANIZATION_MANAGER_TOOL_NAMES,
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
} from "@chief/piing/extension-runtime";
import { ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS } from "./organization-runtime-policy";
import { appendBoundedJsonlLine, BUS_EVENTS_MAX_BYTES } from "./bus-events-bounded-append";
import {
  ChiefdUnavailableError,
  type FetchTransport,
  isTransientChiefdError,
  OrgRowRefusalError,
  postOrgRoute,
  readDaemonRendezvous,
  RowStoresClient,
  subscribeSse,
  type AgentTokenManager,
  paneChiefdTransport,
  paneTokenManager,
  personPiHome,
  type AgentKeyRefusal,
  type PaneIdentity,
  type SseChannelState,
  type SseDocChangeEvent,
  type SseWatcherOptions,
} from "@chief/chiefing/extension-runtime";
import {
  CARD_EXPAND_HINT_TEXT,
  accentRgb,
  organizationPersonDisplayAccent,
  identityAccentOrder,
  organizationPersonAccents,
  cardBody,
  cardCallLine,
  cardDetail,
  cardHint,
  cardStateIcon,
  cardTitle,
  domainIcon,
  paneFailureSpec,
  providerConfigurationError,
  providerInsufficientCreditsError,
  providerRequestTooLargeError,
  renderCard as renderSharedCard,
  toolFailureText,
  finalizeHumanRef,
  scrubHumanRef,
  type CardLine,
  type CardTheme,
  type CardSpec,
  type CardTag,
  type MentionColorizer,
  type CardIcon,
  type CardState,
  type RenderCardOptions,
  CARD_GLYPHS,
  CARD_TEXT_SYMBOLS,
} from "./card-style";

export { ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS } from "./organization-runtime-policy";
/** Materialized active Pi homes advertise the full normal tool surface. The
 * extension still withholds runtime-fenced tools from unfenced processes. */
export {
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
  ORGANIZATION_MANAGER_TOOL_NAMES,
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
  ORGANIZATION_SUBTREE_TOOL_NAMES,
} from "@chief/piing/extension-runtime";

/**
 * The one production card boundary for organization-intercom.  Tool hooks and
 * live message renderers deliberately enter here rather than calling the
 * shared primitive themselves, so this extension has one migration seam while
 * preserving Pi's resolved theme and its current expanded/collapsed state.
 */
export function renderOrganizationCard(
  theme: CardTheme,
  spec: CardSpec,
  options: RenderCardOptions = {},
): Box | Text {
  return renderSharedCard(theme, spec, options);
}

// Keep the local call-site spelling while binding every existing Intercom card
// to the extension-owned entry above. This makes the 38-hook migration atomic
// and avoids a behavior-changing mechanical rewrite of each renderer.
const renderCard = renderOrganizationCard;

/** Tools that are safe to expose from a materialized organization home even
 * while a historical/diagnostic Pi process has no live launcher runtime. */
export const ORGANIZATION_BASELINE_TOOL_NAMES = [
  "org_send", "org_roster",
  // Durable reminders. Baseline, not
  // manager-only: the operator's ask was that an agent could schedule a
  // reminder for ITSELF, so a worker who cannot arm one is the feature not
  // being delivered. Reaching ANOTHER person is the cross-person write, and
  // chiefd fences it — `ensure_reminder_scope` refuses a caller who neither is
  // nor manages `personId`, against the credential the caller presented. Who
  // may REACH the tool and who they may reach WITH it are two questions, and
  // only the first is decided here.
  "org_create_reminder", "org_list_reminders", "org_stop_reminder",
] as const;

/** Exported so a harness can hand the `message_start` handler the same custom
 * type Pi does; the delivery path keys on it and a test that spells it itself
 * is a test of its own spelling. */
export const MESSAGE_TYPE = "organization-intercom-message";
const RESUME_TYPE = "organization-work-resumed";
/** A fatal pane-bootstrap failure (e.g. an unconfigured provider) rendered as a
 * legible `failure` card instead of a raw error dump (#399 part 2). */
const PANE_FAILURE_TYPE = "organization-pane-failure";
// TOMBSTONE (#751/P4): "reflection" is deleted from the product. A bounded
// handoff document (summary/learning/handoff/artifacts/openCommitments) that
// an agent wrote through an `org_reflect` tool before park/bench/transfer/
// offboard no longer exists, and neither does the request message type, the
// settled follow-up flag, the delivery-acceptance receipts, or the retry
// machinery that chased an interrupted one. Lifecycle transitions themselves
// are unchanged; they simply carry no handoff payload.
const SCHEMA_VERSION = 1 as const;
/**
 * Sender/notice accents resolve to Pi theme tokens, never hex. The reader's
 * own Pi session already supplies whichever theme is active (light, dark, or
 * auto) to every card renderer; picking colors this way means they inherit
 * that adaptation for free instead of fighting it with a hardcoded palette
 * that only ever looked right on one background (previously: white sender
 * names on cross-org/broadcast messages, invisible on any light terminal).
 *
 * Each token below is one a theme author already tuned for direct text
 * rendering (syntax/markdown/thinking colors), so rotating through them by
 * roster order gives distinct, theme-legible per-person identity without a
 * second, parallel color system. The same person always resolves to the same
 * token -- stable identity across a session -- while the underlying color
 * adapts with the active theme.
 */
const ORGANIZATION_PERSON_ACCENT_TOKENS: readonly ThemeColor[] = [
  "accent", "syntaxFunction", "syntaxVariable", "syntaxType", "syntaxKeyword",
  "mdHeading", "mdLink", "mdListBullet", "mdCode", "syntaxString",
  "syntaxNumber", "borderAccent", "customMessageLabel", "bashMode",
  "thinkingLow", "thinkingMedium", "thinkingHigh", "thinkingXhigh",
  "syntaxComment", "border",
];
/** Cross-org, broadcast, and unrecognized senders: a neutral token distinct
 * from any named person's rotation, not the white this replaces. */
const UNKNOWN_SENDER_ACCENT_TOKEN: ThemeColor = "muted";
/** System-notice titles use the theme's primary accent -- legible by
 * construction in every shipped theme, replacing the near-invisible-on-light
 * gray this had hardcoded. */
const SYSTEM_NOTICE_ACCENT_TOKEN: ThemeColor = "accent";

// --- #433: identity = color -------------------------------------------------
//
// Every rendered `@name` must carry THAT person's roster accent, so an operator
// can read who sent/received a message by color alone. The theme-token rotation
// above (`ORGANIZATION_PERSON_ACCENT_TOKENS`) cannot do this: a token always
// resolves through the *reader's* own Pi theme, so the same token is the same
// on-screen color no matter whose name it wraps (index-0 always = the reader's
// own `accent`). A person's identity color is a fixed hue owned by their roster
// position (`ORGANIZATION_PERSON_ACCENTS`, the source of truth the #393 pane
// text and the org-runtime `@accent` border both already use), so it can only be
// emitted as a per-person truecolor literal — Pi's `theme.fg()` only accepts
// named tokens, never a hex. The raw roster value remains the stable identity
// input. Text derives a Light or Dark foreground from that hue because one RGB
// value cannot meet 4.5:1 on both Pi surface families. The shared transform is
// in card-style, not copied into this file.
//
// #150 batch D: the accent palette, hue-wrap allocator, and operator
// exemption now live in ONE extension-side home — `card-style.ts` (the ORIGIN;
// see the pinning contract there). This file imports them; the old duplicated
// copy (with its "if you edit one, edit both" comment) is deleted.
// Re-exported so existing consumers keep their import site.
export { isOperatorIdentity, organizationPersonAccents } from "./card-style";
// (local bindings for this file's own use — a re-export creates none)

/** The identity accent for a non-Chief roster person: the RAW roster hex, byte-for-byte
 * the value the org-runtime pane `@accent` border uses and the input from which
 * the current-mode readable mention is derived. Returns
 * `undefined` for a broadcast (`all`), an
 * id not in this company's roster, or a sender from another organization — the
 * documented neutral cases that must NOT borrow a person's color. The Chief
 * is the root department head and keeps Pi's ordinary neutral appearance. */
export function organizationPersonAccentHex(
  manifest: IntercomOrganizationManifest,
  personId: string,
  senderOrganization?: string,
): string | undefined {
  if (senderOrganization && senderOrganization !== manifest.slug) return undefined;
  if (personId === "all") return undefined;
  const chiefPersonId = manifest.departments[manifest.rootDepartmentId]?.headPersonId;
  if (personId === chiefPersonId) return undefined;
  // #485: allocate by identity (createdAt order), not peopleOrder position, so a
  // person's @name mention colour is stable across roster growth and stays
  // byte-identical to their pane border (which now does the same).
  const order = identityAccentOrder(manifest.people);
  const index = order.indexOf(personId);
  if (index < 0) return undefined;
  return organizationPersonAccents(order)[index]!;
}

/** Truecolor foreground escape for a #rrggbb color, wrapping `text` and closing
 * with a foreground-only reset (`\x1b[39m`), mirroring how Pi's own
 * `Theme.fg` closes. `reopen` is re-applied after the reset so a colored
 * mention embedded inside a dim run leaves the surrounding dim intact. */
function truecolorMention(hexColor: string, text: string, reopen: string): string {
  const [r, g, b] = accentRgb(hexColor);
  return `\x1b[38;2;${r};${g};${b}m${text}\x1b[39m${reopen}`;
}

/** Matches a bare `@handle` mention (lowercase alphanumerics + hyphens).
 *
 * The grammar is shared by two different things and that is deliberate: a
 * person's USERNAME, which is what these cards and every delivered message
 * now show, and a person's kebab id, which older text and cross-organization
 * senders may still carry. Both are matched so a mention is colored either
 * way; the colorizer resolves the roster and leaves anything it cannot place
 * uncolored. */
const PERSON_MENTION_PATTERN = /@([a-z0-9][a-z0-9-]*)/gi;

/**
 * Replace every `@<rosterId>` in `text` with that person's identity-accent
 * truecolor escape (#433). A mention whose id is not a roster person — `@all`,
 * a department, an unknown handle — is left exactly as-is (the documented
 * neutral treatment: it keeps whatever surrounding color the caller set).
 * `reopenToken` names the theme color the surrounding run is drawn in (default
 * `dim`, what card targets use) so it can be restored after each colored
 * mention; the reopen escape is read from the reader's own live theme so it
 * stays theme-adaptive.
 */
export function colorizePersonMentions(
  theme: any,
  manifest: IntercomOrganizationManifest,
  text: string,
  reopenToken: string = "dim",
): string {
  const reopen = typeof theme?.getFgAnsi === "function" ? theme.getFgAnsi(reopenToken) : "";
  return text.replace(PERSON_MENTION_PATTERN, (whole, mention: string) => {
    // A mention may be a USERNAME or an id, and both must colour. Ids still
    // resolve first and exactly, so a handle that happens to equal a different
    // person's id can never steal their colour; only an unmatched token falls
    // through to the handle lookup. Without this the accent would quietly
    // disappear from every surface that started naming people properly — the
    // colour is how a reader tells one person from another at a glance.
    const id = manifest.people[mention]
      ? mention
      : (manifest.peopleOrder.find((candidate) => personHandle(manifest, candidate) === mention.toLowerCase()) ?? mention);
    const hexColor = organizationPersonAccentHex(manifest, id);
    return hexColor
      ? truecolorMention(organizationPersonDisplayAccent(theme, hexColor), whole, reopen)
      : whole;
  });
}

/**
 * Best-effort synchronous cache of the last successfully loaded manifest per
 * organization, warmed by every successful {@link loadIntercomOrganization}
 * call. Pi's `renderCall`/`renderResult` callbacks run synchronously on the
 * render path and must never block on a docstore fetch, so
 * `personMentionColorizer` below reads this cache instead of awaiting a
 * fresh read — a cold cache renders uncolored, never throws, never stalls.
 */
const lastKnownIntercomManifest = new Map<string, IntercomOrganizationManifest>();

/**
 * The USERNAME to SHOW for a person, resolved at render time from the
 * last-known-good roster.
 *
 * Same contract as the mention colorizer below: display-only, never a fresh
 * read, never throws. An unknown person — a cold cache, or a sender from
 * another organization whose roster is not ours — renders the raw id, which is
 * exactly what every surface did before and is therefore never a regression.
 */
function displayHandle(organization: string | undefined, personId: string): string {
  const manifest = organization ? lastKnownIntercomManifest.get(organization) : undefined;
  return manifest ? personHandle(manifest, personId) : personId;
}

/** Build a {@link MentionColorizer} for a card target, resolving the roster
 * from the last-known-good in-memory manifest at render time (never a fresh
 * docstore read — see {@link lastKnownIntercomManifest}). Falls back to the
 * uncolored target if no manifest has been cached yet — a card must always
 * render, never throw or stall over a cosmetic color. */
function personMentionColorizer(theme: any, context: OrganizationRuntimeContext): MentionColorizer {
  return (target: string) => {
    const manifest = lastKnownIntercomManifest.get(context.organization);
    if (!manifest) return target;
    try {
      return colorizePersonMentions(theme, manifest, target);
    } catch {
      return target;
    }
  };
}

export const ORGANIZATION_HEALTH_NOTICE_KINDS = [
  "exception",
  "runtime_log_error",
  "maintenance_stuck",
  "mailbox_recipient_inactive",
  "mailbox_unit_inactive",
  "mailbox_invalid",
  "mailbox_delivery_stale",
  "supervisor_not_running",
  "supervisor_stale",
  "supervisor_error",
  "runtime_reconciliation_stalled",
  "runtime_activity_mismatch",
  "runtime_projection_mismatch",
  "runtime_session_missing",
  "runtime_dead_processes",
  "idle_pane_awaiting_release",
  "runtime_ownership_conflict",
] as const;

type EmploymentState = "active" | "benched" | "departed";
type PersonKind = "executive" | "head" | "worker";
type MessageUrgency = "normal" | "interrupt";
// TOMBSTONE: `SESSION_REPLACEMENT_UNSUPPORTED` and `hostReplacesSessions`,
// #1244's capability gate and #1245's polarity note.
//
// They existed to refuse `fresh_session` honestly on a Pi that cannot replace a
// session — which is every released Pi, since chief authored that API in its
// own patch. The operator's ruling deletes the FEATURE, so the gate has nothing
// left to guard: an instrument on a branch that cannot be taken is exactly what
// this file keeps removing, and one guarding a tool that no longer exists is
// the same thing with a shorter life.
//
// Deleted in the same commit as the tool rather than left orphaned.

// ONE ACTION. `fresh_session` and `set_model` are deleted with the tool that
// was their only caller; `compact` survives because the AUTOMATIC compaction
// queues through this same pipeline from the settle handler, and its Pi hooks
// are upstream rather than patched.
type SessionMaintenanceAction = "compact";
interface SessionMaintenanceRequest {
  id: string;
  action: SessionMaintenanceAction;
  personId: string;
  requestedBy: string;
  reason: string;
  automatic: boolean;
  status: "queued" | "running" | "applying" | "completed" | "failed" | "skipped";
  requestedAt: string;
  startedAt?: string;
  completedAt?: string;
  error?: string;
  attempt?: number;
  recoveredFromRequestId?: string;
  retryNotBefore?: string;
  claimedProcessId?: number;
  claimedSessionId?: string;
  claimToken?: string;
  completedProcessId?: number;
  completedSessionId?: string;
  completionClaimToken?: string;
  companyActionId?: string;
  force?: boolean;
  interruptedProcessId?: number;
  interruptedSessionId?: string;
  interruptedClaimToken?: string;
  interruptedAt?: string;
  compactSessionId?: string;
  compactAnchorEntryId?: string;
  completedCompactionEntryId?: string;
}
interface SessionMaintenanceClaim { processId: number; sessionId: string; claimToken: string }
/**
 * `boundary` names WHICH lifecycle proof minted this lease, because the two
 * boundaries prove different things and `isCurrent` must check each against
 * its own evidence:
 *
 * - `settled` — Pi emitted `agent_settled`. The pane is between turns and is
 *   only safe to mutate while it STAYS between turns, so the lease also
 *   requires no pending messages: a pending message means Pi is about to
 *   start a turn this lease cannot see.
 * - `pre-turn` — Pi is blocked inside `prompt()`, awaiting this extension's
 *   `before_agent_start` handler, before the agent run begins and before the
 *   provider is contacted. Nothing in this process can start a turn while the
 *   handler is awaited, so this boundary is STRICTLY stronger than `settled`
 *   and deliberately does NOT consult `hasPendingMessages()`: pending
 *   messages are the normal state of the pane this boundary exists to rescue.
 */
interface SessionMaintenanceLifecycleLease { epoch: number; boundary: "settled" | "pre-turn" }
interface SessionMaintenanceLifecycleFence {
  sessionStarted(extensionContext: ExtensionContext): void;
  invalidate(): void;
  toolStarted(toolCallId: unknown): void;
  toolEnded(toolCallId: unknown): void;
  settled(extensionContext: ExtensionContext): SessionMaintenanceLifecycleLease | undefined;
  /** The pre-turn boundary: mint a lease for the window Pi is holding open
   *  inside `prompt()` while this extension's `before_agent_start` handler is
   *  awaited. */
  beforeTurn(extensionContext: ExtensionContext | undefined): SessionMaintenanceLifecycleLease | undefined;
  capture(extensionContext: ExtensionContext | undefined): SessionMaintenanceLifecycleLease | undefined;
  isCurrent(lease: SessionMaintenanceLifecycleLease | undefined, extensionContext: ExtensionContext | undefined): boolean;
}
interface SessionMaintenanceProjection {
  queued?: SessionMaintenanceRequest;
  running: SessionMaintenanceRequest[];
  applying?: SessionMaintenanceRequest;
  /** The exact current terminal native reset is exposed only to repair a
   * separately proven historical Pi marker at startup. */
  failed?: SessionMaintenanceRequest;
  /** Any unresolved fleet action blocks ordinary work for every member, even
   * after this person's own target has completed. `unknown` fails closed when
   * a present ledger cannot be projected safely. */
  blockingCompanyActionId?: string;
  /**
   * Why a fail-closed projection could not be resolved, set exactly when
   * `blockingCompanyActionId` is the `unknown` sentinel.
   *
   * The sentinel alone is not a diagnosis. Collapsing every fault into one
   * literal told an operator their ledger was corrupt when the real fault was
   * a dead write service or the wrong database. The cause names WHAT failed
   * and the detail says what to check.
   */
  unresolvable?: { cause: SessionMaintenanceUnresolvableCause; detail: string };
}
/**
 * `ledger` — the session-maintenance document could not be projected: it is
 *   present but corrupt (fail closed).
 * `ledger_unreachable` — the write service did not answer; transient, and it
 *   clears by itself when the service returns.
 */
type SessionMaintenanceUnresolvableCause = "ledger" | "ledger_unreachable";
interface RecoveredSessionMaintenance {
  interrupted: SessionMaintenanceRequest[];
  replacements: SessionMaintenanceRequest[];
}

/** Pi's exact stale-context diagnostic, matched on a fragment so a wording
 * change in the surrounding sentence does not silently stop matching. */
const PI_STALE_CONTEXT_ERROR = "stale after session replacement";

/**
 * Every field on Pi's `ExtensionContext` is a guarded getter that first calls
 * `ExtensionRunner.assertActive()`, and the runner is invalidated by
 * `AgentSession.dispose()` — precisely what a session replacement does to the
 * outgoing session. Because the throw happens *inside* the getter,
 * `ctx.sessionManager?.getSessionId?.()` protects nothing: optional chaining
 * never gets to run. Read the session manager only through here, so a context
 * that went stale across an await degrades to "no live session" instead of
 * throwing out of a lifecycle handler.
 */
function sessionManagerOf(extensionContext: ExtensionContext | undefined): ExtensionContext["sessionManager"] | undefined {
  if (!extensionContext) return undefined;
  try {
    return extensionContext.sessionManager ?? undefined;
  } catch {
    return undefined;
  }
}

/** True once Pi has invalidated the runner owning this context. A stale
 * context can never be used for reads, maintenance authority, or host calls. */
function isExtensionContextStale(extensionContext: ExtensionContext | undefined): boolean {
  if (!extensionContext) return false;
  try {
    void extensionContext.sessionManager;
    return false;
  } catch {
    return true;
  }
}

function isStaleExtensionContextError(error: unknown): boolean {
  return error instanceof Error && error.message.includes(PI_STALE_CONTEXT_ERROR);
}

/**
 * Pi 0.80.10 sets `_isAgentRunActive = false` before it awaits extension
 * `agent_settled` handlers. A newer prompt may therefore begin while an older
 * settled handler is still awaiting mailbox or launcher I/O. This synchronous
 * lifecycle lease makes that old handler permanently stale as soon as Pi
 * emits any start boundary. Tool completion never restores the lease: only a
 * later explicit `agent_settled` can prove the tool result turn fully settled.
 */
function createSessionMaintenanceLifecycleFence(): SessionMaintenanceLifecycleFence {
  let epoch = 0;
  let leaseEpoch: number | undefined;
  let leaseBoundary: SessionMaintenanceLifecycleLease["boundary"] | undefined;
  const activeToolCallIds = new Set<string>();
  const invalidate = () => {
    epoch += 1;
    leaseEpoch = undefined;
    leaseBoundary = undefined;
  };
  const isLiveIdle = (extensionContext: ExtensionContext | undefined) => {
    if (!extensionContext || activeToolCallIds.size) return false;
    // A replaced session is never "live idle", however idle its dead context
    // still claims to be.
    if (isExtensionContextStale(extensionContext)) return false;
    try {
      return extensionContext.isIdle?.() === true && extensionContext.hasPendingMessages?.() !== true;
    } catch {
      return false;
    }
  };
  /** The pre-turn boundary's own idleness proof. `hasPendingMessages()` is
   *  deliberately absent — see `SessionMaintenanceLifecycleLease`. Everything
   *  else `isLiveIdle` demands still holds: a replaced session is dead, and a
   *  tool that Pi never reported as ended must still fence maintenance out. */
  const isPreTurnIdle = (extensionContext: ExtensionContext | undefined) => {
    if (!extensionContext || activeToolCallIds.size) return false;
    if (isExtensionContextStale(extensionContext)) return false;
    try {
      return extensionContext.isIdle?.() === true;
    } catch {
      return false;
    }
  };
  const holds = (extensionContext: ExtensionContext | undefined): boolean => (
    leaseBoundary === "pre-turn" ? isPreTurnIdle(extensionContext) : isLiveIdle(extensionContext)
  );
  const capture = (extensionContext: ExtensionContext | undefined): SessionMaintenanceLifecycleLease | undefined => (
    leaseEpoch === epoch && leaseBoundary !== undefined && holds(extensionContext)
      ? { epoch, boundary: leaseBoundary }
      : undefined
  );
  return {
    sessionStarted(extensionContext) {
      invalidate();
      // A replacement Pi session cannot inherit an in-flight tool from the
      // previous native session. Pi may omit tool_execution_end when that
      // session is aborted, so discard those stale observations here.
      activeToolCallIds.clear();
      // A restored native session may be genuinely idle before its first new
      // agent turn. The session_start lifecycle boundary can authorize polling
      // only while its exact live context remains idle and queue-free.
      if (isLiveIdle(extensionContext)) {
        leaseEpoch = epoch;
        leaseBoundary = "settled";
      }
    },
    invalidate,
    toolStarted(toolCallId) {
      invalidate();
      if (typeof toolCallId === "string" && toolCallId) activeToolCallIds.add(toolCallId);
    },
    toolEnded(toolCallId) {
      if (typeof toolCallId === "string" && toolCallId) activeToolCallIds.delete(toolCallId);
      // Deliberately do not restore the lease epoch here. In pinned Pi, the
      // toolResult message is emitted and persisted after tool_execution_end.
    },
    settled(extensionContext) {
      // agent_settled is Pi's authoritative proof that the run, including all
      // tools and persisted tool results, has finished. Recover from a missed
      // tool_execution_end instead of permanently fencing maintenance.
      activeToolCallIds.clear();
      leaseEpoch = isLiveIdle(extensionContext) ? epoch : undefined;
      leaseBoundary = leaseEpoch === undefined ? undefined : "settled";
      return capture(extensionContext);
    },
    beforeTurn(extensionContext) {
      // Pi awaits `before_agent_start` inside `prompt()`, before
      // `_runAgentPrompt` sets its run active and before any provider call.
      // Invalidating first makes every older lease — including a settled
      // handler still awaiting its own I/O — stale at this instant, exactly as
      // the plain `invalidate()` that used to be all this boundary did. A
      // replacement session cannot inherit an in-flight tool either, and Pi
      // may omit `tool_execution_end` for an aborted one, so the same stale
      // observations `sessionStarted` discards are discarded here.
      invalidate();
      activeToolCallIds.clear();
      if (!isPreTurnIdle(extensionContext)) return undefined;
      leaseEpoch = epoch;
      leaseBoundary = "pre-turn";
      return { epoch, boundary: "pre-turn" };
    },
    capture,
    isCurrent(lease, extensionContext) {
      return lease?.epoch === epoch && lease.boundary === leaseBoundary
        && leaseEpoch === epoch && holds(extensionContext);
    },
  };
}
type OrganizationUnitKind = "company" | "department" | "contract";

interface ContractUnitMetadata {
  engagement: string;
  launchedAt: string;
  expiresAt?: string;
}

interface DepartmentRecord {
  id: string;
  name: string;
  purpose: string;
  /** Optional only for schema-v1 manifests created before unit kinds. */
  kind?: OrganizationUnitKind;
  transient?: ContractUnitMetadata;
  parentDepartmentId?: string;
  headPersonId: string;
  state: "active" | "paused";
}

export interface PersonRecord {
  id: string;
  name: string;
  title: string;
  kind: PersonKind;
  departmentId: string;
  employmentState: EmploymentState;
  /** #485: persisted creation time (present on the durable manifest this
   * projects from), so identity accents can be allocated by identity order. */
  createdAt: string;
}

export interface IntercomOrganizationManifest {
  schemaVersion: number;
  kind: "organization";
  slug: string;
  name: string;
  rootDepartmentId: string;
  /* AC6: `runtimeSession` is gone from chiefd's manifest, and nothing in this
   * extension derives a replacement any more — the only consumer was a CEO
   * boot-lease check that compared the derived name against itself. */
  departmentOrder: string[];
  peopleOrder: string[];
  departments: Record<string, DepartmentRecord>;
  people: Record<string, PersonRecord>;
}

export interface OrganizationRuntimeContext {
  /**
   * THE COMPANY DIRECTORY — the one the operator ran `chief` in, and this
   * pane's own cwd. `<dir>/.chief` holds the store, the keys and the
   * rendezvous; `<dir>/.pi` is Pi's.
   */
  organizationDir: string;
  /** The company's DISPLAY name. Names nothing: two directories may hold
   * companies with the same one, which is why {@link
   * OrganizationRuntimeContext.companyKey} exists. */
  organization: string;
  personId: string;
  /** Exact directory holding this person's company identity key. */
  identityDir: string;
  launcherRoot: string;
  runtimeSocket?: string;
  runtimeSession?: string;
  /** Runtime-owned pane token captured at pane execution before Pi sanitizes it. */
  runtimePane?: string;
  /**
   * THIS company's daemon, resolved from beacond by
   * {@link resolveOrganizationRuntimeContext} — never read from the process
   * environment, and never shared between two companies hosted in one process.
   *
   * `undefined` on a context that has only been PARSED
   * ({@link readOrganizationRuntimeContext}), which is a legitimate state for
   * the identity-only callers; every chiefd call then refuses with
   * {@link OrgChiefdUrlUnsetError} rather than guessing an address that may
   * belong to another company.
   */
  chiefdUrl?: string;
  /**
   * THIS company's wire identity, `sha256(<dir>)[..12]`, READ from the same
   * rendezvous file the URL came from — never derived here.
   *
   * It replaces the composite `slug@sha256(orgsRoot)[..12]` this file rebuilt
   * at a dozen call sites. One producer (whoever created the company), one
   * field on the wire, and no second derivation to drift: a key rebuilt
   * slightly differently does not fail loudly, it matches no live company, so
   * the route 404s and the write silently never happens.
   *
   * `undefined` on a context that has only been PARSED
   * ({@link readOrganizationRuntimeContext}) — a legitimate state for the
   * identity-only callers. It arrives WITH {@link
   * OrganizationRuntimeContext.chiefdUrl}, from one file read, so a context
   * that can reach a daemon can always name its company.
   */
  companyKey?: string;
}

export interface OrganizationEnvelope {
  schemaVersion: typeof SCHEMA_VERSION;
  id: string;
  organization: string;
  fromPersonId: string;
  to: string;
  recipients: string[];
  body: string;
  urgency: MessageUrgency;
  replyTo?: string;
  healthIncident?: {
    fingerprint: string;
    kind: string;
    recipientPersonId: string;
  };
  createdAt: string;
}

/** A content-addressed, bounded review of normal peer mail. The pending
 * mailbox rows remain the durable checklist; this object is only the one Pi
 * turn that presents that checklist. */
interface OrganizationMailboxBatch {
  schemaVersion: 1;
  batchId: string;
  envelopes: OrganizationEnvelope[];
}

function mailboxBatchId(envelopes: readonly OrganizationEnvelope[]): string {
  return `organization-mailbox-batch-${envelopes.map((envelope) => envelope.id).join("+")}`;
}

function isOrganizationMailboxBatch(value: unknown): value is OrganizationMailboxBatch {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const candidate = value as Partial<OrganizationMailboxBatch>;
  return candidate.schemaVersion === 1 && typeof candidate.batchId === "string"
    && candidate.batchId.length > 0 && Array.isArray(candidate.envelopes)
    && candidate.envelopes.length > 0 && candidate.envelopes.every((envelope) => envelope && typeof envelope === "object");
}

interface WorkResumeDetails {
  personId: string;
  /** #399: this is the person's first materialization (no prior Pi transcript
   * was restored), so the card/prompt welcome them instead of claiming a
   * "brief restart interrupted this Pi session." Absent/false = genuine resume. */
  firstBoot?: boolean;
  /** How many people the whole company holds, from the manifest this install
   * already loaded — a REPORTED FACT, not a decision. {@link workResumePrompt}
   * combines it with {@link WorkResumeDetails.firstBoot} to recognise the
   * FOUNDING BOOT: seconds after genesis a company is exactly one person — the
   * CEO chiefd minted — with no departments (every department has a head, and a
   * head LIVES in the one it heads, so one person means one root and nothing
   * under it), no goals, no schedule and no history.
   *
   * It is carried because the first-boot copy tells a person to "take the exact
   * next useful step toward the work you were hired for", and on a company that
   * new the only way to comply is to INVENT that work: the operator watched
   * their brand-new company start building departments and hiring into them
   * while they were still reading the first screen. Read fresh on every boot
   * and never persisted — a stored "first boot" flag is a second answer to a
   * question the roster already answers, and it is the answer nobody clears. */
  companyPeopleCount: number;
  /** How many durable messages are waiting unread in this person's mailbox —
   * the only durable reason a person still has work to come back to. */
  pendingMessageCount: number;
  /** Launcher-owned protected schedule, the only recurring check a resuming
   * person still carries in-session. Durable reminders are delivered as their
   * own messages and are deliberately not summarized here. */
  protectedSchedules: string[];
}

/** Resolve the sender's theme-token accent without guessing across organizations or broadcasts. */
export function organizationMessageSenderAccent(
  manifest: IntercomOrganizationManifest,
  senderOrganization: string | undefined,
  senderPersonId: string,
): ThemeColor {
  if (senderOrganization && senderOrganization !== manifest.slug) return UNKNOWN_SENDER_ACCENT_TOKEN;
  // A local CEO is deliberately not a roster-coloured identity. Resolve the
  // CEO from the root department, not from a roster position.
  if (!organizationPersonAccentHex(manifest, senderPersonId, senderOrganization)) {
    return UNKNOWN_SENDER_ACCENT_TOKEN;
  }
  const index = manifest.peopleOrder.indexOf(senderPersonId);
  return index < 0 ? UNKNOWN_SENDER_ACCENT_TOKEN : ORGANIZATION_PERSON_ACCENT_TOKENS[index % ORGANIZATION_PERSON_ACCENT_TOKENS.length]!;
}

/** Color text with a theme token through the active theme -- never a
 * hand-built ANSI escape, so it always follows the reader's light/dark/auto
 * Pi theme instead of a color baked in at write time.
 *
 * (#357 supersedes #353's dual-legible-reshape version of this function --
 * confirmed with theme-eng: that approach kept the raw hex accents and
 * reshaped them at render time based on a heuristic read of
 * `theme.getFgAnsi("customMessageText")`; this delegates the color choice to
 * the theme itself via `theme.fg(token, text)`, which is strictly more
 * correct -- there is nothing left to reshape or guess about.) */
function colorOrganizationMessageSender(theme: any, accent: ThemeColor, text: string): string {
  return cardBody(theme, accent, text);
}

export interface LauncherSystemNoticePresentation {
  // "⏰ Reminder" is its own title, never folded into a launcher one: a
  // reminder is the PERSON's own scheduled note to themselves, not the
  // launcher acting on them, and rendering it under a launcher title would
  // leave two different events indistinguishable on screen with nothing on the
  // pane able to reveal which one fired.
  title: "🧠 Learning needs attention"
    | "⚡ Work recovery" | "❗ System needs attention"
    | "⚙ System notice" | "⏰ Reminder";
  summary: string;
  /**
   * The producer's own prose, for the branches whose body is safe launcher-
   * authored text (#8).
   *
   * Deliberately OPT-IN per branch rather than "render `envelope.body`". A
   * health-incident or unrecognized notice may carry opaque runtime detail and
   * provider tokens, and the launcher card exists precisely to present those
   * neutrally -- the existing "no person semantics, nothing opaque" contract
   * (and its snapshot) is what caught a blanket emission leaking
   * `token=never-render-this` onto the screen. Absent means render no prose.
   */
  body?: string;
  /**
   * What KIND of payload `body` is, which decides how it collapses (#103).
   *
   * - `"prose"` (default): conversational text whose opening sentence carries
   *   the gist. A bounded preview is a faithful summary of it.
   * - `"list"`: an enumeration — the items a reminder names. **A preview is a
   *   summarisation strategy that assumes the
   *   beginning of the text is representative, and for a list it is not**: 96
   *   characters of a list is one item and an ellipsis. So a `"list"` body
   *   renders IN FULL even collapsed.
   *
   * This distinction is the whole of #103. The operator's original complaint
   * was "I have never seen these cards"; we fixed the card to carry a body,
   * and a collapsed preview then delivered the notification while withholding
   * the information — a card announcing that something was due without naming
   * WHAT. That is the same complaint one layer in.
   *
   * Deliberately per-branch rather than global: conversational and opaque
   * launcher notices KEEP the bounded preview, which is correct for prose and
   * is what stops a health incident spilling runtime detail onto the pane.
   */
  bodyLayout?: "list" | "prose";
  context: string[];
  nextAction: string;
  impact: string;
  blocked: boolean | "unknown";
}

const MAX_SYSTEM_NOTICE_BODY_CHARACTERS = 32_768;

function boundedNoticeBody(value: unknown): string | undefined {
  return typeof value === "string" ? value.slice(0, MAX_SYSTEM_NOTICE_BODY_CHARACTERS) : undefined;
}

/**
 * The prose a launcher notice carries, with its leading `[marker]` routing token
 * removed (#8).
 *
 * The marker is how `launcherSystemNoticePresentation` chose the card, so the
 * title already encodes it; showing it again would put a bare `[reminder]` on
 * screen -- the same content-free token #76 deleted from the other end of this
 * pipeline. Everything after the marker is the producer's real
 * text and is rendered verbatim.
 */
function systemNoticeBody(body: unknown): string | undefined {
  const text = boundedNoticeBody(body);
  if (!text) return undefined;
  const stripped = text.replace(/^\[[^\]\n]*\]\s*/, "").trim();
  if (!stripped.length) return undefined;
  // A launcher card must stay visually distinct from a person card: no line
  // below the title may open with a person-card bullet or a card glyph, so a
  // producer's leading hyphen is re-set to a middle dot. The prose itself is
  // untouched -- only the list glyph is normalized to this card's idiom, so
  // the reader sees every line verbatim without the card impersonating person
  // mail.
  return stripped
    .split("\n")
    .map((line) => line.replace(/^(\s*)-\s+/, "$1· "))
    .join("\n");
}

function unknownSystemNoticePresentation(): LauncherSystemNoticePresentation {
  return {
    title: `${CARD_TEXT_SYMBOLS.gear} System notice`,
    summary: "ChiefD supplied a notice this version does not recognize.",
    context: [],
    nextAction: "Inspect org_roster and the restricted health diagnostics; do not repeat or forward the raw payload.",
    impact: "Normal-work impact is unknown; verify system state before relying on the affected path.",
    blocked: "unknown",
  };
}

function boundedSystemNoticeText(value: unknown, maximum = 140): string | undefined {
  if (typeof value !== "string") return undefined;
  const scrubbed = scrubHumanRef(value, { prefixes: ["transition", "health"] }).trim();
  return finalizeHumanRef(scrubbed, maximum);
}

function knownNoticePerson(body: unknown, manifest?: IntercomOrganizationManifest): string | undefined {
  const text = boundedNoticeBody(body);
  if (!text || !manifest) return undefined;
  const candidates = new Set<string>();
  for (const match of text.matchAll(/@([a-z0-9][a-z0-9-]{0,63})/gi)) candidates.add(match[1]!);
  for (const match of text.matchAll(/'([^'\n]{1,80})'/g)) candidates.add(match[1]!);
  for (const match of text.matchAll(/[a-z0-9][a-z0-9-]{1,63}/gi)) candidates.add(match[0]!);
  for (const personId of manifest.peopleOrder) {
    if (personId !== "launcher" && candidates.has(personId) && manifest.people[personId]) return personId;
  }
  return undefined;
}

const WORK_RECOVERY_HEALTH_KINDS = new Set<string>([
  "mailbox_recipient_inactive",
  "mailbox_unit_inactive",
  "mailbox_invalid",
  "mailbox_delivery_stale",
]);
const SYSTEM_ATTENTION_HEALTH_KINDS = new Set<string>(ORGANIZATION_HEALTH_NOTICE_KINDS.filter((kind) =>
  kind !== "idle_pane_awaiting_release" && !WORK_RECOVERY_HEALTH_KINDS.has(kind)));

function healthNoticePresentation(
  envelope: OrganizationEnvelope,
  manifest?: IntercomOrganizationManifest,
): LauncherSystemNoticePresentation | undefined {
  const kind = envelope.healthIncident?.kind;
  if (!kind) return undefined;
  const personId = knownNoticePerson(envelope.body, manifest);
  const context = personId ? [`Affected person: @${manifest ? personHandle(manifest, personId) : personId}`] : [];
  if (kind === "idle_pane_awaiting_release") return {
    // The title set is a closed union; the reflection-era "🪞 Reflection
    // requested" entry left it with this packet. This is a runtime fault an
    // operator must look at, so it takes the system-attention title and earns
    // its own case purely for the specific summary and next action below —
    // which the generic system-attention card cannot say.
    title: `${CARD_GLYPHS.failure} System needs attention`,
    summary: "A work-free person still holds a pane because their idle transition has not been released.",
    context,
    nextAction: personId
      ? `ChiefD is waiting on @${manifest ? personHandle(manifest, personId) : personId}'s existing idle transition; do not kill the pane or open a second transition.`
      : "ChiefD is waiting on the affected person's existing idle transition; do not kill the pane or open a second transition.",
    impact: "Normal work can continue; only the safe pause is blocked.",
    blocked: false,
  };
  if (WORK_RECOVERY_HEALTH_KINDS.has(kind)) {
    const nextAction = kind === "mailbox_recipient_inactive"
      ? "Recall the affected person or reroute ownership; do not resend the durable message."
      : kind === "mailbox_unit_inactive"
        ? "Resume the affected unit or reroute ownership; do not resend the durable message."
        : kind === "mailbox_invalid"
          ? "Inspect the envelope metadata and repair its producer without copying message content into logs."
          : kind === "mailbox_delivery_stale"
            ? "Check the recipient runtime and provider; ChiefD will retry the existing durable message."
            : "Repair the transport or ownership fault through the normal manager workflow; do not duplicate the durable message.";
    return {
      title: "⚡ Work recovery",
      summary: "A durable work message cannot currently reach its intended owner.",
      context,
      nextAction,
      impact: "The affected delivery is blocked; unrelated work can continue.",
      blocked: true,
    };
  }
  if (SYSTEM_ATTENTION_HEALTH_KINDS.has(kind)) return {
    title: `${CARD_GLYPHS.failure} System needs attention`,
    summary: kind === "maintenance_stuck"
      ? "Session maintenance has remained unfinished beyond its recovery window."
      : "A ChiefD runtime health check found an infrastructure problem.",
    context,
    nextAction:
      "Inspect org_roster and the restricted health diagnostics, then repair the affected system path.",
    impact: "Normal work is blocked only where it depends on this system path; unrelated work can continue.",
    blocked: true,
  };
  return undefined;
}

/** Presentation-only classification for runtime-authored mail. Raw launcher
 * bodies are never rendered as person prose and unknown shapes fail to a
 * bounded neutral notice. */
export function launcherSystemNoticePresentation(
  envelope: OrganizationEnvelope,
  manifest?: IntercomOrganizationManifest,
): LauncherSystemNoticePresentation | undefined {
  if (envelope.fromPersonId !== "launcher") return undefined;
  const health = healthNoticePresentation(envelope, manifest);
  if (health) return health;
  if (envelope.healthIncident) return unknownSystemNoticePresentation();
  const body = boundedNoticeBody(envelope.body);
  if (body?.startsWith("[reminder]")) {
    return {
      title: "⏰ Reminder",
      summary: "A reminder you scheduled has come due.",
      // The person's own words, rendered verbatim. This branch is the whole
      // point of a reminder: paraphrasing it would deliver a notification that
      // the reminder fired without delivering the reminder.
      body: systemNoticeBody(body),
      // #41/#103: a reminder prompt is frequently a LIST (a checklist, a set of
      // things to re-read), and a 96-char collapsed preview would show the
      // marker and one clause — the person would learn that something was due
      // without learning what. Declaring the layout is what makes the body
      // survive collapsing; omitting it is how #103 shipped the first time.
      bodyLayout: "list",
      context: [],
      nextAction: "Do the thing you asked to be reminded of, or stop the reminder with org_stop_reminder if it is no longer useful.",
      // A reminder is never an incident. It must not render as blocked, or a
      // routine self-scheduled nudge would read to the operator as a fault.
      impact: "Normal work is not blocked; this is a reminder you scheduled for yourself.",
      blocked: false,
    };
  }
  // TOMBSTONE: the `[protected background learning failure · …]` notice. No
  // producer anywhere in this repo writes that body — there is no background
  // learning pass left to fail — and its next action told the reader to check
  // "the approved provider", an approval policy this product does not have. A
  // body nothing emits now falls to the bounded neutral notice below, which is
  // what every other unrecognized launcher body already gets. The affected
  // person line went with it: the reminder branch never carried one, so
  // `knownNoticePerson` had no other reader here.
  return unknownSystemNoticePresentation();
}

// #751/G9: THE SECOND TRANSPORT IS DELETED, NOT PORTED.
//
// `LauncherCommandResult`, `LauncherRunner`, `CoordinatedLauncherRunner` and
// `createLauncherCommandQueue` lived here. They typed and serialized a
// subprocess call into `apps/cli/src/Main.ts` — a Pi extension spawning bun to
// reach an authority the same process was already connected to over HTTP. The
// audit named it violation 2: `piing -> apps/cli` inverts the layering law.
//
// Every verb this transport carried moved onto chiefd's own API, one family at
// a time, and each helper below still records the route it went to. The queue
// existed only so one Pi process could not race its own launcher mutations;
// with no subprocess left there is nothing to serialize, and chiefd owns
// admission for the routes that replaced it.

/** The minimal `SseWatcher` surface `installOrganizationIntercom` itself calls — a test seam can substitute anything shaped like this. */
export interface SseWatcherLike {
  close(): void;
}

export interface InstallOrganizationIntercomOptions {
  environment?: Record<string, string | undefined>;
  /**
   * #827: no longer a floor cadence (the floor is deleted, D0). Kept as a
   * test/fixture-only seam: `0` disables ALL background activity — no
   * `SseWatcher` reader is constructed — which conformance's deterministic
   * single-call fixtures depend on (recorded by the deleted `conformance/lib/tool-host.ts`). Any
   * other value has no effect; there is no timer left for it to configure.
   */
  pollIntervalMs?: number;
  /** Base chiefd URL for the SSE watch endpoint; defaults to the same `chiefdEndpoint(context)` every docstore call in this file already uses. */
  sseUrl?: string;
  /** Test seam: substitutes the real `SseWatcher` construction with a fake conforming to {@link SseWatcherLike}. Production omits this and gets a real `new SseWatcher(...)`. */
  createSseWatcher?: (watcherOptions: SseWatcherOptions) => SseWatcherLike;
  /** Test seam; production keeps a short barrier while Pi restores a session. */
  idleResumeDelayMs?: number;
  clock?: () => number;
  scheduler?: OrganizationIntercomScheduler;
  /** Test seam; production performs an abortable timer outside the ledger lock. */
  /**
   * Test seam for the boot-time authority read's transient ladder
   * ({@link BOOT_TRANSIENT_RETRY_DELAYS_MS} in production): a pane booting
   * while chiefd is mid-restart retries the unreachable store instead of
   * dying at extension load (#428). Tests inject a millisecond-scale ladder.
   */
  bootTransientRetryDelaysMs?: readonly number[];
  /** Test seam; production aborts a turn with no observable progress after fifteen minutes. */
  turnWatchdogMs?: number;
  /** Test seam; production checks turn progress once a minute. 0 disables the watchdog. */
  turnWatchdogIntervalMs?: number;
}

export interface OrganizationIntercomInterval {
  unref?: () => void;
}

export interface OrganizationIntercomScheduler {
  setInterval(callback: () => void, intervalMs: number): OrganizationIntercomInterval;
  clearInterval(interval: OrganizationIntercomInterval): void;
}

/** A blocked fleet re-runs the parked-target company-maintenance reconcile at this cadence. */
export const ORGANIZATION_PARKED_MAINTENANCE_RECONCILE_INTERVAL_MS = 60_000;
/**
 * The shortest cadence the arm tool will OFFER, mirroring chiefd's
 * `MIN_REMINDER_INTERVAL_MS`.
 *
 * **The daemon is authoritative and this is a courtesy**: a caller that sends
 * less is refused server-side, with a refusal that explains the reason. This
 * value exists so the schema an agent reads does not advertise a cadence the
 * daemon will reject.
 *
 * It is twice `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`, because every fire
 * delivers a turn and every turn resets the settle countdown — so a reminder
 * inside the settle window makes parking unreachable and holds the person
 * resident for ever. That is not a hypothetical: it was measured on a live
 * company on 2026-08-27, a person woken about once a minute, each turn
 * correctly deciding nothing needed doing, $2.295 spent deciding it.
 *
 * **This is a SECOND HOME for one contract value, which is a defect class this
 * repository has already paid for.** It is therefore pinned by
 * `ReminderFloorParity.test.ts`, which resolves BOTH Rust constants from source
 * and evaluates the relation rather than matching a literal — the Rust side is
 * `2 * <lease>`, so a regex looking for a number would sail past it green for
 * ever.
 */
export const MIN_REMINDER_INTERVAL_MS = 60_000;
/**
 * And the floor a RECURRING cadence must clear: twice
 * `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`, mirroring chiefd's
 * `MIN_RECURRING_REMINDER_INTERVAL_MS`.
 *
 * The schema below cannot express "minimum depends on another field", so the
 * advertised `minimum` is the DELAY floor — legal for the one-shot case — and
 * this value is named in the description instead. A recurring request under it
 * is refused server-side with a refusal that explains why. Both numbers are
 * pinned against the Rust source by `ReminderFloorParity.test.ts`.
 */
export const MIN_RECURRING_REMINDER_INTERVAL_MS = 600_000;
/** The docstore stores every agent's `SseWatcher` subscribes to, beyond its own `mailbox/<personId>`. */
export const ORGANIZATION_SSE_MAINTENANCE_STORES = ["session-maintenance", "supervision"] as const;
/**
 * A turn that shows no message/tool progress for this long is aborted and
 * re-driven from the settled path. Above every legitimate blocking bound:
 * managed foreground Bash is capped at 4 minutes and the managed proxy gives
 * up on a provider call at 10 minutes. Provider-admission waits are excluded
 * explicitly — a saturated pool is a queue, not a stall.
 */
export const ORGANIZATION_TURN_WATCHDOG_MS = 15 * 60_000;
export const ORGANIZATION_TURN_WATCHDOG_INTERVAL_MS = 60_000;
/**
 * Pi 0.80.10's abort does not race a tool execution that never returns
 * (agent-loop awaits the tool promise; the signal is advisory), so an abort
 * can fail to end a wedged turn. After this grace with the turn still in
 * flight, the watchdog escalates to a journal event an operator or chiefd
 * health rule can act on (kill-pane; the respawn proof restores service).
 */
export const ORGANIZATION_TURN_WATCHDOG_ESCALATION_MS = 2 * 60_000;
export const ORGANIZATION_PROVIDER_FAILURE_ESCALATION_LIMIT = 3;
/**
 * A fire-and-forget Pi queue request is neither acceptance nor rejection. Its
 * in-process lease lasts until Pi accepts it or reports `agent_settled` (which
 * Pi defines as having no queued continuation left). A process restart starts
 * with an empty lease map and replays the still-durable envelope. Never expire
 * a live-process lease on a timer: a long-running turn otherwise duplicates a
 * valid follow-up while its recipient is still busy.
 */
// Organization Pi homes use native followUpMode="all", so one provider turn
// can accept this bounded FIFO group while every envelope still emits its own
// message_start receipt. The durable remainder stays on disk for the next
// settled drain instead of becoming an unbounded hidden Pi queue.
export const ORGANIZATION_MAILBOX_MAX_OUTSTANDING_DELIVERIES = 8;
/** A small inbox stays immediate. A larger normal backlog is one bounded
 * review turn, rather than a hidden FIFO of separate model turns. */
export const ORGANIZATION_MAILBOX_BATCH_THRESHOLD = 3;
export const ORGANIZATION_MAILBOX_BATCH_MAX_ITEMS = 12;
export const ORGANIZATION_IDLE_RESUME_READY_ATTEMPTS = 10;
/**
 * #827 step 7: the ONE bounded fallback attempt for an idle-resume that is
 * waiting on a session-maintenance/supervision doc-change (see
 * `scheduleIdleResume`) — guards against a missed SSE event wedging the
 * wait forever. Not a re-arming poll: this timer fires at most once per
 * wait, and the wait is cleared (fallback cancelled) the instant a matching
 * doc-change event arrives first.
 */
export const ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS = 60_000;
const ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT = 3;
const ORGANIZATION_SEND_BODY_REQUIRED_GUIDANCE = "org_send requires a non-empty body. Add the message text in body and retry once; no message was queued.";
export const ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS = [2_000, 10_000, 30_000, 60_000] as const;
/**
 * How long `before_agent_start` holds Pi's own `prompt()` call while a
 * pre-turn compaction it just claimed finishes. Pi's `compact()` is
 * fire-and-forget with `onComplete`/`onError` callbacks, and the whole point
 * of compacting here is that the turn underneath must not reach the provider
 * until the branch is smaller — so the handler waits. It waits with a bound
 * because an unbounded wait on a callback would be a NEW way to wedge a pane
 * forever, which is the exact failure class this claim point exists to
 * remove. Past the bound the turn proceeds (and, on an over-ceiling session,
 * fails at the provider exactly as it did before) while the compaction keeps
 * running and writes its own durable receipt.
 */
const ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS = 180_000;

/**
 * How long a COMPACTION may hold the settle countdown off before the ordinary
 * settle owns the person again.
 *
 * OPERATOR RULING, 2026-08-24: *"no for 2. if it reads the mail and it's
 * thinking, leave it until it settles then start the timer."* The countdown
 * must not run while somebody is working, and a compaction is work — it is the
 * longest, quietest work a pane does, because it emits no turn events at all
 * while it runs.
 *
 * Measured on a live box: exec-runner was mid-compaction at ~90% of a
 * 1M context when its window was reaped, about 100 seconds after the countdown
 * showed 1m43s. The transcript survived; the COMPACTION did not, after paying
 * for a summarize call over 909k tokens. That produces a livelock — every wake
 * re-triggers auto-compact, the next countdown kills it again, and the session
 * that most needs compacting can never finish one.
 *
 * A CEILING, and it is 3x {@link ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS} for
 * that constant's own stated reason: an unbounded hold would be a NEW way to
 * wedge a pane for ever. The ruling is about not dying MID-WORK, not about
 * never parking — a compaction that hangs still settles, ten minutes later.
 */
const ORGANIZATION_COMPACTION_BEAT_CEILING_MS = 600_000;

const ORGANIZATION_FOREGROUND_BASH_DEADLINE_GUIDANCE = `Managed foreground Bash is limited to ${ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS} seconds so queued organization messages cannot be starved by one tool call. Arm a durable reminder with org_create_reminder for a future-time wait. Use a truly detached process with redirected stdio and an explicit supervisor only when a persistent process is the deliverable.`;

interface SessionMaintenanceStartRetry {
  requestId?: string;
  failures: number;
  nextAttemptAt: number;
}

interface SessionMaintenanceDeferralRetry {
  requestId?: string;
  claim?: SessionMaintenanceClaim;
  failures: number;
  nextAttemptAt: number;
  reported: boolean;
}

interface NativeCompactionLease {
  requestId?: string;
  sessionId?: string;
  anchorEntryId?: string;
  completedEntryId?: string;
  /**
   * Resolves when Pi's fire-and-forget `compact()` has called back, either
   * way. Only the pre-turn claim awaits it: `before_agent_start` must not let
   * the turn reach the provider until the branch it is about to send has
   * actually shrunk. Every other caller keeps the existing fire-and-forget
   * behavior and ignores this.
   */
  settled?: Promise<void>;
}

// TOMBSTONE: `NativeFreshSessionLease`, the three ids that tracked one native
// session replacement from queue to host request. Deleted with the machinery.

function requiredEnvironment(environment: Record<string, string | undefined>, name: string): string {
  const value = environment[name]?.trim();
  if (!value) throw new Error(`${name} is required for an organization-managed Pi runtime`);
  return value;
}

/**
 * Recover the current Pi process's pane from the runtime when Pi has sanitized both
 * the raw pane variable and the launch wrapper's preserved token. Pi runs
 * beneath the bounded stderr-capture wrapper, so its direct pid need not be
 * the runtime pane pid; walk only its own process parents until that pane root.
 * This discovers identity only: the fenced CLI child still proves its own
 * process ancestry and launcher tags before it can mutate durable state.
 */
export function authoritativeRuntimePane(
  socketName: string | undefined,
  processId: number = process.pid,
  run: typeof spawnSync = spawnSync,
): string | undefined {
  if (!socketName) return undefined;
  // `tmux` is the literal executable name, not a naming choice. #751/P9's
  // tmux -> runtime sweep rewrote this argv[0] to "runtime", a binary that does
  // not exist, which turned every pane-recovery attempt into an ENOENT that
  // this function reports as "no pane" — indistinguishable from a genuinely
  // unfound pane. The flags below (`-L`, `list-panes`, `#{pane_id}`) are tmux's
  // own, which is the proof the program being spawned has to be tmux.
  const result = run("tmux", ["-L", socketName, "list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}"], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  });
  if (result.status !== 0 || typeof result.stdout !== "string") return undefined;
  const panesByPid = new Map<string, string>();
  for (const line of result.stdout.split("\n")) {
    const [paneId, panePid] = line.split("\t");
    if (panePid && /^%\d+$/.test(paneId ?? "")) panesByPid.set(panePid, paneId);
  }
  let currentPid = String(processId);
  const seen = new Set<string>();
  for (let depth = 0; depth < 32 && !seen.has(currentPid); depth += 1) {
    seen.add(currentPid);
    const pane = panesByPid.get(currentPid);
    if (pane) return pane;
    const parent = run("ps", ["-o", "ppid=", "-p", currentPid], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    if (parent.status !== 0 || typeof parent.stdout !== "string") return undefined;
    const nextPid = parent.stdout.trim();
    if (!/^\d+$/.test(nextPid) || nextPid === "0" || nextPid === currentPid) return undefined;
    currentPid = nextPid;
  }
  return undefined;
}

/**
 * The identity half of an install's context, parsed from its environment.
 *
 * Pure and synchronous: every field here is something the process was TOLD
 * (which company, which person, which pane). It deliberately carries no
 * `chiefdUrl` — where that company's daemon is listening is not something an
 * environment can be trusted to know, and is resolved by
 * {@link resolveOrganizationRuntimeContext}.
 */
export function readOrganizationRuntimeContext(
  environment: Record<string, string | undefined> = process.env,
): OrganizationRuntimeContext {
  const organizationDir = requiredEnvironment(environment, "ORG_LAUNCHER_ORG_DIR");
  const identityDir = requiredEnvironment(environment, "ORG_LAUNCHER_IDENTITY_DIR");
  const organization = requiredEnvironment(environment, "ORG_LAUNCHER_ORGANIZATION");
  const inheritedLauncherRoot = requiredEnvironment(environment, "ORG_LAUNCHER_ROOT");
  if (!isAbsolute(organizationDir) || !isAbsolute(identityDir) || !isAbsolute(inheritedLauncherRoot)) throw new Error("Organization runtime paths must be absolute");
  // Pi may remove TMUX_PANE after the launcher wrapper begins. A real raw
  // TMUX_PANE wins whenever present; otherwise only the wrapper's preserved
  // token is accepted. Never accept malformed identity as a fallback.
  //
  // `TMUX_PANE` is set by TMUX ITSELF inside every pane; it is not a name this
  // repo gets to choose. #751/P9's sweep renamed it to `RUNTIME_PANE` on both
  // the read and the write side here, which kept THIS file self-consistent
  // while silently deleting the raw-pane tier of the ladder — nothing sets
  // `RUNTIME_PANE`. The same sweep renamed the read inside the pane-attestation
  // shell, whose `exit 125` guard would then have fired on every single spawn.
  const rawPane = environment.TMUX_PANE?.trim();
  const preservedPane = environment.ORG_LAUNCHER_PANE_ID?.trim();
  const runtimePane = rawPane
    || preservedPane
    || authoritativeRuntimePane(environment.ORG_LAUNCHER_RUNTIME_SOCKET?.trim());
  if (runtimePane !== undefined && !/^%\d+$/.test(runtimePane)) {
    throw new Error("TMUX_PANE or ORG_LAUNCHER_PANE_ID must be a runtime pane id");
  }
  const context: OrganizationRuntimeContext = {
    organizationDir,
    identityDir,
    organization,
    personId: requiredEnvironment(environment, "ORG_LAUNCHER_PERSON"),
    launcherRoot: inheritedLauncherRoot,
    runtimeSocket: environment.ORG_LAUNCHER_RUNTIME_SOCKET?.trim() || undefined,
    runtimeSession: environment.ORG_LAUNCHER_RUNTIME_SESSION?.trim() || undefined,
    runtimePane,
  };
  if (Boolean(context.runtimeSocket) !== Boolean(context.runtimeSession)) {
    throw new Error("ORG_LAUNCHER_RUNTIME_SOCKET and ORG_LAUNCHER_RUNTIME_SESSION must be provided together");
  }
  return context;
}

/**
 * THE ONE PLACE THIS EXTENSION LEARNS WHERE ITS COMPANY'S DAEMON IS, AND WHAT
 * ITS COMPANY IS CALLED ON THE WIRE.
 *
 * # What this replaces, and why each shape could not be patched
 *
 * The address first arrived as `ORG_CHIEFD_URL`, one process-global
 * environment variable stamped in by the chiefd that spawned the pane. That is
 * exactly right for one deployment and one only — one Pi process per tmux
 * pane, one company per process. `apps/web` runs MANY companies in one server
 * process, and there is no value that variable can hold which is correct for
 * more than one of them. The failure it produced is the worst available shape:
 * SILENT. A wrong daemon ANSWERS. It does not refuse, it does not 500, it does
 * not time out — it commits the mutation into another company and returns 200.
 *
 * It then became a beacond lookup by SLUG. That fixed the per-process problem
 * and kept a subtler one: a slug is not an identity. Two directories may hold
 * companies called the same thing, and the registry had exactly one answer for
 * the word — so the second company's panes reached the first company's daemon,
 * silently, in the same way.
 *
 * # Why the rendezvous file, and not the registry
 *
 * A pane's cwd IS its company directory, and a directory already knows where
 * its own daemon is: `chiefd` publishes `<dir>/.chief/run/daemon.json` with
 * the URL it bound AND the company key it serves. Reading it is one local
 * file read — no network, no registry on the hot path between a pane and its
 * own company, and no question whose answer could be about a different
 * company. beacond survives for the box-wide question ("what is running
 * anywhere"), which `chief ls` and `apps/web` ask and a pane never does.
 *
 * The file is a POINTER, never authority: it says a daemon was last seen at
 * this address, and a stale one fails the way any dead address does. It also
 * carries the directory it describes, so a rendezvous copied along with a
 * project is REFUSED rather than followed into the original company's daemon
 * (`parseDaemonRendezvous`).
 *
 * Resolution happens once per INSTALL rather than once per call because an
 * install belongs to one company for its whole life; per-company is the
 * property that was missing, not per-call.
 */
export function resolveOrganizationRuntimeContext(
  environment: Record<string, string | undefined> = process.env,
): OrganizationRuntimeContext {
  const context = readOrganizationRuntimeContext(environment);
  const rendezvous = readDaemonRendezvous(context.organizationDir);
  if (!rendezvous) return context;
  return { ...context, chiefdUrl: rendezvous.url, companyKey: rendezvous.key };
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}

/**
 * org-ops R2 — the people a department removal would fire, split into the unit's
 * own head and everyone else (staff + every person in every descendant unit).
 * Pure and local: this extension is copied into each Pi home and cannot import
 * from `../src/`, so the removal blast-radius walk is mirrored here (the runtime
 * twin lives in `src/organization/org-staffing.ts#describeUnitRemovalImpact`).
 */
function intercomUnitRemovalImpact(
  manifest: IntercomOrganizationManifest,
  unitId: string,
): { headPersonId?: string; memberPersonIds: string[]; memberNames: string[] } {
  const removed = new Set<string>([unitId]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const [id, dept] of Object.entries(manifest.departments)) {
      if (!removed.has(id) && dept.parentDepartmentId && removed.has(dept.parentDepartmentId)) {
        removed.add(id);
        grew = true;
      }
    }
  }
  const headPersonId = manifest.departments[unitId]?.headPersonId;
  const memberPersonIds = manifest.peopleOrder.filter((id) => {
    const person = manifest.people[id];
    return person !== undefined && removed.has(person.departmentId) && id !== headPersonId;
  });
  const memberNames = memberPersonIds.map((id) => manifest.people[id]?.name ?? id);
  return { headPersonId, memberPersonIds, memberNames };
}

interface IntercomManifestWire {
  manifest: IntercomOrganizationManifest;
  /** The org-events sequence that fences an atomic org-ops mutation. */
  seq: number;
  /** The exact typed-route identity for this company/root pair. */
  key: string;
}

/**
 * The company key (`sha256(<dir>)[..12]`) every chiefd route resolves its
 * authority by.
 *
 * READ, never derived. It was `documentKey(slug, dirname(organizationDir))`,
 * rebuilt independently at a dozen call sites in this file alone; chiefd
 * matches `req.slug` against `org_documents_slug`, and a key built slightly
 * differently does not fail loudly — it matches no live company, so the route
 * 404s and the write silently never happens. The value now has one producer
 * and reaches this pane in the rendezvous its own daemon published.
 *
 * A context with no key has no daemon URL either (they arrive together, from
 * one file read), so it refuses with the same error every other call would.
 */
function companyKeyOf(context: OrganizationRuntimeContext): string {
  const key = context.companyKey?.trim();
  if (!key) throw new OrgChiefdUrlUnsetError();
  return key;
}

async function readIntercomManifestWire(context: OrganizationRuntimeContext): Promise<IntercomManifestWire> {
  const key = companyKeyOf(context);
  let manifest: IntercomOrganizationManifest;
  let seq: number;
  try {
    const wire = await chiefdPostJson<{ found: boolean; manifest?: string; seq?: number }>(
      chiefdEndpoint(context), "/v1/org/manifest/read", { slug: key },
    );
    if (!wire.found || wire.manifest === undefined || wire.seq === undefined || !Number.isSafeInteger(wire.seq)) {
      throw new Error("normalized manifest is absent");
    }
    manifest = JSON.parse(wire.manifest) as IntercomOrganizationManifest;
    seq = wire.seq;
  } catch (error) {
    throw new Error(`Cannot read normalized organization authority '${context.organization}': ${error instanceof Error ? error.message : String(error)}`);
  }
  if (manifest.kind !== "organization" || manifest.slug !== context.organization) {
    throw new Error(`Normalized organization runtime identity does not match '${context.organization}'`);
  }
  if (!manifest.people?.[context.personId]) throw new Error(`Unknown organization person '${context.personId}'`);
  // Warm the synchronous render-time cache (`personMentionColorizer`) on every
  // successful load — never on a failed/partial read.
  lastKnownIntercomManifest.set(context.organization, manifest);
  return { manifest, seq, key };
}

export async function loadIntercomOrganization(context: OrganizationRuntimeContext): Promise<IntercomOrganizationManifest> {
  return (await readIntercomManifestWire(context)).manifest;
}

/* TOMBSTONE (chief-home-is-cwd §4c): `ceoOnlyBootInFlight()` stood here and
 * read `/v1/org/ceo-boot-lease/read` directly, duplicating the expiry rule so
 * this copied-into-every-Pi-home file needed no import. Route, row and lease are
 * deleted with the daemon-side CEO boot; the daemon brings up no pane, so no
 * boot can be mid-flight. Fleet safety never rested on this read — the
 * launch-intent fence, which still fails closed, is what carries it. */

function currentPerson(context: OrganizationRuntimeContext, manifest: IntercomOrganizationManifest): PersonRecord {
  const person = manifest.people[context.personId];
  if (!person) throw new Error(`Unknown organization person '${context.personId}'`);
  if (person.employmentState === "departed") throw new Error(`Person '${context.personId}' is no longer employed by '${manifest.slug}'`);
  return person;
}

function manager(person: PersonRecord): boolean {
  return person.kind === "executive" || person.kind === "head";
}

/**
 * The department window a person's pane belongs to, DERIVED.
 *
 * A non-head sits in their ASSIGNED department; a head sits in their
 * department's parent; a top-level head sits at the root.
 *
 * # Why this is derived and not read
 *
 * chiefd used to persist this answer as `PersonActivityState.lastPaneDepartmentId`
 * and publish it on the activity document. #751/P9 deleted both the rule and its
 * two SQL columns, because a stored answer is only rewritten when the activity
 * ledger is: reparent a department and the column still names the old parent
 * until the next reconcile, so a reader placed a head's pane in a window the
 * tree no longer describes. `chiefd-core`'s own
 * `a_reparent_moves_the_head_for_both_sides_because_nothing_is_stored` pins the
 * two answers agreeing now that nothing is stored.
 *
 * This is the same rule as `chief-cli/src/placement.rs`'s `pane_department_id`
 * and it is deliberately a second DERIVATION rather than a second stored copy —
 * two derivations of one rule from one manifest cannot drift the way a
 * derivation and a persisted column did.
 *
 * Returns `undefined` when the person, or the department they head, names a
 * department the manifest does not declare. Callers must treat that as "cannot
 * place", never as a default window.
 */
function personDepartmentId(
  manifest: IntercomOrganizationManifest,
  personId: string,
): string | undefined {
  const headed = Object.values(manifest.departments).find(
    (department) => department.headPersonId === personId,
  );
  if (headed) {
    return headed.parentDepartmentId ?? manifest.rootDepartmentId;
  }
  const person = manifest.people[personId];
  if (!person) return undefined;
  return manifest.departments[person.departmentId] ? person.departmentId : undefined;
}

function directManagerId(manifest: IntercomOrganizationManifest, assignee: PersonRecord): string | undefined {
  const headed = Object.values(manifest.departments).find((department) => department.headPersonId === assignee.id);
  if (headed) {
    const parent = headed.parentDepartmentId ? manifest.departments[headed.parentDepartmentId] : undefined;
    return parent?.headPersonId;
  }
  return manifest.departments[assignee.departmentId]?.headPersonId;
}

function headedDepartmentId(manifest: IntercomOrganizationManifest, managerPersonId: string): string | undefined {
  return Object.values(manifest.departments).find((department) => department.headPersonId === managerPersonId)?.id;
}

/** How many times to retry the boot-time manifest LOAD before deciding a
 * structural-root check has persistently failed (#270). Small and immediate —
 * this runs synchronously during tool registration, so it must not block the
 * pane boot; a few retries catch a momentary mid-write inconsistency, and a
 * persistently-unreachable/unknown-company chiefd falls through to the
 * defensive (fail-open + loud-log) branch rather than silently locking out the
 * CEO's own tools. */
const STRUCTURAL_ROOT_PROBE_ATTEMPTS = 3;

/**
 * #270: resolve whether the installing person is the structural root, keeping a
 * TRANSIENT manifest-read failure distinct from a GENUINE non-root person. The
 * ONLY genuine-non-root signal is a SUCCESSFUL manifest load whose person
 * resolves to an active actor with a manager (`directManagerId` defined) — that
 * hides the CEO-only tools silently, unchanged, so a real department head keeps
 * NOT seeing them (the #307 surface contract). Every OTHER outcome — the
 * manifest read failing, an identity mismatch, an unknown/departed installer — is
 * "could not determine": it is retried across a few attempts and, if it never
 * resolves, reported as `readFailed` so the caller can fail OPEN (register the
 * tools defensively) and log loudly, rather than the old silent lockout of the
 * real CEO on a boot-time blip.
 */
export async function resolveInstallerStructuralRoot(
  context: OrganizationRuntimeContext,
): Promise<{ isRoot: boolean; readFailed: boolean; attempts: number; error?: unknown }> {
  let lastError: unknown;
  for (let attempt = 1; attempt <= STRUCTURAL_ROOT_PROBE_ATTEMPTS; attempt += 1) {
    try {
      const manifest = await loadIntercomOrganization(context);
      const person = currentPerson(context, manifest);
      return { isRoot: directManagerId(manifest, person) === undefined, readFailed: false, attempts: attempt };
    } catch (error) {
      lastError = error;
    }
  }
  return { isRoot: false, readFailed: true, attempts: STRUCTURAL_ROOT_PROBE_ATTEMPTS, error: lastError };
}

/** Human-facing unit creation defaults to the caller's natural management root. */
async function launchParentUnitId(context: OrganizationRuntimeContext, requestedParentId: unknown): Promise<string> {
  const manifest = await loadIntercomOrganization(context);
  const person = currentPerson(context, manifest);
  // No role gate. Every node may create a department beneath itself — that is
  // how a leaf becomes a parent — and the SCOPE check below is the only
  // restriction: you may act at or under your own subtree, never above it.
  const requested = typeof requestedParentId === "string" ? requestedParentId.trim() : "";
  if (person.kind === "executive") {
    // The company slug is a natural CEO alias for the root executive unit.
    if (!requested || requested === manifest.slug || requested === context.organization) return manifest.rootDepartmentId;
    return requested;
  }
  if (requested) return requested;
  const managedRoot = authorityRootDepartmentId(manifest, person);
  if (!managedRoot) throw new Error(`Cannot determine the department '${person.id}' belongs to`);
  return managedRoot;
}

/**
 * Where a person may attach a NEW department — the one place a leaf may grow.
 *
 * A head attaches beneath the department it heads; everyone else attaches
 * beneath the department it is assigned to. This is growth DOWNWARD only, and
 * it is deliberately more permissive than [`departmentIsInScope`]: creating a
 * child unit takes no authority over anybody who already exists, and the
 * creator becomes the new unit's head, which is where its authority then comes
 * from.
 *
 * The distinction is the whole tree model. A leaf may become a parent — that
 * is this function — but it may never reach sideways at a peer or upward at its
 * own manager, which is why `departmentIsInScope` stays rooted at the headed
 * unit. Before this existed a non-head had neither, and the operator's heads
 * under a chief of staff had no create-department at all.
 */
function authorityRootDepartmentId(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
): string | undefined {
  return headedDepartmentId(manifest, person.id) ?? person.departmentId;
}

/**
 * Why a department id is unusable for this person. `undefined` means usable.
 *
 * The two answers are kept APART because collapsing them is what made a new
 * company's CEO unable to hire (#1048). `departmentIsInScope` returned one
 * boolean, an unknown id produced `false` on its very first line — before the
 * executive short-circuit that grants the CEO the whole company — and every
 * caller rendered that `false` as "you do not manage this department". The CEO
 * had the authority all along; the id simply did not exist. An authority
 * message for a typo sends the caller hunting a permission it already holds.
 */
type DepartmentScopeDenial = "unknown-department" | "out-of-scope";

export function departmentScopeDenial(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
  departmentId: string,
): DepartmentScopeDenial | undefined {
  if (!manifest.departments[departmentId]) return "unknown-department";
  if (person.kind === "executive") return undefined;
  // Rooted at the department this person HEADS, deliberately, and NOT at the one
  // they are assigned to. A leaf's subtree is itself: its own department
  // contains its manager, so assignment-rooted scope would let a worker act on
  // the head above it — the one direction the tree model forbids.
  const root = headedDepartmentId(manifest, person.id);
  if (!root) return "out-of-scope";
  let cursor: DepartmentRecord | undefined = manifest.departments[departmentId];
  const seen = new Set<string>();
  while (cursor) {
    if (cursor.id === root) return undefined;
    if (seen.has(cursor.id) || !cursor.parentDepartmentId) return "out-of-scope";
    seen.add(cursor.id);
    cursor = manifest.departments[cursor.parentDepartmentId];
  }
  return "out-of-scope";
}

/** The plain predicate, for the call sites that FILTER rather than refuse —
 *  a person the caller cannot see is simply left out of a list, and "why" has
 *  no reader there. Everything that raises an error asks
 *  {@link departmentScopeDenial} instead. */
function departmentIsInScope(manifest: IntercomOrganizationManifest, person: PersonRecord, departmentId: string): boolean {
  return departmentScopeDenial(manifest, person, departmentId) === undefined;
}

/** The department ids this person may name, in the manifest's own order. */
function departmentIdsInScope(manifest: IntercomOrganizationManifest, person: PersonRecord): string[] {
  const order = manifest.departmentOrder.filter((id) => manifest.departments[id]);
  const ids = order.length ? order : Object.keys(manifest.departments);
  return ids.filter((id) => departmentIsInScope(manifest, person, id));
}

/** Lowercase, non-alphanumerics collapsed to one hyphen — enough to see that
 *  "Belfort Brothers Capital" and `belfort-brothers-capital` are the same
 *  words. It matches ids to display names for a HINT only; nothing is resolved
 *  or accepted through it. */
function departmentMatchKey(value: string): string {
  return value.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

/**
 * The id the caller most likely meant, when what it passed matches no id.
 *
 * This is the exact trap the incident walked into: the root department's id is
 * `executive`, its NAME is the company display name, and `org_roster` shows the
 * name. A CEO read the company off its own contract and passed the company
 * slug. Naming the real id turns a dead end into a one-line correction — and it
 * stays a correction: the wrong id is still refused.
 */
function nearestDepartmentHint(
  manifest: IntercomOrganizationManifest,
  departmentId: string,
): string | undefined {
  const key = departmentMatchKey(departmentId);
  if (!key) return undefined;
  if (key === departmentMatchKey(manifest.slug) || key === departmentMatchKey(manifest.name)) {
    return `The root department id is '${manifest.rootDepartmentId}' — '${departmentId}' names the company, not a department.`;
  }
  const byName = Object.values(manifest.departments).find(
    (department) => departmentMatchKey(department.name) === key,
  );
  if (byName) return `'${departmentId}' is the NAME of department '${byName.id}': pass the id.`;
  return undefined;
}

/**
 * The ONE sentence a caller-supplied department id that does not exist gets.
 *
 * `action` completes "Departments you may <action>" — "hire into", "act on".
 */
export function unknownDepartmentMessage(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
  departmentId: string,
  action: string,
): string {
  const hint = nearestDepartmentHint(manifest, departmentId);
  const available = departmentIdsInScope(manifest, person);
  const list = available.length
    ? `Departments you may ${action}: ${available.join(", ")}.`
    : `You may ${action} no department.`;
  // "Unknown department", not "No such department": every caller of this file
  // has said "Unknown department '<id>'" since before the split, and the
  // contract suite asserts that wording for the unknown-source refusal. The
  // defect was that an unknown id was reported as an AUTHORITY failure, not
  // the two words that named it correctly — so the words stay and the hint and
  // the id list are added after them.
  return `Unknown department '${departmentId}'.${hint ? ` ${hint}` : ""} ${list}`;
}

/**
 * How THIS person reaches a hire, from where it actually sits.
 *
 * One static sentence used to tell everybody to "create a department beneath
 * yourself with org_add_department (naming yourself as its existing head)".
 * chiefd refused that create for anybody who already headed a department —
 * `head-not-eligible` — and still refuses it for the CEO
 * (`exec-root-protected`), which is a product invariant and stays. So the
 * advice given to a head was guaranteed to fail, and the CEO of every new
 * company met it on its first hire.
 *
 * The heads-nothing branch is now TRUE for everybody it is offered to. Until
 * 2026-08-13 chiefd also refused it for any person homed in the executive
 * root, so a Chief of Staff who headed nothing was told to do something the
 * backend would not allow — this advice was correct and the guard was wrong.
 * Only the CEO is refused now.
 *
 * The sitting-head branch changed for a different reason: a head now HAS a way
 * to lead a different department, by saying what becomes of the one they leave
 * (`vacates`). It is offered second, because growing a department
 * beneath the one you already head keeps both and is what a head usually
 * wants.
 *
 * The CEO is the exception to that second path and is never offered it. The
 * CEO always heads the root, the root can never be vacated, and advice that
 * offered it would send the one person who cannot take that path down it.
 */
export function hiringPathAdvice(manifest: IntercomOrganizationManifest, person: PersonRecord): string {
  const headed = headedDepartmentId(manifest, person.id);
  if (headed) {
    const grow = `You head '${headed}': hire into '${headed}' directly, or create a department beneath it with org_add_department and a NEW head (the head argument), then hire into that.`;
    if (headed === manifest.rootDepartmentId) {
      return `${grow} You always head the company root, so do not name yourself as the head of a new department.`;
    }
    return `${grow} To lead a different department yourself instead, name yourself as its existing head AND say what becomes of '${headed}' with the vacates argument — hand it to one of its members, or dissolve it if you are its last one.`;
  }
  return "You head no department yet: create one beneath yourself with org_add_department, naming yourself as its existing head (the existingHeadPersonId argument), then hire into that.";
}

/**
 * What ONE person may do about structure and staffing, read off the gates.
 *
 * Every field here is the ANSWER of a function that gates a real call —
 * `headedDepartmentId`, `authorityRootDepartmentId` (what `launchParentUnitId`
 * uses), and `departmentScopeDenial` through `departmentIsInScope`. Nothing
 * here re-states a rule. That is the whole design: a roster that describes the
 * authority model in its own words drifts from the model, and a drifted roster
 * is worse than no roster field, because it will be believed.
 */
export interface PersonAuthorityView {
  /** The department this person heads, if any. */
  headedDepartmentId?: string;
  /** Where a NEW department attaches for this person. */
  createBeneathDepartmentId?: string;
  /** Every department id the scope gate admits, in the manifest's own order. */
  hireDepartmentIds: string[];
  /** True when the gate admits EVERY department that exists. */
  companyWide: boolean;
}

export function personAuthority(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
): PersonAuthorityView {
  const headed = headedDepartmentId(manifest, person.id);
  const createBeneath = authorityRootDepartmentId(manifest, person);
  const departmentIds = Object.keys(manifest.departments);
  return {
    ...(headed ? { headedDepartmentId: headed } : {}),
    ...(createBeneath ? { createBeneathDepartmentId: createBeneath } : {}),
    hireDepartmentIds: departmentIdsInScope(manifest, person),
    companyWide: departmentIds.length > 0
      && departmentIds.every((id) => departmentIsInScope(manifest, person, id)),
  };
}

/**
 * The same answer as {@link hiringPathAdvice}, worded for a reader looking at
 * SOMEBODY ELSE.
 *
 * This is the missing feedback loop. A refusal teaches the person refused; it
 * teaches nothing to a manager deciding whom to hand the work to, because that
 * manager makes no call. Live incident: a CEO told the operator that its Chief
 * of Staff "does not hold the org-management tools" to create a department, and
 * did the work itself. There is no role gate — the Chief of Staff holds
 * `org_add_department` and `org_hire` like everybody else, and the accepted
 * path was open to him. He never attempted, so nothing ever corrected the
 * belief. The roster is where a manager already looks, so the answer belongs
 * there.
 *
 * Third person, because the reader is not the subject. Short, because this
 * lands on every person in every roster read.
 */
export function personAuthorityText(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
): string {
  const view = personAuthority(manifest, person);
  const hire = view.hireDepartmentIds.length === 0
    ? "may hire into no department yet"
    : view.companyWide
      ? "may hire anywhere in the company"
      : view.headedDepartmentId
        ? `may hire at or under ${view.headedDepartmentId}`
        : `may hire into ${view.hireDepartmentIds.join(", ")}`;
  if (view.headedDepartmentId) {
    // A head still heads ONE department, so the create offered first is the one
    // that keeps both: a NEW head beneath it. Moving their own headship is
    // possible now and is named as what it costs — the department they leave
    // changes hands or ends. The CEO is never offered it: the root is never
    // vacated. The whole field stays inside the roster line's 160-character
    // budget, which is why the clause names no id — the headed department is
    // already named immediately before it.
    const move = view.headedDepartmentId === manifest.rootDepartmentId
      ? ""
      : ", or vacate it to head another department";
    return `heads ${view.headedDepartmentId} · may add departments under it with a new head${move} · ${hire}`;
  }
  if (!view.createBeneathDepartmentId) return `heads no department · may add no department · ${hire}`;
  // The accepted path, named exactly as the refusal names it. `hire` is
  // appended only when the gate already admits a department — for everybody
  // else the sentence ends where their authority does.
  const grow = `heads no department · may add one under ${view.createBeneathDepartmentId} with org_add_department, as its own head, then hire into it`;
  return view.hireDepartmentIds.length === 0 ? grow : `${grow} · ${hire}`;
}

function departmentIsActive(manifest: IntercomOrganizationManifest, departmentId: string): boolean {
  let cursor: DepartmentRecord | undefined = manifest.departments[departmentId];
  const seen = new Set<string>();
  while (cursor) {
    if (seen.has(cursor.id) || cursor.state !== "active") return false;
    seen.add(cursor.id);
    cursor = cursor.parentDepartmentId ? manifest.departments[cursor.parentDepartmentId] : undefined;
  }
  return seen.size > 0;
}

function operationalIntercomManager(manifest: IntercomOrganizationManifest, personId: string): boolean {
  const person = manifest.people[personId];
  if (!person || person.employmentState !== "active"
    || (person.kind !== "executive" && person.kind !== "head")
    || !departmentIsActive(manifest, person.departmentId)) return false;
  const headed = Object.values(manifest.departments).find((department) => department.headPersonId === personId);
  return !headed || departmentIsActive(manifest, headed.id);
}

function messageWakeDisposition(manifest: IntercomOrganizationManifest, personId: string): { wake: boolean; guidance?: string } {
  const person = manifest.people[personId];
  if (!person || person.employmentState === "departed") {
    return { wake: false, guidance: `@${personHandle(manifest, personId)} has departed; reroute the durable message to an employed owner.` };
  }
  if (person.employmentState !== "active") {
    // #401: never say the message "remains queued" — it is already durably
    // delivered (the mailbox write is the delivery authority); only the
    // recipient's WAKE-UP is on hold, which this guidance already states.
    return { wake: false, guidance: `@${personHandle(manifest, personId)} is benched; recall them or reroute ownership.` };
  }
  if (!departmentIsActive(manifest, person.departmentId)) {
    return { wake: false, guidance: `@${personHandle(manifest, personId)}'s department is inactive; resume the unit or reroute ownership.` };
  }
  return { wake: true };
}

async function requireManagedDepartment(context: OrganizationRuntimeContext, departmentId: string): Promise<IntercomOrganizationManifest> {
  const manifest = await loadIntercomOrganization(context);
  const person = currentPerson(context, manifest);
  // An id that names nothing is not an authority problem, and reporting it as
  // one is what sent a CEO hunting a permission it already had (#1048).
  if (departmentScopeDenial(manifest, person, departmentId) === "unknown-department") {
    throw new CallerRefusal(unknownDepartmentMessage(manifest, person, departmentId, "act on"));
  }
  // SCOPE, and nothing else. `manager(person)` — a kind of `executive` or
  // `head` — stood beside this check and decided nothing: `departmentIsInScope`
  // already returns false for a non-executive who heads no department, so
  // passing scope IMPLIES heading one, and every chiefd write that makes
  // somebody a head sets kind Head inside the same transaction (the only two
  // `set_department_head` sites, `org_ops.rs:1075`/`:3705`, each pair with a
  // `set_person_kind(Head)`; a hired head's seed is validated as `Head`;
  // genesis writes `Executive` for the CEO). "Heads a unit while recorded a
  // worker" is a state chiefd never writes, so the title half could only ever
  // change the answer in a company whose manifest was already wrong — and
  // there it refused a person their OWN subtree.
  //
  // It is deleted rather than kept because a title check in an authority gate
  // is what the operator ruling of 2026-08-13 (`AGENTS.md`) forbids: authority
  // is the subtree you head, never the job title, and no tool is "CEO-level"
  // or "head-level". chiefd remains the authority and re-checks every
  // mutation; this is a pre-flight.
  if (!departmentIsInScope(manifest, person, departmentId)) {
    throw new CallerRefusal(`'${person.id}' does not manage department '${departmentId}'`);
  }
  return manifest;
}

/**
 * The CREATE-path authority: where this person may attach a NEW department.
 *
 * Deliberately NOT {@link requireManagedDepartment}. That predicate answers
 * "do you manage this existing unit", and it is rooted at the unit you HEAD —
 * correct for stopping, removing, reparenting or staffing something that
 * already exists, and wrong for creation. Creating a child unit takes
 * authority over nobody: nothing that already exists changes hands, and the
 * creator heads the new unit, which is where its authority then comes from.
 *
 * So the accepted parent is {@link authorityRootDepartmentId} — the unit you
 * head, or failing that the unit you are assigned to — or anything already
 * inside your management scope. That is the one place a leaf may grow, and it
 * grows DOWNWARD only: a peer, an ancestor, or any unit outside the subtree is
 * still refused, by exactly the checks that refused it before.
 *
 * Before this existed, `org_add_department` routed through the manager-gated
 * predicate, so the documented rule "a leaf may become a parent" was
 * unreachable: `authorityRootDepartmentId` computed the right parent and the
 * very next call refused it for not being headed.
 */
async function requireDepartmentCreationParent(
  context: OrganizationRuntimeContext,
  parentDepartmentId: string,
): Promise<IntercomOrganizationManifest> {
  const manifest = await loadIntercomOrganization(context);
  const person = currentPerson(context, manifest);
  // Say WHICH check failed: an unknown parent is not an authority problem, and
  // reporting it as one sends the caller hunting a permission they do have.
  if (!manifest.departments[parentDepartmentId]) {
    throw new CallerRefusal(unknownDepartmentMessage(manifest, person, parentDepartmentId, "create a department beneath"));
  }
  const authorityRoot = authorityRootDepartmentId(manifest, person);
  if (parentDepartmentId === authorityRoot) return manifest;
  if (departmentIsInScope(manifest, person, parentDepartmentId)) return manifest;
  throw new CallerRefusal(
    `'${person.id}' may create a department beneath ${authorityRoot ? `'${authorityRoot}'` : "no department"} or anything under it, not beneath '${parentDepartmentId}'`,
  );
}

// TOMBSTONE: `requireManagedPerson`. It read the manifest a second time and
// asked `manager(managerPerson)` — a JOB TITLE — beside the scope check, then
// refused with "'x' does not manage person 'y'". Its last caller left with the
// staffing family's move onto chiefd routes, and it stood here with none. The
// operator ruling of 2026-08-13 (`AGENTS.md`) is why it is deleted rather than
// kept: authority is the subtree you head, never your kind, and a role-gated
// twin of a scope-only check is a second opinion waiting for a caller.
// `requireManagedTarget` below is the live check, and it asks scope alone.

/**
 * Does this staffing caller manage the named person? Answered against the
 * manifest the tool has ALREADY read through `staffingAuthority`. Two reads of
 * the same authority inside one tool call are two chances to disagree, and the
 * second is what the mutation would then be authorized against.
 *
 * SCOPE ONLY. No kind, no title, and no executive-root exemption: a person
 * homed in the root department is an ordinary person here, exactly as the
 * ruling requires.
 */
function requireManagedTarget(gate: StaffingAuthority, personId: string): PersonRecord {
  const target = gate.manifest.people[personId];
  // AN ABSENT PERSON IS NOT AN AUTHORITY FAILURE, and collapsing the two is
  // exactly the defect #1048 fixed for DEPARTMENTS and nobody carried across to
  // people. It cost a live company: a CEO created a department with a new head
  // and staff, went to bring them up, and was told
  // `'ceo' does not manage person 'ada-lovelace'`.
  //
  // That sentence could not be true. The CEO heads the root department, so its
  // subtree is the whole company and `departmentScopeDenial` short-circuits on
  // `kind === "executive"` before it walks anything. The only branch that could
  // fire was `!target` — the person was not in the manifest this gate read —
  // and it rendered as a permission the caller already held. The operator read
  // it as the tree model being broken, and hunted authority they were never
  // missing.
  if (!target) {
    throw new CallerRefusal(
      `no person '${personId}' exists in this company — this is not an authority refusal. ` +
        `If they were just created, the manifest this call read predates them; read the roster ` +
        `again and retry.`,
    );
  }
  if (!departmentIsInScope(gate.manifest, gate.person, target.departmentId)) {
    throw new CallerRefusal(
      `'${gate.person.id}' does not manage person '${personId}': authority is the subtree you ` +
        `head, and '${personId}' sits in '${target.departmentId}', which is not under it.`,
    );
  }
  return target;
}

/** #333: at most one console.error per process — enough that a stopped event
 * trail is discoverable, not so much that a persistent fault spams a pane's
 * terminal on every subsequent call (almost nothing checks this function's
 * return value). */
let appendOrganizationEventFailureLoggedOnce = false;

/** Test-only: this file's tests share one process/module instance, so a test
 * exercising the once-per-process fallback needs to reset it — the same
 * pattern `org-log.ts`'s `resetOrgLogSizeTracking` already establishes for an
 * identical need. */
export function resetAppendOrganizationEventFailureLoggedOnceForTests(): void {
  appendOrganizationEventFailureLoggedOnce = false;
}

/**
 * The three fields the two diagnostic sinks below actually read.
 *
 * Narrower than {@link OrganizationRuntimeContext} on purpose: every existing
 * caller still satisfies it structurally, and A4's key-refusal report — which
 * fires from inside the TRANSPORT, where only the endpoint is known — can reach
 * the same trail without a runtime context being threaded through the request
 * path to reach a diagnostic.
 */
interface OrganizationDiagnosticTarget {
  organizationDir: string;
  organization: string;
  personId: string;
}

/**
 * Pane diagnostics belong to Chief, not to the operator's project tree.
 * `organizationDir` is the project directory stamped into every pane; Chief's
 * private subtree is always one `.chief` join below it.
 */
function organizationBusDirectory(context: OrganizationDiagnosticTarget): string {
  return join(context.organizationDir, ".chief", "bus");
}

function organizationLogsDirectory(context: OrganizationDiagnosticTarget): string {
  return join(context.organizationDir, ".chief", "logs");
}

function appendOrganizationEvent(context: OrganizationDiagnosticTarget, event: Record<string, unknown>): boolean {
  try {
    // A removed organization no longer has a directory. The directory is
    // never an existence witness — only a committed manifest row is.
    if (!existsSync(context.organizationDir)) return false;
    const bus = organizationBusDirectory(context);
    mkdirSync(bus, { recursive: true, mode: 0o700 });
    appendBoundedJsonlLine(join(bus, "events.jsonl"), JSON.stringify(safeOrganizationEvent(event)), BUS_EVENTS_MAX_BYTES);
    return true;
  } catch (error) {
    if (!appendOrganizationEventFailureLoggedOnce) {
      appendOrganizationEventFailureLoggedOnce = true;
      console.error(`organization-intercom: appendOrganizationEvent failed for '${context.organization}' — the durable event trail (.chief/bus/events.jsonl) has stopped: ${error instanceof Error ? error.message : String(error)}`);
    }
    return false;
  }
}

function syncOrganizationEventDirectory(directory: string): void {
  const descriptor = openSync(directory, "r");
  try { fsyncSync(descriptor); }
  finally { closeSync(descriptor); }
}

/**
 * Publish one durable once-marker with SQL insert-or-ignore. This is the native
 * form of the O_EXCL+hardlink file marker it replaced: `ON CONFLICT DO NOTHING`
 * lets exactly one writer win, and the row is crash-durable the instant the
 * write service commits. The store key is byte-identical to the launcher-side
 * `journal-markers/<sha256(id)>` in `src/organization/org-event-journal.ts`, so
 * this extension producer and the launcher producer share ONE marker authority
 * — a terminal-health-incident resolution the health monitor reads back is seen
 * whether the extension or the launcher recorded it. The untrusted id is hashed
 * into the store key and never becomes a path component. `true` means THIS call
 * created the marker.
 */
async function publishOrganizationEventMarkerOnce(context: OrganizationRuntimeContext, id: string, value: Record<string, unknown>): Promise<boolean> {
  const digest = createHash("sha256").update(id).digest("hex");
  // Keyed off the company DIRECTORY, which is what the key IS
  // (`sha256(<dir>)[..12]`), so this producer and the launcher producer share
  // ONE marker row by construction. Its predecessor reached the same value by
  // rebuilding `documentKey(basename(root), dirname(root))` — a second
  // derivation that only agreed while `basename(organizationDir)` happened to
  // equal the company's slug.
  const key = companyKeyOf(context);
  // Insert-or-ignore through chiefd's normalized `event_once_markers` rows
  // (DocStore-direct: no live company, no org_events fence), via the shared
  // `RowStoresClient.insertEventOnceMarker` — its wire shape is exactly
  // `/v1/org/event-journal/insert-if-absent`. Byte-shares ONE marker row with
  // the launcher producer's /v1/org/event-journal route: same company key +
  // same sha256(id) digest + same event → the same row. Send the inner event
  // object (chiefd derives event_type + thr_* from it), matching the launcher
  // producer which sends `record.event`. `created` is true only when THIS
  // call won.
  const eventPayload = ((value as { event?: Record<string, unknown> }).event ?? value);
  const endpoint = chiefdEndpoint(context);
  const outcome = await new RowStoresClient(chiefdTransport(endpoint), endpoint.url).insertEventOnceMarker(key, {
    keyDigest: digest, id, event: eventPayload, createdAtMs: Date.now(),
  });
  return outcome.created;
}

export async function appendOrganizationEventOnce(context: OrganizationRuntimeContext, id: string, event: Record<string, unknown>): Promise<boolean> {
  try {
    if (!existsSync(context.organizationDir)) return false;
    const bounded = safeOrganizationEvent({ ...event, id });
    const created = await publishOrganizationEventMarkerOnce(context, id, {
      schemaVersion: 1,
      keyDigest: createHash("sha256").update(id).digest("hex"),
      event: bounded,
    });
    if (!created) return true;
    // The marker is the complete durable projection. A failed JSONL append is
    // safe to omit because retrying cannot distinguish a pre/post-append crash.
    appendOrganizationEvent(context, bounded);
    return true;
  } catch {
    return false;
  }
}

const MAX_EXCEPTION_MESSAGE_LENGTH = 600;

/** Keep diagnostics useful without persisting credentials, payloads, or whole command responses. */
function safeExceptionMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  const newline = message.indexOf("\n");
  const lineEnd = newline < 0 ? message.length : newline > 0 && message[newline - 1] === "\r" ? newline - 1 : newline;
  const firstLine = message.slice(0, Math.min(lineEnd, 8_192));
  let redacted = firstLine
    .replace(/https?:\/\/\S+/gi, "[redacted-url]")
    .replace(/\b\d{8,}:[A-Za-z0-9_-]{20,}\b/g, "[redacted-token]")
    .replace(/\bbearer\s+\S+/gi, "Bearer [redacted]")
    // Conventional command-line/header forms, such as `token=…` or
    // `Authorization: Bearer …`.
    .replace(/\b(bearer|token|api[_-]?key|authorization|password|secret|chat[_-]?id)\s*[:=]\s*(?:bearer\s+)?\S+/gi, "$1=[redacted]")
    // JSON-like diagnostic fragments must receive the same treatment.
    .replace(/(["'](?:bearer|token|api[_-]?key|authorization|password|secret|chat[_-]?id)["']\s*:\s*)(?:"[^"]*"|'[^']*'|[^,\s}\]]+)/gi, "$1[redacted]")
    .replace(/\b(stdout|command[_ -]?output|ledger|payload|request[_ -]?body|response[_ -]?body)\s*[:=].*$/i, "$1=[omitted]")
    .replace(/\s+/g, " ")
    .trim();
  if (/^[\[{]/.test(redacted)) redacted = "Structured diagnostic omitted";
  if (!redacted) redacted = "Diagnostic unavailable";
  if (redacted.length <= MAX_EXCEPTION_MESSAGE_LENGTH) return redacted;
  return `${redacted.slice(0, MAX_EXCEPTION_MESSAGE_LENGTH - 48)}… [${redacted.length - (MAX_EXCEPTION_MESSAGE_LENGTH - 48)} characters omitted]`;
}

/** Preserve ChiefD's structured refusal while bounding its persisted text. */
function sessionMaintenanceFailure(error: unknown): {
  message: string;
  retryable: boolean;
  terminalStatus?: "failed";
  refusalCode?: string;
  refusalDetail?: string;
} {
  const message = safeExceptionMessage(error);
  const retryable = isExpectedLifecycleProjectionError(error);
  const terminalStatus = retryable ? undefined : "failed";
  if (!(error instanceof OrgRowRefusalError)) return { message, retryable, terminalStatus };
  return {
    message,
    retryable,
    terminalStatus,
    refusalCode: error.code,
    refusalDetail: error.detail,
  };
}

function safeOrganizationEvent(event: Record<string, unknown>): Record<string, unknown> {
  const visit = (value: unknown, key?: string): unknown => {
    if (typeof value === "string" && key && /^(?:error|lastError|failureReason|reason)$/i.test(key)) {
      return safeExceptionMessage(value);
    }
    if (Array.isArray(value)) return value.map((item) => visit(item));
    if (value && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).map(([nestedKey, nested]) => [nestedKey, visit(nested, nestedKey)]));
    }
    return value;
  };
  return visit(event) as Record<string, unknown>;
}

function safeExceptionExtra(extra: Record<string, unknown>): Record<string, string | number | boolean | null | string[]> {
  const safe: Record<string, string | number | boolean | null | string[]> = {};
  const reserved = new Set(["schemaVersion", "at", "organization", "personId", "source", "error"]);
  // Fields the caller has already bounded and redacted at the source (a
  // launcher stderr tail, an argv list) must reach the log verbatim, as
  // structured data: routing them back through `safeExceptionMessage`'s
  // first-line-only + placeholder rules would re-introduce the exact
  // illegibility #331 exists to fix, and stringifying `args` would collapse
  // a greppable argv array into an opaque blob.
  for (const [key, value] of Object.entries(extra)) {
    if (reserved.has(key)) continue;
    if (key === "args" && Array.isArray(value)) { safe[key] = value.slice(0, 64).map((entry) => String(entry)); continue; }
    if (key === "stderrTail" && typeof value === "string") { safe[key] = value.slice(0, 4_000); continue; }
    if (typeof value === "string") safe[key] = safeExceptionMessage(value);
    else if (typeof value === "number" || typeof value === "boolean" || value === null) safe[key] = value;
    else safe[key] = `[${Array.isArray(value) ? "array" : typeof value} omitted]`;
  }
  return safe;
}

/** Mirrors the pane's in-flight turn, module-scoped.
 *
 * The session closure owns `turnInFlight`, but the mailbox drain runs outside
 * it and has to ask the same question, so every assignment to that flag sets
 * this one beside it. `PI_TURN_IN_FLIGHT_MIRRORED` pins that pairing. */
let piTurnInFlight = false;

/** Has this session's FIRST agent run begun?
 *
 * Module-scoped for the same reason `piTurnInFlight` is: the mailbox drain
 * runs outside the session closure and has to ask the question.
 *
 * # The window this exists to close
 *
 * Pi's interactive TUI calls `prompt()` BARE — no `streamingBehavior` — for the
 * initial message chief passes on every spawn, and `prompt()` throws
 * `Agent is already processing…` if anything flipped `isStreaming` between the
 * TUI's idle judgment and its own check. Before this gate the intercom was the
 * thing flipping it: a delivery arriving in the boot window took `triggerTurn`,
 * whose `_runAgentPrompt` sets the run-active flag on its first line, while the
 * boot prompt was still in flight. The operator saw a bare error line and their
 * pane's boot instruction was gone — `showError` renders and persists nothing.
 *
 * While this is false a delivery is INJECTED INTO the coming turn instead of
 * starting one, so the boot prompt and the delivery ride together rather than
 * racing. */
let firstRunStarted = false;

/** Opens the gate on a pane whose first run never comes.
 *
 * # This is the ORDINARY company-restart case, not a curiosity
 *
 * This comment used to say chief always passes an initial message, so
 * `agent_start` always arrives, and only a hand-run `pi` could need the
 * fallback. **Measured false on 2026-08-27**: a RESUME relaunch passes no
 * initial message — the pane comes back on old scrollback — so no first turn
 * ever comes, and the fallback is the only thing that opens the gate. A
 * whole-company restart is therefore the common path through here, not the
 * exotic one.
 *
 * That mattered because of what the fallback did NOT do. Everything delivered
 * inside the window is parked in Pi's `_pendingNextTurnMessages`, whose only
 * reader is the next prompt submission; the fallback opened the gate and
 * re-delivered nothing, and `mailboxDeliveryAttempts` is released only at
 * `agent_settled` — which needs the turn that never starts. A live company's
 * mail sat undelivered with the same envelope re-queued ninety seconds apart
 * and never consumed, because **every retry arrived through a door that was
 * closed at the moment of arrival**.
 *
 * Ten seconds is far outside the microtask-to-second window the race lives in
 * and far inside any human's patience. */
let firstRunFallbackTimer: ReturnType<typeof setTimeout> | undefined;

/** The boot window is over: deliveries may start turns again. */
function openFirstRunGate(): void {
  firstRunStarted = true;
  if (firstRunFallbackTimer) clearTimeout(firstRunFallbackTimer);
  firstRunFallbackTimer = undefined;
}

/** A new session starts closed, with the fallback armed.
 *
 * `onFallbackResolved` is the CONSEQUENCE of the gate opening without a first
 * turn: whatever was parked during the window has to be re-delivered, because
 * nothing else will. It runs only on the fallback path — when `agent_start`
 * opens the gate there IS a turn, the ordinary drain rides it, and re-driving
 * would be a second delivery of mail already on its way.
 *
 * It is not a new clock. The timer already existed; this adds what happens when
 * it fires. */
function closeFirstRunGate(
  openAfterMs = FIRST_RUN_FALLBACK_MS,
  onFallbackResolved?: () => void,
): void {
  firstRunStarted = false;
  if (firstRunFallbackTimer) clearTimeout(firstRunFallbackTimer);
  firstRunFallbackTimer = setTimeout(() => {
    firstRunFallbackTimer = undefined;
    // OPEN FIRST, THEN RE-DELIVER. The drain below asks `queuedPiDelivery`,
    // which reads this flag: with the gate still closed every envelope would
    // park exactly as it did on the way in, and the rescue would re-create the
    // livelock it exists to end.
    firstRunStarted = true;
    onFallbackResolved?.();
  }, openAfterMs);
  firstRunFallbackTimer.unref?.();
}

/** How long a session may wait for a first run before the gate opens anyway. */
const FIRST_RUN_FALLBACK_MS = 10_000;

/** @internal The delivery-option rule, exported so a test can drive the
 * CONDITION (busy or idle) rather than only the option names.
 *
 * `bootWindow` defaults to the live gate so every existing call site is
 * unchanged; a test passes it explicitly to drive the third state. */
export function queuedPiDeliveryForTest(
  mode: "steer" | "followUp",
  turnActive: boolean,
  bootWindow: boolean = !firstRunStarted,
) {
  return queuedPiDelivery(mode, turnActive, bootWindow);
}

/** What to do with one submitted input.
 *
 * Pure, and exported, so the rule can be driven without a Pi session — the
 * handler below is a thin wrapper that performs the effects this decides on.
 *
 * # The rule
 *
 * A submission that carries NO `streamingBehavior` is one whose submitter
 * believed the pane was idle. If it is not idle any more, Pi's `prompt()`
 * throws and the text is gone: the TUI clears its editor at submit and
 * `showError` renders the bare error without persisting a byte. So that one
 * case is re-queued as a `followUp` and answered `handled`.
 *
 * `followUp`, never `steer`: the submitter did not know a run was in progress,
 * so "after the current run" is the honest reading of what they asked for. A
 * steer would inject their line into a turn they did not know existed.
 *
 * Everything else continues untouched — including the re-submission itself,
 * which arrives carrying a behaviour and therefore takes the `continue` arm.
 * That is what makes the rescue unable to loop. */
export function inputInterceptionDecision(
  event: { text?: unknown; images?: unknown; streamingBehavior?: unknown; source?: unknown },
  idle: boolean,
): "requeue" | "continue" {
  if (idle) return "continue";
  if (event.streamingBehavior !== undefined) return "continue";
  return "requeue";
}

/** What one rescue records, and what it must never record.
 *
 * #645's rule stands: input text belongs to Pi's session writer alone, and a
 * line in `.chief/logs` is not that writer. So this carries the SHAPE of the
 * submission — who, from where, how long, how many images — and no content.
 *
 * It exists so the next "randomly and often" is a number somebody can grep
 * rather than a screenshot. Until now this defect produced no record anywhere:
 * Pi's `showError` renders and persists nothing, which is why its frequency
 * could not be counted even after the operator reported it. */
export function inputRequeueLogDetail(
  personId: string,
  event: { text?: unknown; images?: unknown; source?: unknown },
): { personId: string; source: string; length: number; images: number } {
  const text = typeof event.text === "string" ? event.text : "";
  return {
    personId,
    source: typeof event.source === "string" ? event.source : "unknown",
    length: text.length,
    images: Array.isArray(event.images) ? event.images.length : 0,
  };
}

/** @internal Drives the boot gate from a test without a live Pi session.
 *
 * `close` takes the fallback consequence too, so a test can pin the two
 * properties that make the rescue correct rather than merely present: that the
 * consequence runs when NO first turn came, and that it does not when one did.
 */
export function firstRunGateForTest(): {
  open: () => void;
  close: (afterMs?: number, onFallbackResolved?: () => void) => void;
  isOpen: () => boolean;
} {
  return { open: openFirstRunGate, close: closeFirstRunGate, isOpen: () => firstRunStarted };
}

/** Does a parked work-resume prompt need re-driving?
 *
 * **Not a test seam — the rescue itself calls this**, which is why it does not
 * carry the `ForTest` suffix every other export around it does. A name that
 * said otherwise would tell the next reader the production path calls a test
 * helper.
 *
 * The rule: `requestWorkResume` guards on `pending && !prompted`, and a prompt
 * PARKED in the boot window left exactly the opposite — `prompted` with nothing
 * pending — so a bare re-drive returns early and rescues nothing. Reaching the
 * fallback is the proof no turn consumed it, because `agent_start` clears that
 * timer.
 *
 * A prompt that was never issued (`!prompted`) needs no rescue: the ordinary
 * path still owns it. */
export function workResumeNeedsRedrive(prompted: boolean, pending: boolean): boolean {
  return prompted && !pending;
}

/** Supply both supported Pi queue option names while launcher-managed panes
 * may be running either side of Pi's extension API transition.
 *
 * `triggerTurn` ONLY when no turn is in flight, and that condition is the whole
 * point. Pi 0.80's `sendCustomMessage` routes on `isStreaming`, which is false
 * during an active run's tool execution — so a delivery arriving in that window
 * skipped the follow-up queue, fell through to `triggerTurn`, and called
 * `agent.prompt()`, which throws `Agent is already processing a prompt` while a
 * run is active. Pi catches that itself and emits it as an
 * `Extension "<runtime>" error`, so this extension's own try/catch never saw it
 * and the message was simply lost: a live CEO pane showed 44 of those errors
 * with nine messages still unread in its mailbox.
 *
 * Without `triggerTurn` a busy pane takes the follow-up/steer path when it is
 * streaming and an ordinary appended message when it is not — delivered, either
 * way, rather than thrown away. An idle pane still gets its turn started.
 *
 * BEFORE THE FIRST RUN there is a third answer, and it is neither of those:
 * `nextTurn`. Starting a turn in the boot window races the bare `prompt()` the
 * TUI is already making for chief's initial message, and losing that race
 * throws away the operator's boot instruction. `nextTurn` injects the message
 * into the turn that is COMING, so it is delivered by the very run it would
 * otherwise have broken. Interrupt urgency is gated too: inside the boot
 * window, riding the first turn IS delivering now.
 *
 * Once the gate opens the table below is byte-identical to what it has always
 * been, which `an_open_gate_restores_the_exact_busy_idle_table` pins. */
function queuedPiDelivery(
  mode: "steer" | "followUp",
  turnActive: boolean = piTurnInFlight,
  bootWindow: boolean = !firstRunStarted,
): QueuedPiDeliveryOptions {
  if (bootWindow) return { deliverAs: "nextTurn" };
  const options: QueuedPiDeliveryOptions = { deliverAs: mode, streamingBehavior: mode };
  if (!turnActive) options.triggerTurn = true;
  return options;
}

/** One shape with optional members rather than a union of three.
 *
 * A union would be the tighter type and it is the wrong one here: every reader
 * — including the tests that pin the busy/idle table field by field — asks
 * about `triggerTurn` and `streamingBehavior` directly, and a union makes each
 * of those a type error that has to be narrowed away before it can be checked.
 * The invariant that matters is enforced by the one function that builds it. */
export interface QueuedPiDeliveryOptions {
  deliverAs: "steer" | "followUp" | "nextTurn";
  streamingBehavior?: "steer" | "followUp";
  triggerTurn?: true;
}

/** A stale Pi can observe its old pane disappear while the supervisor is
 * replacing it. Automatic lifecycle housekeeping must leave that durable
 * request alone and let the next pass retry, not paint it as an agent
 * failure. */
function isExpectedLifecycleProjectionError(error: unknown): boolean {
  const message = safeExceptionMessage(error);
  return /(?:missing|no longer has|does not have|cannot find).{0,80}(?:launcher-owned|owned|runtime|pane)/i.test(message)
    || /runtime (?:server|session).{0,80}(?:missing|not found|exited)/i.test(message);
}

function isExpectedFreshSessionSelfReplacement(error: unknown): boolean {
  const message = safeExceptionMessage(error);
  return isExpectedLifecycleProjectionError(error)
    // #331 prefixes the launcher-command failure message with "<verb> failed
    // (exit <n>): ", so this can no longer be an exact-string match — match
    // the reason as a suffix instead.
    || /ended without an exit status$/i.test(message)
    || /(?:old|source).{0,20}Pi.{0,60}(?:ended|exited|terminated).{0,80}(?:replacement|respawn)/i.test(message);
}

function isNothingToCompact(error: unknown): boolean {
  return /nothing to compact(?:\s*\(session too small\))?/i.test(safeExceptionMessage(error));
}

/** Best-effort local diagnostic trail for extension/tool failures, never credentials. */
function logOrganizationException(
  context: OrganizationDiagnosticTarget,
  source: string,
  error: unknown,
  extra: Record<string, unknown> = {},
): void {
  try {
    const directory = organizationLogsDirectory(context);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    // #331's `LauncherCommandError` auto-enrichment stood here: a subprocess
    // failure carried argv/exit status/stderr tail, and this seam attached
    // them to whichever log line a call site already wrote. Both the error
    // class and the subprocess that threw it are deleted, so every extra key
    // in this record now comes from the caller that knows what it means.
    appendFileSync(join(directory, "exceptions.jsonl"), `${JSON.stringify({
      schemaVersion: 1,
      at: new Date().toISOString(),
      organization: context.organization,
      personId: context.personId,
      source,
      error: safeExceptionMessage(error),
      ...safeExceptionExtra(extra),
    })}\n`, { encoding: "utf8", mode: 0o600 });
  } catch {
    // Exception reporting must never destabilize the active Pi turn.
  }
}

const OPERATION_ID_ALPHABET = "0123456789abcdef";

/** A short id, not a security token — cheap enough to generate on every tool
 * call, unique enough that "grep the logs for ref a1b2c3" reliably lands on
 * one operation. */
function generateOperationId(): string {
  let id = "";
  for (let index = 0; index < 6; index += 1) {
    id += OPERATION_ID_ALPHABET[Math.floor(Math.random() * OPERATION_ID_ALPHABET.length)];
  }
  return id;
}

const INPUTS_DIGEST_MAX_FIELD_LENGTH = 200;
const INPUTS_DIGEST_MAX_FIELDS = 20;

/**
 * A bounded, flat, JSON-safe snapshot of a tool call's own params — only
 * primitive top-level fields (personId, departmentId, …), each
 * length-bounded; nested objects/arrays are dropped rather than stringified,
 * since they are usually a payload, not an identifier worth persisting.
 * #333's finding: a failed tool log line had no identifying field to
 * correlate against — this is the generic fix, not a per-tool one.
 */
function boundedInputsDigest(params: unknown): Record<string, string | number | boolean> {
  const digest: Record<string, string | number | boolean> = {};
  if (!params || typeof params !== "object" || Array.isArray(params)) return digest;
  let fields = 0;
  for (const [key, value] of Object.entries(params as Record<string, unknown>)) {
    if (fields >= INPUTS_DIGEST_MAX_FIELDS) break;
    if (typeof value === "string") {
      digest[key] = value.length > INPUTS_DIGEST_MAX_FIELD_LENGTH ? `${value.slice(0, INPUTS_DIGEST_MAX_FIELD_LENGTH)}…` : value;
    } else if (typeof value === "number" || typeof value === "boolean") {
      digest[key] = value;
    } else {
      continue;
    }
    fields += 1;
  }
  return digest;
}

/**
 * The one structured failure-log contract (#333): `source` names the
 * operation (same `source` shape every `logOrganizationException` call
 * already uses, e.g. `tool:<name>`, so existing log consumers/tests are
 * unaffected), `actor`/`target` identify who/what, `inputsDigest` is the
 * bounded call params, `cause` is the underlying error/reason, `retryable`
 * classifies it, and `opId` is the same short id echoed on the user-facing
 * card — so a cryptic card is always one grep away from this exact line.
 * Callers that already invoke `logOrganizationException` directly keep
 * working; this is the contract new call sites (and the generic tool
 * wrapper) should adopt.
 */
function logOperationFailure(context: OrganizationRuntimeContext, source: string, input: {
  actor?: string;
  target?: string;
  inputsDigest?: Record<string, string | number | boolean>;
  cause: unknown;
  retryable?: boolean;
  opId: string;
}): void {
  logOrganizationException(context, source, input.cause, {
    opId: input.opId,
    ...(input.actor ? { actor: input.actor } : {}),
    ...(input.target ? { target: input.target } : {}),
    ...(input.retryable !== undefined ? { retryable: input.retryable } : {}),
    ...input.inputsDigest,
  });
}

/**
 * One structured line into `<orgDir>/.chief/logs/<service>.jsonl`, the same schema
 * `src/organization/org-log.ts` emits.
 *
 * Written inline rather than imported because extensions are a deliberate
 * boundary — nothing under `extensions/` imports from `src/`, since a copied
 * extension must still load. `tests/org-log.test.ts` asserts this record's
 * shape against the exported key set, so the duplication cannot drift silently.
 */
function appendOrganizationLogLine(
  context: OrganizationRuntimeContext,
  service: string,
  event: string,
  level: "debug" | "info" | "warn" | "error",
  detail: Record<string, unknown>,
): void {
  try {
    const directory = organizationLogsDirectory(context);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    appendFileSync(join(directory, `${service}.jsonl`), `${JSON.stringify({
      schemaVersion: 1,
      at: new Date().toISOString(),
      level,
      service,
      event,
      organization: context.organization,
      pid: process.pid,
      ...(context.personId ? { personId: context.personId } : {}),
      detail: safeExceptionExtra(detail),
    })}\n`, { encoding: "utf8", mode: 0o600 });
  } catch {
    // Observability must never destabilize the active Pi turn.
  }
}

/**
 * THE USERNAME for a person, which is what every surface an agent reads must
 * show them.
 *
 * Operator ruling: *"Every time, use the USERNAME. That's how we
 * communicate."* A person's `id` is a kebab slug — `portfolio-management-head`
 * — and it is the durable key for mailboxes, document-store paths and
 * transcripts. It is not a name, and showing it to an agent is how an agent
 * comes to address people by it.
 *
 * Normalization matches the pane surfaces exactly: the lowercased first word
 * of the roster name, keeping only alphanumerics, `_` and `-`.
 *
 * An id this roster does not know is returned unchanged. A cross-organization
 * sender is a real case and its handle is not ours to invent.
 */
function personHandle(manifest: IntercomOrganizationManifest, personId: string): string {
  const name = manifest.people[personId]?.name;
  if (!name) return personId;
  const first = name.trim().split(/\s+/)[0] ?? "";
  const slug = first.toLowerCase().replace(/[^a-z0-9_-]/g, "");
  return slug || personId;
}

/**
 * Resolve `to` to person ids, accepting a USERNAME or an id.
 *
 * The id path stays first and exact, so nothing that addresses by key changes
 * behaviour or gets slower. The handle path exists because every surface an
 * agent reads now shows handles, and an agent that is shown a handle will send
 * to a handle — a resolver that accepted only ids would turn the naming fix
 * into a new class of failed delivery.
 */
function recipientsFor(manifest: IntercomOrganizationManifest, sender: string, to: string): string[] {
  const recipient = to.trim().replace(/^@/, "");
  if (!recipient) throw new CallerRefusal("Recipient is required");
  if (recipient === "all") {
    const recipients = manifest.peopleOrder.filter((id) => id !== sender && manifest.people[id]?.employmentState !== "departed");
    if (!recipients.length) throw new CallerRefusal("Broadcast has no employed peer recipients");
    return recipients;
  }
  const employed = manifest.peopleOrder.filter((id) => manifest.people[id]?.employmentState !== "departed");
  let resolved = recipient;
  if (!manifest.people[recipient] || manifest.people[recipient]?.employmentState === "departed") {
    // Not an id, so try it as a USERNAME across employed people.
    const byHandle = employed.filter((id) => personHandle(manifest, id) === recipient.toLowerCase());
    if (byHandle.length > 1) {
      // AMBIGUOUS: two people share a first name. Guessing would deliver
      // somebody's message to the wrong person silently, which is strictly
      // worse than refusing, so name both and let the sender choose.
      const both = byHandle.map((id) => `@${personHandle(manifest, id)} (${id})`).join(" and ");
      throw new CallerRefusal(
        `'${recipient}' is ambiguous — it matches ${both}. Address the one you mean by its id.`,
      );
    }
    if (byHandle.length === 1) resolved = byHandle[0] as string;
  }
  const target = manifest.people[resolved];
  if (!target || target.employmentState === "departed") {
    // The error text is guidance an agent copies from, so it lists USERNAMES
    // and carries the id in parentheses for anyone addressing by key.
    const available = employed.map((id) => `@${personHandle(manifest, id)} (${id})`);
    throw new CallerRefusal(`Unknown employed recipient '${recipient}'; choose one of ${available.join(", ")} or all`);
  }
  if (resolved === sender) throw new CallerRefusal("Send messages to a peer, not yourself");
  return [resolved];
}

/** Compare caller-owned immutable content for idempotent replay. `createdAt` is
 * generated per attempt, so the first durable envelope owns it. */
function messageReplayContentMatches(left: OrganizationEnvelope, right: OrganizationEnvelope): boolean {
  const comparable = (envelope: OrganizationEnvelope) => ({
    schemaVersion: envelope.schemaVersion,
    id: envelope.id,
    organization: envelope.organization,
    fromPersonId: envelope.fromPersonId,
    to: envelope.to,
    recipients: envelope.recipients,
    body: envelope.body,
    urgency: envelope.urgency,
    replyTo: envelope.replyTo,
    healthIncident: envelope.healthIncident,
  });
  return JSON.stringify(comparable(left)) === JSON.stringify(comparable(right));
}

/** Pi acceptance must match the exact persisted envelope, timestamp included. */
function messageContentMatches(left: OrganizationEnvelope, right: OrganizationEnvelope): boolean {
  return messageReplayContentMatches(left, right) && left.createdAt === right.createdAt;
}


/** Find a retry of one caller-supplied message id even after Pi archived it. */
async function existingMailboxMessage(
  context: OrganizationRuntimeContext,
  recipient: string,
  messageId: string,
): Promise<OrganizationEnvelope | undefined> {
  const doc = await readMailboxDoc(context, recipient);
  if (!doc) return undefined;
  return findMailboxEntryByMessageId(context, doc, messageId, ["pending", "accepted"])?.envelope;
}

/**
 * What a send did, not merely what it wrote.
 *
 * `replayedFrom` carries the earlier delivery's `createdAt` when this attempt
 * was recognized as the replay of one. The caller MUST surface it: a
 * suppressed send that reports plain success is the failure mode this whole
 * change is written to avoid — a duplicate message is annoying, a message the
 * sender believes was delivered and was not is invisible.
 */
type OrganizationSendOutcome = {
  envelope: OrganizationEnvelope;
  replayedFrom?: string;
};

export async function sendOrganizationMessage(
  context: OrganizationRuntimeContext,
  input: {
    to: string;
    body?: string;
    urgency?: MessageUrgency;
    replyTo?: string;
  },
  options: { now?: string; id?: string } = {},
): Promise<OrganizationSendOutcome> {
  const manifest = await loadIntercomOrganization(context);
  currentPerson(context, manifest);
  const body = typeof input.body === "string" ? input.body.trim() : "";
  if (!body) throw new CallerRefusal(ORGANIZATION_SEND_BODY_REQUIRED_GUIDANCE);
  const recipients = recipientsFor(manifest, context.personId, input.to);
  // Replay identity, when the caller does not own one. Scan this fingerprint's
  // candidate ids oldest-first: the newest one that already exists is either
  // the interrupted attempt (inside the window — this call is its replay) or a
  // settled earlier send (outside it — this call is a deliberate repeat and
  // takes the next index).
  let replayedFromPrior: string | undefined;
  let resolvedId = options.id;
  if (!resolvedId) {
    const nowIso = options.now ?? new Date().toISOString();
    const resolved = await resolveSendReplay({
      fingerprint: messageContentFingerprint(context, { ...input, body }, recipients),
      nowMs: Date.parse(nowIso),
      // A broadcast is one send. A prior copy in ANY recipient's mailbox marks
      // the index occupied; the per-recipient top-up below then finishes an
      // interrupted broadcast without re-delivering the recipients it reached.
      lookup: async (candidate) => (await Promise.all(recipients.map(
        (recipient) => existingMailboxMessage(context, recipient, candidate),
      ))).find((entry) => entry !== undefined),
    });
    resolvedId = resolved.id;
    replayedFromPrior = resolved.replayedFrom;
  }
  const envelope: OrganizationEnvelope = {
    schemaVersion: SCHEMA_VERSION,
    id: resolvedId,
    organization: manifest.slug,
    fromPersonId: context.personId,
    to: input.to.trim().replace(/^@/, ""),
    recipients,
    body,
    urgency: input.urgency ?? "normal",
    replyTo: input.replyTo,
    createdAt: options.now ?? new Date().toISOString(),
  };
  const existing = new Map(await Promise.all(recipients.map(
    async (recipient) => [recipient, await existingMailboxMessage(context, recipient, envelope.id)] as const,
  )));
  const canonical = existing.values().next().value as OrganizationEnvelope | undefined ?? envelope;
  if (!messageReplayContentMatches(canonical, envelope)) {
    throw new CallerRefusal(`Message id '${envelope.id}' already has conflicting content`);
  }
  for (const [recipient, prior] of existing) {
    if (prior && !messageReplayContentMatches(prior, canonical)) {
      throw new CallerRefusal(`Message id '${envelope.id}' has conflicting content for '${recipient}'`);
    }
  }
  let queued = false;
  for (const recipient of recipients) {
    if (existing.get(recipient)) continue;
    await publishMailboxEnvelope(context, recipient, canonical);
    queued = true;
  }
  appendOrganizationEvent(context, {
    event: queued ? "message-queued" : "message-queue-replayed",
    ...canonical,
    at: envelope.createdAt,
  });
  // An interrupted broadcast that reached some recipients still has real work
  // to do for the rest, and having done it is not a suppressed send. Only a
  // call that delivered to nobody is reported back as a replay.
  return { envelope: canonical, replayedFrom: queued ? undefined : replayedFromPrior };
}

/** The id prefixes this extension mints for mail it sends ABOUT a failure,
 * rather than mail a person wrote. */
// `content-filter-bounce-` IS A HISTORICAL NAME AND IT IS LOAD-BEARING. The
// bounce was scoped to content refusals for one commit and now fires on every
// failed kind, so the prefix under-describes what it marks — but it is what
// this list matches on, and this list is the only thing standing between two
// failing people and an endless exchange of bounces. Renaming it is fine;
// renaming it WITHOUT changing this array in the same commit silently deletes
// the ping-pong guard while appearing to tidy a string.
const SYSTEM_FAILURE_MESSAGE_ID_PREFIXES = [
  "content-filter-bounce-",
  "content-filter-",
  "provider-health-",
] as const;

/**
 * Is this envelope one the failure machinery itself produced?
 *
 * THE BOUNCE MUST NEVER BOUNCE. Two people whose turns are failing can mail
 * each other for ever otherwise, and the loop is not hypothetical: a bounce is
 * an ordinary delivery to its recipient, so it starts a turn, and a person
 * whose session context is what the provider refuses fails EVERY turn —
 * including the one reading a bounce. Each round mints a NEW envelope id, so
 * the idempotent-send dedup never collapses the chain. A live box has
 * three content-filtered people today, so a filtered pair is a live shape and
 * not a thought experiment.
 *
 * Keyed on the id prefix rather than a flag on the envelope because the ids are
 * OURS and deterministic — every one of them is minted by the call sites in
 * this file — and because adding a durable envelope field to carry it would
 * change what every reader of a mailbox row has to understand.
 */
function isSystemFailureMessageId(id: string): boolean {
  return SYSTEM_FAILURE_MESSAGE_ID_PREFIXES.some((prefix) => id.startsWith(prefix));
}

/**
 * The durable identity of a send, derived from WHAT IS BEING SENT.
 *
 * Deliberately not a function of the Pi tool-call id, the process, or the
 * clock. A resumed agent re-issues the call from a NEW assistant message, so
 * it carries a NEW `tool_use` id (the provider mints it per block); the
 * previous key hashed exactly that id and therefore could not fire on the one
 * failure it existed for.
 *
 * `createdAt` is excluded for the same reason, and its exclusion is the whole
 * mechanism: two attempts at one send must collide.
 */
function messageContentFingerprint(
  context: OrganizationRuntimeContext,
  input: { to: string; body: string; urgency?: MessageUrgency; replyTo?: string },
  recipients: readonly string[],
): string {
  return createHash("sha256")
    .update(JSON.stringify({
      organization: context.organization,
      fromPersonId: context.personId,
      to: input.to.trim().replace(/^@/, ""),
      recipients,
      body: input.body,
      urgency: input.urgency ?? "normal",
      replyTo: input.replyTo ?? null,
    }))
    .digest("hex")
    .slice(0, 24);
}

function departmentDepth(manifest: IntercomOrganizationManifest, departmentId: string): number {
  let depth = 0;
  let cursor = manifest.departments[departmentId];
  const seen = new Set<string>();
  while (cursor?.parentDepartmentId) {
    if (seen.has(cursor.id)) break;
    seen.add(cursor.id);
    depth += 1;
    cursor = manifest.departments[cursor.parentDepartmentId];
  }
  return depth;
}

function organizationUnitKind(manifest: IntercomOrganizationManifest, unit: DepartmentRecord): OrganizationUnitKind {
  const kind = unit.kind ?? (unit.id === manifest.rootDepartmentId ? "company" : "department");
  if (kind !== "company" && kind !== "department" && kind !== "contract") {
    throw new Error(`Organization unit '${unit.id}' has unknown kind '${String(kind)}'`);
  }
  return kind;
}

type RosterRuntimeStatus = "absent" | "starting" | "recovering" | "stopped" | "idle" | "running";
// `"suppressed"` was a member until chief-home-is-cwd §4c: it meant "down
// during a CEO-only boot, durable work retained", and only the CEO boot lease
// could produce it. The daemon boots no pane, so nothing can observe it.
type RosterPersonRuntimeState = "absent" | "starting" | "recovering" | "stopped" | "parked" | "departed" | "running" | "handoff-held";

interface RosterActivityPerson {
  personId: string;
  /**
   * `lastPaneDepartmentId` is DELIBERATELY ABSENT. chiefd deleted the persisted
   * head-in-parent column (#751/P9); the window is derived per read through
   * `personDepartmentId(manifest, personId)`, which tracks the CURRENT tree.
   */
  lastDesiredActive: boolean;
  activeTransitionId?: string;
}

interface RosterActivityTransition {
  id: string;
  personId: string;
  action: "park" | "transfer" | "offboard";
  status: "awaiting_handoff" | "overdue" | "ready" | "applied" | "cancelled" | "forced";
}

interface RosterActivityLedger {
  schemaVersion: number;
  organization: string;
  personOrder: string[];
  people: Record<string, RosterActivityPerson>;
  transitionOrder: string[];
  transitions: Record<string, RosterActivityTransition>;
}

interface RosterRuntimeProjection {
  version: number;
  observedAt: string | null;
  status?: "starting" | "recovering" | "running" | "idle" | "stopped";
  /**
   * Person -> the process handle the ACTUATOR reported, as chiefd publishes it
   * today. #751 moved tmux out of the backend, so chiefd holds no display id
   * and will never hold one again (`converge_apply/cycle.rs`,
   * `actuate/report.rs`: "People and processes, never panes").
   *
   * The value is the pid as a decimal string, and the EMPTY STRING when the
   * actuator proved the process alive but could read no pid. So the KEY SET is
   * the load-bearing half — a key means "alive" — and the value is a
   * diagnostic. Reading liveness off the VALUE reads an alive person as parked
   * every time the pid is unknown.
   *
   * This key was called `panes` until the rename that carries this comment, and
   * the name was the whole defect: a reader that believed it validated the map
   * against a `%\d+` tmux id and refused every real payload, taking `org_roster`
   * down for every person in every company. A field whose name states the
   * opposite of its contents is an invitation to the next reader to repeat that.
   *
   * `windows` is DELETED, not renamed. It was department -> tmux window id;
   * both live publishers hardcode it to `{}` and no backend reader consults
   * it. An always-empty map is a dead mechanism, and this reader demanding an
   * entry in it is half of why `org_roster` failed for every person.
   */
  processHandles: Record<string, string>;
  reconciliation?: {
    phase: "in_progress";
    startedAt: string;
  };
}

export interface OrganizationRosterPersonObservation {
  personId: string;
  state: RosterPersonRuntimeState;
  /** The pid chiefd published for this person's process, when it knew one.
   * Absent both when the person has no process AND when the actuator proved
   * one alive without reading a pid — `state` is the liveness authority, never
   * this field. */
  processId?: string;
  /** The department the runtime places this person under, DERIVED from the
   * current manifest tree. chiefd publishes no placement. */
  departmentId?: string;
  transitionId?: string;
  transitionAction?: RosterActivityTransition["action"];
  transitionStatus?: RosterActivityTransition["status"];
}

export interface OrganizationRosterObservation {
  organization: string;
  status: RosterRuntimeStatus;
  observedAt?: string;
  reconciliation?: {
    phase: "in_progress";
    startedAt: string;
  };
  people: Record<string, OrganizationRosterPersonObservation>;
  /** Exact process/desired-active mismatches that an ordinary
   * authorization-aware reconcile can repair. Structural projection errors
   * never enter this set. */
  runtimeActivityDivergence?: {
    missingProcessPersonIds: string[];
    unexpectedProcessPersonIds: string[];
  };
}

function jsonAuthority(path: string, label: string): unknown {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`Cannot read ${label} authority '${path}': ${error instanceof Error ? error.message : String(error)}`);
  }
}

function exactStringOrder(value: unknown, records: Record<string, unknown>, label: string): string[] {
  if (!Array.isArray(value) || value.some((entry) => typeof entry !== "string")) throw new Error(`${label} order is invalid`);
  const order = value as string[];
  if (new Set(order).size !== order.length || order.some((id) => !records[id]) || order.length !== Object.keys(records).length) {
    throw new Error(`${label} order is stale or corrupt`);
  }
  return order;
}

/**
 * Validate the runtime row's person -> process-handle map.
 *
 * A non-string value is the only rejection, and that is deliberate. What stood
 * here demanded a non-empty, `%\d+`-shaped, map-unique tmux pane id. chiefd
 * stopped publishing tmux ids when #751 moved tmux out of the backend, so the
 * real payload — `{"ceo":"","head-of-engineering":""}` — was rejected FOUR
 * ways at once (empty, not `%\d+`, and two `""` values counted as duplicates),
 * and `org_roster` failed for every person in every company with "runtime
 * panes must contain non-empty string ids". The empty string is chiefd's
 * honest "alive, pid unknown", not corruption, and two people whose pids are
 * both unknown are not a duplicate id. The map has since been renamed to
 * `processHandles`, so the name no longer suggests the check that failed.
 */
function processHandles(value: unknown, label: string): Record<string, string> {
  const raw = object(value, label);
  if (Object.values(raw).some((entry) => typeof entry !== "string")) throw new Error(`${label} must map each person to a string process handle`);
  return raw as Record<string, string>;
}

/* TOMBSTONE (chief-home-is-cwd §4c): `loadRosterCeoBootLease()` stood here. It
 * read the CEO boot lease so the roster could interpret a one-process
 * observation as an intentional CEO-only projection rather than a broken one.
 * The lease is deleted with the boot that took it. */

/**
 * Read one durable document out of the SQL write service.
 *
 * Deliberately self-contained: this file is COPIED into each role's Pi home, so
 * it must not import from `../src/` (enforced by org-materialize's test). The
 * key derivation stays byte-compatible with `src/organization/org-durable-store.ts`
 * via the served company key (never a second hash implementation).
 */

/**
 * Thrown when the runtime context this install was built from carries no
 * chiefd base URL — that is, it was PARSED
 * ({@link readOrganizationRuntimeContext}) and never RESOLVED
 * ({@link resolveOrganizationRuntimeContext}).
 *
 * There is no fixed-port fallback and no ambient re-read (ruling D0/D1):
 * guessing an address, or picking up whatever address the process happens to
 * hold, risks talking to another company's daemon — which answers.
 */
export class OrgChiefdUrlUnsetError extends Error {
  readonly name = "OrgChiefdUrlUnsetError" as const;
  constructor() {
    super(
      "this install's runtime context carries no chiefd base URL; it must be resolved from beacond for its own company before any docstore call can be made",
    );
  }
}

/**
 * ONE company's daemon, and the identity whose key signs for it.
 *
 * This is what every chiefd call in this file travels on, and it is
 * per-install rather than per-process on purpose. The address used to be read
 * from `process.env.ORG_CHIEFD_URL`, which is correct for exactly one
 * deployment — one Pi process per tmux pane, one company per process — and
 * unusable for a host that runs several companies in one process. There is no
 * single correct value there, and setting the variable around a call is a race
 * whose failure mode is silent: another company's daemon ANSWERS. It does not
 * error. The address now comes from beacond, keyed by THIS install's own
 * company slug (see {@link resolveOrganizationRuntimeContext}).
 *
 * The credential travels with the address for the same reason and one more: a
 * host runs several PEOPLE, and two people of the same company share a URL
 * while having different keys. Resolving the signer from anything narrower
 * than this pair would present one person's bearer for another's call.
 */
export interface ChiefdEndpoint {
  /** The company daemon's own bound base URL. */
  readonly url: string;
  /** The acting person — whose pi-home identity key signs this call. */
  readonly personId: string;
  /** The COMPANY DIRECTORY; where that person's own agent folder is found. */
  readonly organizationDir: string;
  /** Exact directory holding this person's company identity key. */
  readonly identityDir: string;
}

/**
 * The daemon this context's company answers on, with this context's person.
 *
 * Threaded from the install's own resolved context, never re-read from the
 * ambient process. No fixed-port fallback (ruling D0/D1) — a context with no
 * URL throws {@link OrgChiefdUrlUnsetError} rather than guessing an address
 * that may belong to another company's daemon.
 */
function chiefdEndpoint(context: OrganizationRuntimeContext): ChiefdEndpoint {
  const url = context.chiefdUrl?.trim();
  if (!url) throw new OrgChiefdUrlUnsetError();
  return { url, personId: context.personId, organizationDir: context.organizationDir, identityDir: context.identityDir };
}

/**
 * Which (key file, reason, mode) triples this process has already reported.
 *
 * The refusal is discovered ONCE PER REQUEST — the shared pane acquirer
 * re-reads the key on every call — so an unbounded report would write one line
 * per org tool call for the lifetime of a broken pane. One report per distinct
 * problem IS the signal; the second one carries no new information.
 */
const reportedKeyRefusals = new Set<string>();

/**
 * The pane-side half of the 0600 rule (A4), and the reporting path
 * `readAgentKeypair` cannot have.
 *
 * That reader refuses a group- or world-readable key and had NO WAY TO SAY SO:
 * it lives in the extension-runtime closure, which is copied FLAT into every
 * pi-home, imports nothing but `node:*`, and cannot use `console` (banned by
 * lint precisely so a shared logger is used instead). So the refusal was
 * silent — and since A1 made a bad key mode a hard refusal on the daemon side
 * too, the pane simply stopped working with nothing anywhere saying why. A
 * strict refusal whose only signal is silence is a support incident waiting to
 * happen.
 *
 * The channels are the two every other in-pane failure already uses together
 * (see the session-maintenance poll below): the durable
 * `.chief/bus/events.jsonl` trail and the `.chief/logs/exceptions.jsonl`
 * diagnostic. The message names the FILE
 * and the exact `chmod 600` that fixes it, matching what the daemon says about
 * its own operator key (`identity_keys::load_private_key_pem`) — a report that
 * says "bad mode" and stops is a second puzzle, not a fix.
 *
 * `absent` IS BENIGN ONLY WHEN NOTHING ABOUT THE PANE CLAIMS A PERSON, and
 * that condition is the whole rule.
 *
 * A pane with no person id legitimately has no identity key — reporting there
 * would be noise on every ordinary pane, so the early return below keeps its
 * subject. A pane that IS running as a person cannot legitimately lack that
 * person's key: it was launched from that person's home and every org route it
 * will call is fenced, so absent means the pane is broken before it does
 * anything.
 *
 * The rule used to be unconditional — "a documented, benign state" — and that
 * hid a real bug for the length of this branch. A WRONG PATH produces
 * `absent`, indistinguishable here from a key not yet minted.
 * `paneHomeDirectory` composed `<dir>/people/<id>/pi-home` while
 * materialization writes under `<dir>/.chief`, so EVERY person-pane looked one
 * segment too shallow, found no key, and `paneTokenManager` returned
 * `undefined` — benign by that rule, so nothing threw and nothing logged.
 * Every `/v1/org/*` call then answered `missing bearer token`, a whole segment
 * away from the cause.
 *
 * A person-pane says so where the absence is DISCOVERED rather than at the
 * first 401 downstream. The dedup below is what keeps that from becoming
 * noise: one report per (path, reason, mode), not one per request.
 *
 * The company is the DIRECTORY the pane is standing in, so this needs no
 * ambient context: it is called from the transport, which knows only the
 * endpoint it was handed. The display name it stamps on the log event is that
 * directory's own basename — a label for a human reading the event, never a
 * key anything resolves by.
 */
export function reportPaneKeyRefusal(refusal: AgentKeyRefusal, identity: PaneIdentity): void {
  // THE PREDICATE, and it lives here rather than in the caller because this is
  // exported and callable directly. `paneTokenManager` already refuses a
  // person-less identity before it ever reads a key, so in production every
  // call past this line claims a person — but a rule that depends on its one
  // caller keeping a guard is a rule that is one refactor from silent.
  if (refusal.reason === "absent" && !identity.personId.trim()) return;
  const mode = refusal.mode === undefined ? "unknown" : refusal.mode.toString(8).padStart(4, "0");
  const seen = [refusal.keyPath, refusal.reason, mode].join("|");
  if (reportedKeyRefusals.has(seen)) return;
  reportedKeyRefusals.add(seen);
  const target = {
    organizationDir: identity.organizationDir,
    organization: basename(identity.organizationDir),
    personId: identity.personId,
  };
  // Each reason names the FILE and what to do about it. A report that says
  // "no key" and stops is a second puzzle, not a fix — and for `absent` the
  // path IS the diagnosis, which is what would have made the wrong-segment
  // defect self-explaining instead of surfacing as `missing bearer token`.
  const detail = refusal.reason === "permissive-mode"
    ? `identity key ${refusal.keyPath} is mode ${mode}; a group- or world-readable key is refused. Run: chmod 600 ${refusal.keyPath}`
    : refusal.reason === "absent"
    ? `no identity key at ${refusal.keyPath}, so this pane cannot prove who it is and every /v1/org/* call will be refused. Either this person was never enrolled, or the pane is looking in the wrong place.`
    : `identity key ${refusal.keyPath} could not be read, so this pane cannot prove who it is.`;
  appendOrganizationEvent(target, {
    event: "identity-key-refused",
    personId: identity.personId,
    reason: refusal.reason,
    keyPath: refusal.keyPath,
    mode,
    detail,
    at: new Date().toISOString(),
  });
  logOrganizationException(target, "identity-key-refused", new Error(detail), {
    reason: refusal.reason,
    mode,
  });
}

/**
 * The pane's credential for one endpoint, through chiefing's ONE acquirer.
 *
 * The implementation used to live here, which meant exactly one extension in a
 * pane could authenticate: `team-ui` and every SSE reader run in the SAME pane
 * over the SAME key and reached chiefd with nothing — not because they had no
 * key, but because the acquirer was not reachable from where they were. It now
 * lives in `@chief/chiefing`'s `PaneIdentity`, shared rather than copied: a
 * second acquirer would be a second token cache, and the two would disagree the
 * moment either re-acquired.
 */
function endpointTokenManager(endpoint: ChiefdEndpoint): AgentTokenManager | undefined {
  return paneTokenManager(endpoint, reportPaneKeyRefusal);
}

/**
 * The credential this install's SSE reader presents, or `undefined` when there
 * is no address to mint one against.
 *
 * Takes the resolved URL rather than calling {@link chiefdEndpoint}, because
 * the test seam (`options.sseUrl`) supplies an address on contexts that carry
 * none, and a bearer lookup must never be the thing that throws where the URL
 * itself did not.
 */
export function organizationSseBearer(
  context: OrganizationRuntimeContext,
  sseUrl: string | undefined,
): AgentTokenManager | undefined {
  const url = sseUrl?.trim() ?? context.chiefdUrl?.trim();
  if (!url) return undefined;
  return endpointTokenManager({
    url,
    personId: context.personId,
    organizationDir: context.organizationDir,
    identityDir: context.identityDir,
  });
}

/**
 * The transport every org tool call travels on: the shared pane transport, with
 * the token cache, the bearer header and the re-acquire-once-on-401 retry it
 * has always had — and now with a refused key reported instead of swallowed.
 */
function chiefdTransport(endpoint: ChiefdEndpoint): FetchTransport {
  return paneChiefdTransport(endpoint, reportPaneKeyRefusal);
}

/**
 * The async, in-process twin of the deleted `spawnSync("curl")` transport:
 * one POST through the shared chiefing extension-runtime transport
 * (`postOrgRoute` + `FetchTransport`), awaited on the pane's own JS thread.
 * Every route's path/body stays byte-identical to before this conversion —
 * only the transport underneath changed. Kept under this name so the ~24
 * existing call sites below only need `await` + an `async` enclosing
 * function, not a route-by-route rewrite.
 */
async function chiefdPostJson<T>(endpoint: ChiefdEndpoint, path: string, body: unknown): Promise<T> {
  return postOrgRoute<T>(chiefdTransport(endpoint), endpoint.url, path, body);
}

/** The two possible outcomes of Chiefd's revisionless person-transfer operation. */
type AtomicPersonTransferResult =
  | { applied: true; moved: string[] }
  | { refused: string; detail: string };

type AtomicPersonTransferRequest = {
  slug: string;
  personId: string;
  destinationId: string;
  intent: string;
  actor: string;
  /** Required when the person being moved heads a department. Moving them out
   *  leaves it without a head, and the model has exactly two answers. */
  vacates?: ChiefdHeadVacancy;
};

type AtomicPersonTransferCaller = (endpoint: ChiefdEndpoint, request: AtomicPersonTransferRequest) => Promise<AtomicPersonTransferResult>;

/**
 * Issue the named atomic person-transfer operation without routing through the
 * legacy lifecycle CLI.  `intent` is intentionally an opaque mechanics marker
 * (used only to identify a superseded transition); the staffing-history line is
 * AUTHORED by chiefd from the act and the authenticated caller, so no caller
 * writes audit prose and a transition id can never be presented as one.
 *
 * A 422 refusal is decoded by the shared `postOrgRoute` into a thrown
 * `OrgRowRefusalError` — caught here and mapped into this route's own typed
 * `{refused, detail}` VALUE outcome (never a second parser; `.code`/`.detail`
 * are chiefing's own already-decoded fields).
 */
async function chiefdAtomicPersonTransfer(
  endpoint: ChiefdEndpoint,
  body: AtomicPersonTransferRequest,
): Promise<AtomicPersonTransferResult> {
  try {
    const result = await postOrgRoute<{ applied?: unknown; moved?: unknown }>(
      chiefdTransport(endpoint), endpoint.url, "/v1/org/person/transfer", body,
    );
    if (result.applied === true && Array.isArray(result.moved) && result.moved.every((personId) => typeof personId === "string")) {
      return { applied: true, moved: result.moved as string[] };
    }
    throw new Error("chiefd docstore /v1/org/person/transfer returned an invalid outcome");
  } catch (error) {
    if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };
    throw error;
  }
}

/**
 * The two possible outcomes of chiefd's atomic create-department operation.
 *
 * `warnings` is not decoration. chiefd's `materialize_after_commit` runs the
 * pi-home materialization AFTER the rows commit and deliberately downgrades its
 * own failures to this array rather than to an error, precisely so a caller
 * whose department already exists is never told the call failed. That array
 * used to be dropped on the floor here, so the honesty chiefd paid for never
 * reached the manager: the department was half-materialized and the answer was
 * an unqualified success. It is carried through to the tool result now.
 */
type AtomicDepartmentCreateResult =
  | { applied: true; departmentId: string; warnings: readonly string[] }
  | { refused: string; detail: string };

/** One person seed as `/v1/org/department/create` accepts it. */
export type ChiefdDepartmentPersonSeed = {
  kind: "hire-new";
  personId: string;
  name: string;
  title?: string;
  mandate: string;
  personKind: "head" | "worker";
  employmentState: "active" | "benched";
  tools?: string[];
};

/** The department definition a manager's `org_launch_department` call carries. */
export type IntercomDepartmentSpec = {
  id?: string;
  name: string;
  purpose: string;
  head: Record<string, unknown>;
  staff?: Array<Record<string, unknown>>;
};

function departmentPersonSeed(
  raw: Record<string, unknown>,
  personKind: "head" | "worker",
): ChiefdDepartmentPersonSeed {
  const text = (key: string): string | undefined => {
    const value = raw[key];
    return typeof value === "string" && value.trim() ? value.trim() : undefined;
  };
  const name = text("name");
  if (!name) throw new CallerRefusal(`Department ${personKind} requires a name`);
  const mandate = text("mandate");
  if (!mandate) throw new CallerRefusal(`Department ${personKind} '${name}' requires a mandate`);
  const tools = declarablePersonTools(raw, `Department ${personKind} '${name}'`);
  return {
    kind: "hire-new",
    // An absent id is chiefd's to mint (#751/R3). Sending "" says "you decide"
    // rather than inventing a second opinion about what this person is called;
    // the launcher's client-side minting is exactly what the port removes.
    personId: text("id") ?? "",
    name,
    ...(text("title") ? { title: text("title") } : {}),
    mandate,
    personKind,
    employmentState: raw.startActive === false ? "benched" : "active",
    ...(tools ? { tools } : {}),
  };
}

/**
 * The manager's department spec, as chiefd's `/v1/org/department/create` body.
 *
 * A PURE function, deliberately: this is the whole of the translation between
 * what a CEO's tool call carries and what the route accepts, and it is the part
 * that used to be spread across a CLI's argument parsing, a stdin JSON
 * document, and `planDepartmentCreate`'s client-side id minting. Keeping it
 * pure is what lets a unit test pin the mapping without a daemon, a subprocess,
 * or a company.
 *
 * Ids and titles it cannot know are sent EMPTY rather than invented — chiefd
 * mints them with the same rules genesis uses (`mint_department_create_ids`).
 */
/** Which kind of unit a create commits. A contract is the same row with
 *  transient engagement metadata attached. */
export type ChiefdCreateUnit =
  | { kind: "department" }
  | { kind: "contract"; transient: { engagement: string; launchedAt: string; expiresAt?: string } };

/** Hire the head, or appoint somebody who already works here. */
export type ChiefdDepartmentHead =
  | ChiefdDepartmentPersonSeed
  | { kind: "appoint-existing"; personId: string };

/**
 * What happens to the unit an appointee ALREADY heads.
 *
 * A person heads one unit here, and that is enforced in SQL
 * (`departments_one_head`), not only in the manifest validator. So appointing a
 * sitting head to lead a NEW unit must say what becomes of the old one, and
 * there are exactly two answers. `hand-over` promotes another member of that
 * unit. `dissolve` applies when the head is its LAST member: a unit's head must
 * be homed in the unit it heads, so a unit always holds at least one person,
 * and a unit that loses its last one cannot exist. Nobody is moved or
 * offboarded by a dissolve — there is nobody left in it to move.
 *
 * WHICH of the two applies is chiefd's answer, never this file's: the refusal
 * names the vacated unit and its eligible successors.
 */
export type ChiefdHeadVacancy =
  | { kind: "hand-over"; successorPersonId: string }
  | { kind: "dissolve" };

export interface ChiefdDepartmentCreateRequest {
  slug: string;
  requester: { kind: "person"; personId: string };
  departmentId: string;
  parentId: string;
  name: string;
  purpose: string;
  reason?: string;
  unit: ChiefdCreateUnit;
  head: ChiefdDepartmentHead;
  /** Required when the appointee already heads a unit; meaningless otherwise. */
  vacates?: ChiefdHeadVacancy;
  staff: ChiefdDepartmentPersonSeed[];
}

export function departmentCreateRequest(input: {
  slug: string;
  parentUnitId: string;
  spec: IntercomDepartmentSpec;
  requesterPersonId: string;
  reason?: string;
  /** A CONTRACT is a department row carrying transient engagement metadata —
   *  the same route, the same table, one typed field apart. */
  unit?: ChiefdCreateUnit;
  /** org-ops R3: an EXISTING person leads it, transferred in and appointed,
   *  instead of a new hire. Exactly one of this or `spec.head`. */
  existingHeadPersonId?: string;
  /** What becomes of the unit the appointee already heads, when they head one. */
  vacates?: ChiefdHeadVacancy;
}): ChiefdDepartmentCreateRequest {
  const { slug, parentUnitId, spec, requesterPersonId, reason } = input;
  if (!spec.name?.trim()) throw new CallerRefusal("A department requires a name");
  const head: ChiefdDepartmentHead = input.existingHeadPersonId?.trim()
    ? { kind: "appoint-existing", personId: input.existingHeadPersonId.trim() }
    : departmentPersonSeed(spec.head, "head");
  return {
    slug,
    requester: { kind: "person", personId: requesterPersonId },
    departmentId: spec.id?.trim() ?? "",
    parentId: parentUnitId,
    name: spec.name.trim(),
    purpose: spec.purpose?.trim() ?? "",
    ...(reason?.trim() ? { reason: reason.trim() } : {}),
    unit: input.unit ?? { kind: "department" },
    head,
    // Only an appoint-existing create can vacate anything. A hire-new head is
    // a person who did not exist a moment ago and heads nothing.
    ...(input.existingHeadPersonId && input.vacates ? { vacates: input.vacates } : {}),
    // An existing-head create takes no initial staff. The tool surface already
    // says so ("An existing-head create makes the head only").
    staff: input.existingHeadPersonId
      ? []
      : (spec.staff ?? []).map((member) => departmentPersonSeed(member, "worker")),
  };
}

/** The person an `org_hire` call describes, as chiefd's `/v1/org/person/hire`
 *  accepts it.
 *
 *  `hiringManagerPersonId` is GONE, and its absence is the rule rather than a
 *  simplification: it existed only so chiefd could refuse
 *  `hiring-manager-mismatch` when a hire tried to inherit a model route from a
 *  manager the caller had not attested. With no route to inherit, the field
 *  names nothing the route decides, and chiefd refuses it as an unknown field
 *  rather than accepting a value it would ignore. The attested caller is
 *  `requester`, which is the only authority question left. */
export interface ChiefdHireRequest {
  slug: string;
  requester: { kind: "person"; personId: string };
  personId: string;
  departmentId: string;
  name: string;
  title: string;
  mandate: string;
  employmentState: "active" | "benched";
  tools: string[];
}

/**
 * The whole of the translation between an `org_hire` tool call and the route.
 *
 * PURE, for the same reason `departmentCreateRequest` is: this used to be
 * spread across a CLI's argument parsing, a stdin JSON document, and that
 * CLI's own client-side id minting, and keeping it pure is what lets a unit
 * test pin the mapping without a daemon, a subprocess, or a company.
 *
 * Bucket A, not C: it decides nothing. The id and title it cannot know are
 * sent EMPTY for chiefd to mint with the rules genesis uses (`mint_hire_ids`)
 * — the CLI's `${department}-${slugify(name)}` was a second opinion about what
 * a person is called, and `title ?? ""` was not a default at all: it was sent
 * verbatim and refused `invalid-seed`, which is why a hire that named no title
 * could never succeed.
 */
export function hireRequest(input: {
  slug: string;
  departmentId: string;
  hiringManagerPersonId: string;
  person: Record<string, unknown>;
}): ChiefdHireRequest {
  const { person } = input;
  const text = (key: string): string | undefined => {
    const value = person[key];
    return typeof value === "string" && value.trim() ? value.trim() : undefined;
  };
  const name = text("name");
  if (!name) throw new CallerRefusal("A hire requires a name");
  const mandate = text("mandate");
  if (!mandate) throw new CallerRefusal(`Hire '${name}' requires a mandate`);
  const tools = declarablePersonTools(person, `Hire '${name}'`) ?? [];
  return {
    slug: input.slug,
    requester: { kind: "person", personId: input.hiringManagerPersonId },
    personId: text("id") ?? "",
    departmentId: input.departmentId,
    name,
    title: text("title") ?? "",
    mandate,
    employmentState: person.startActive === false ? "benched" : "active",
    tools,
  };
}

/** Pause / resume / remove one unit by id — the three verbs that need nothing
 *  but the id, and are the same operation for a department and a contract
 *  because a contract IS a department row with transient metadata. */
async function chiefdUnitStateChange(
  endpoint: ChiefdEndpoint,
  path: "/v1/org/department/pause" | "/v1/org/department/resume" | "/v1/org/department/remove-tree",
  body: { slug: string; departmentId: string },
): Promise<{ applied: true; removedDepartmentIds?: string[]; departedPersonIds?: string[] } | { refused: string; detail: string }> {
  try {
    const result = await postOrgRoute<{
      applied?: unknown; removedDepartmentIds?: unknown; departedPersonIds?: unknown;
    }>(chiefdTransport(endpoint), endpoint.url, path, body);
    if (result.applied !== true) throw new Error(`chiefd docstore ${path} returned an invalid outcome`);
    const ids = (value: unknown): string[] | undefined =>
      Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : undefined;
    return {
      applied: true,
      ...(ids(result.removedDepartmentIds) ? { removedDepartmentIds: ids(result.removedDepartmentIds) } : {}),
      ...(ids(result.departedPersonIds) ? { departedPersonIds: ids(result.departedPersonIds) } : {}),
    };
  } catch (error) {
    if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };
    throw error;
  }
}

/**
 * The one route the MULTI-unit resume posts to, named once.
 *
 * Its own literal rather than a member of `chiefdUnitStateChange`'s union: that
 * helper is the by-id family (`{slug, departmentId}`), and this route takes a
 * LIST plus `skipActive`. Reactivating several units is one transaction, not a
 * loop of single-unit resumes — a loop invalidates its own current-state view
 * and can never get past its first unit.
 */
const UNITS_RESUME_ROUTE = "/v1/org/department/resume-many" as const;

/** Resume several units together, all-or-nothing, with a refusal as a VALUE. */
async function chiefdUnitsResume(
  endpoint: ChiefdEndpoint,
  body: { slug: string; departmentIds: string[]; skipActive: boolean },
): Promise<{ applied: true } | { refused: string; detail: string }> {
  try {
    const result = await postOrgRoute<{ applied?: unknown }>(
      chiefdTransport(endpoint), endpoint.url, UNITS_RESUME_ROUTE, body,
    );
    if (result.applied !== true) throw new Error(`chiefd docstore ${UNITS_RESUME_ROUTE} returned an invalid outcome`);
    return { applied: true };
  } catch (error) {
    if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };
    throw error;
  }
}

/**
 * The `warnings` array a chiefd write route may attach to a 2xx.
 *
 * A route that has already committed reports a post-commit problem here rather
 * than as an error, so the decode has to be total: a missing, non-array or
 * mistyped field is "no warnings", never a thrown decode that would turn a
 * committed write back into a reported failure — the exact inversion this whole
 * seam exists to prevent.
 */
function routeWarnings(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((line): line is string => typeof line === "string" && line.trim().length > 0);
}

/**
 * Create one department through chiefd's own API.
 *
 * This replaced `spawn(bun, [apps/cli/src/Main.ts, "department", "launch", ...])`
 * — a Pi extension shelling out to a TypeScript CLI to reach the daemon it was
 * already connected to. That CLI is gone, so every department creation was
 * failing with `chiefd: unknown command 'department'`: a CEO could not build
 * its own company. A client calls its backend's API; it does not spawn a second
 * client to do it.
 *
 * Refusals come back as a typed VALUE, exactly like
 * `chiefdAtomicPersonTransfer` above: `postOrgRoute` decodes a 422 into an
 * `OrgRowRefusalError`, and a refusal is this route's answer, not its failure.
 */
async function chiefdCreateDepartment(
  endpoint: ChiefdEndpoint,
  body: ChiefdDepartmentCreateRequest,
): Promise<AtomicDepartmentCreateResult> {
  try {
    const result = await postOrgRoute<{ applied?: unknown; departmentId?: unknown; warnings?: unknown }>(
      chiefdTransport(endpoint), endpoint.url, "/v1/org/department/create", body,
    );
    if (result.applied === true && typeof result.departmentId === "string") {
      return { applied: true, departmentId: result.departmentId, warnings: routeWarnings(result.warnings) };
    }
    throw new Error("chiefd docstore /v1/org/department/create returned an invalid outcome");
  } catch (error) {
    if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };
    throw error;
  }
}

/**
 * Every chiefd route the staffing and structure tools post to (#751/P3).
 *
 * A closed union rather than a `string`: a verb reaches a path by naming it
 * here or not at all, so a new route can never arrive by concatenation, and
 * the seam classifier's route-literal inventory stays the real one.
 *
 * Two of these are deliberately the LIFECYCLE spelling of a verb that also has
 * a bare structural route. `/v1/org/person/bench-lifecycle` and
 * `/v1/org/staffing/lifecycle` run the activity transition — which is what
 * sheds launch intent and drives the pane teardown — around the same atomic
 * mutation `/v1/org/person/bench|offboard` performs alone. chiefd's
 * own note on the plain verb says why that matters: it "leaves the fence up for
 * the handoff window, and a person with no live pane can never
 * complete that handoff — the fence would hold a departed person's pane open
 * forever". A fired person whose pane never dies is precisely the class of
 * silent regression this port keeps finding, so the lifecycle route is the
 * faithful one and the structural route is not a shortcut to it.
 */
type StaffingRoutePath =
  | "/v1/org/department/reparent"
  | "/v1/org/department/move-members"
  | "/v1/org/person/appoint-head"
  | "/v1/org/person/replace-head-and-offboard"
  | "/v1/org/person/hire-preview"
  | "/v1/org/person/hire"
  | "/v1/org/person/bench-lifecycle"
  | "/v1/org/person/recall"
  | "/v1/org/person/start"
  | "/v1/org/person/shutdown"
  | "/v1/org/staffing/lifecycle"
  | "/v1/org/stand-down"
  | "/v1/org/stand-down/clear";

/** The two outcomes of a chiefd staffing/structure route. */
type StaffingRouteOutcome =
  | { applied: true; wire: Record<string, unknown> }
  | { refused: string; detail: string };

/**
 * POST one named staffing/structure route and decode its answer.
 *
 * Every route in this family replies `{"applied": true, …}` on 2xx and a
 * `{code, detail}` body on 400/404/422 — the three statuses the shared
 * `postOrgRoute` already decodes into an `OrgRowRefusalError`. Mapping that one
 * decoded error into a typed VALUE here, exactly as `chiefdAtomicPersonTransfer`
 * does, is what keeps a refusal chiefd's ANSWER rather than an exception a card
 * renders as a system fault. There is no second parser and no message matching.
 */
async function chiefdStaffingApplied(
  endpoint: ChiefdEndpoint,
  path: StaffingRoutePath,
  body: Record<string, unknown>,
): Promise<StaffingRouteOutcome> {
  try {
    const wire = await postOrgRoute<Record<string, unknown>>(chiefdTransport(endpoint), endpoint.url, path, body);
    if (wire?.applied !== true) throw new Error(`chiefd docstore ${path} returned an invalid outcome`);
    return { applied: true, wire };
  } catch (error) {
    if (error instanceof OrgRowRefusalError) return { refused: error.code, detail: error.detail };
    throw error;
  }
}

/**
 * Read one normalized organization aggregate through its dedicated typed route.
 *
 * The copied extension intentionally has no generic document escape hatch:
 * adding another store here requires naming its row route and wire field. That
 * keeps every live Pi caller on the same CompanyDb-backed authority as the
 * launcher instead of silently reviving the retired blob table.
 *
 * `journal-markers/`, `acks`, and `operator-escalation-intents` route through
 * `RowStoresClient` (the shared, typed client) rather than the generic
 * `chiefdPostJson` — the `supervision`/`activity`/`session-maintenance`
 * aggregates have no covering client method (`AggregatesClient`, which does
 * model those three, is not exported from the chiefing extension-runtime
 * barrel), so those three keep calling `postOrgRoute` directly at the same
 * routes chiefd already serves.
 */
async function chiefdReadNormalized(
  endpoint: ChiefdEndpoint,
  key: string,
  storeName: string,
  ifSeqNot?: number,
): Promise<
  | { unchanged: true; seq: number }
  | { unchanged: false; blob: string; seq: number }
  | undefined
> {
  if (storeName.startsWith("journal-markers/")) {
    const keyDigest = storeName.slice("journal-markers/".length);
    const row = await new RowStoresClient(chiefdTransport(endpoint), endpoint.url).readEventOnceMarker<unknown>(key, keyDigest);
    // Journal markers are immutable and this caller only probes existence, so
    // no conditional-read cursor is needed for the returned marker.
    return row.found ? { unchanged: false, blob: JSON.stringify(row.doc), seq: 0 } : undefined;
  }

  if (storeName === "supervision" || storeName === "activity" || storeName === "session-maintenance") {
    const r = await chiefdPostJson<{ found: boolean; ledger?: string; seq: number; unchanged?: boolean }>(
      endpoint,
      `/v1/org/${storeName}/read`,
      ifSeqNot === undefined ? { slug: key } : { slug: key, ifSeqNot },
    );
    if (r.unchanged) return { unchanged: true, seq: r.seq };
    return r.found && r.ledger !== undefined
      ? { unchanged: false, blob: r.ledger, seq: r.seq }
      : undefined;
  }

  if (storeName === "acks" || storeName === "operator-escalation-intents") {
    // `RowStoresClient.readAcks`/`readOperatorEscalationIntents` return an
    // already-parsed `OrgRowReadResult`, which drops the `unchanged`/`seq`
    // fields this cache's `ifSeqNot` conditional-read short-circuit needs —
    // called directly via `postOrgRoute` at the same route instead, keeping
    // the exact wire shape chiefd already answers.
    const r = await chiefdPostJson<{ found: boolean; doc?: string; seq: number; unchanged?: boolean }>(
      endpoint,
      `/v1/org/${storeName}/read`,
      ifSeqNot === undefined ? { slug: key } : { slug: key, ifSeqNot },
    );
    if (r.unchanged) return { unchanged: true, seq: r.seq };
    return r.found && r.doc !== undefined
      ? { unchanged: false, blob: r.doc, seq: r.seq }
      : undefined;
  }

  throw new Error(`No normalized organization route is registered for store '${storeName}'`);
}

async function readDurableDocument(context: OrganizationRuntimeContext, storeName: string): Promise<unknown> {
  const row = await chiefdReadNormalized(chiefdEndpoint(context), companyKeyOf(context), storeName);
  return row && !row.unchanged ? JSON.parse(row.blob) : undefined;
}

/**
 * #149 companion (#10): the last blob+seq observed per (url,key,store),
 * so a REPEATED read of a live document (`supervision`, `activity`) can send the
 * cached seq as `ifSeqNot`. Typed row reads currently return the complete aggregate,
 * but the local cache still avoids replacing/re-retaining an identical blob.
 * Keyed per-process; a CEO's extension reads only its own company, so this
 * holds at most a handful of entries.
 */
const conditionalReadCache = new Map<string, { seq: number; blob: string }>();

/** TEST-ONLY: drop every cached live-read seq (a fresh process starts empty). */
export function resetConditionalReadCacheForTest(): void {
  conditionalReadCache.clear();
}

/**
 * Read one durable document from its normalized row route.
 *
 * SAFETY: this ALWAYS round-trips to chiefd. A concurrent commit advances the
 * row seq and returns a fresh aggregate. Re-parses the cached blob on an equal
 * seq (a fresh object each call) so no caller can mutate another's snapshot.
 */
export async function readDurableDocumentCached(
  context: OrganizationRuntimeContext,
  storeName: string,
): Promise<unknown> {
  const endpoint = chiefdEndpoint(context);
  const key = companyKeyOf(context);
  const cacheKey = `${endpoint.url}\x00${key}\x00${storeName}`;
  const cached = conditionalReadCache.get(cacheKey);
  const row = await chiefdReadNormalized(endpoint, key, storeName, cached?.seq);
  if (!row) {
    conditionalReadCache.delete(cacheKey);
    return undefined;
  }
  if (row.unchanged && cached) return JSON.parse(cached.blob);
  if (row.unchanged) {
    conditionalReadCache.delete(cacheKey);
    return undefined;
  }
  conditionalReadCache.set(cacheKey, { seq: row.seq, blob: row.blob });
  return JSON.parse(row.blob);
}

/**
 * #384: the class of failure that means "the store/runtime did not answer THIS
 * INSTANT" (a restart, a brief network blip, runtime not yet spawned) rather
 * than "this data is missing/corrupt/refused". Reused to (a) decide whether
 * a read/command worth retrying should retry with a brief backoff, and (b)
 * recognize, at a core-op tool's own catch block, that a failure -- even
 * after retries are exhausted -- is still this transient class, so the tool
 * degrades to a legible "<capability> is temporarily unavailable ... re-issue
 * this call"
 * result instead of surfacing the raw transport exception (which names an
 * internal URL/path) into the agent session. Any OTHER error (corrupt
 * ledger, wrong store, refused write) is a genuine
 * failure and keeps surfacing exactly as before -- #121's loud-failure
 * guarantee for real misconfiguration is untouched. Same class #343 already
 * carved out for `hasOpenOrganizationWork` (there via a narrower
 * `/chiefd docstore/` test); this generalizes it to the runtime-unreachable
 * sibling and gives every core-op path below the same treatment.
 */
/**
 * #384's transient class, now split per E2's migration table: the chiefd
 * arms (`chiefd docstore.*unreachable|write-service .*unreachable`) migrate
 * to the shared, structural `isTransientChiefdError` (a
 * `ChiefdUnavailableError` that is either `kind: 'unreachable'` or a
 * `kind: 'http-error'` with `status === 429` — never a message regex). The
 * 429 arm is not an afterthought: see `chiefing/src/Errors.ts`, whose own doc
 * records why the one status meaning "back off and ask again" belongs in the
 * transient class. This comment claimed `unreachable` was the only member. The runtime/spawn/launcher arms are NOT chiefd traffic and STAY
 * string-matched, unchanged.
 *
 * #983 added a REGISTRY arm here — a `BeacondUnavailableError` with kind
 * `unreachable`/`timeout` — because an install asked beacond where its
 * company's daemon was, so a registry blip was the same restart-window blip as
 * a chiefd one. That arm is DELETED with the question it answered: an install
 * reads `<dir>/.chief/run/daemon.json` now, and a local file read has no
 * network blip to ride out. Nothing in this file talks to beacond at all.
 */
export function isTransientTransportFailure(error: unknown): boolean {
  if (isTransientChiefdError(error)) return true;
  const message = safeExceptionMessage(error);
  return /no server running|ECONNREFUSED|connection refused|spawn .*E(AGAIN|MFILE|NFILE)|posix_spawn|(?:ChiefD|Launcher) command ended without an exit status/i.test(message);
}

/**
 * Brief, bounded backoff for a transient transport retry -- since #751/G9-S0
 * deleted the lock-busy ladder that used to sit beside it, this is the ONLY
 * retry `runChecked` performs. Two retries (three attempts total) resolve a
 * restart-class blip in well under a second; a genuinely down dependency still
 * fails within ~1s instead of hanging the tool call. Contention is not retried
 * here at all: whoever refuses owns both the refusal and the retry advice.
 */
const TRANSIENT_TRANSPORT_RETRY_DELAYS_MS = [150, 400] as const;

/**
 * Awaited backoff -- the only sleep primitive this file uses.
 * `synchronousTransientBackoff` (`Atomics.wait`) and its two synchronous
 * retry wrappers (`withTransientReadRetry`, `withBootTransientRetry`) are
 * DELETED (E4-S8): parking the whole JS thread would prevent this pane's SSE
 * reader, footer, mailbox, and the very `fetch` trying to recover from
 * making progress at all. Every retry path below is `withTransientReadRetryAsync`.
 */
function asynchronousTransientBackoff(ms: number): Promise<void> {
  if (ms <= 0) return Promise.resolve();
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/**
 * The BOOT ladder: a pane that comes up while its chiefd is mid-restart (a
 * daemon swap, a deploy bounce, an e2e `--once` window — #428) must not die
 * at extension install. Every runtime read above tolerates the brief blip;
 * the install-time authority read had NO retry at all, so a fresh-session
 * re-exec landing inside a restart window threw at load, Pi exited, and the
 * pane was gone for good. Boot is the one place a longer wait is right — the
 * window is still bounded — a genuinely misconfigured store fails loudly
 * with the exact same message, ~8s later (#121's guarantee is preserved, not
 * weakened). Only the transient transport class retries; every other failure
 * (corrupt ledger, wrong store, refused write) throws on first sight.
 */
const BOOT_TRANSIENT_RETRY_DELAYS_MS = [150, 400, 1000, 2000, 4000] as const;

/**
 * The one retry primitive every awaited docstore read in this file uses:
 * identical classifier (the SHARED `isTransientTransportFailure` — never a
 * second copy of that predicate; a divergent duplicate of it is what made
 * #59's retry ladder dead code), identical rethrow contract, and — because
 * the wait between attempts is `await`ed, never `Atomics.wait` — it yields
 * the event loop instead of parking it. `delaysMs` defaults to the short
 * runtime ladder; the boot call site passes the longer
 * `BOOT_TRANSIENT_RETRY_DELAYS_MS`.
 */
export async function withTransientReadRetryAsync<T>(
  read: () => T | Promise<T>,
  delaysMs: readonly number[] = TRANSIENT_TRANSPORT_RETRY_DELAYS_MS,
): Promise<T> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await read();
    } catch (error) {
      if (attempt >= delaysMs.length || !isTransientTransportFailure(error)) throw error;
      await asynchronousTransientBackoff(delaysMs[attempt]!);
    }
  }
}

/**
 * Legible, URL-free degrade text for a core-op tool's catch block:
 * `undefined` when `error` is not the transient transport class (the caller
 * falls through to its existing raw-message behavior, unchanged).
 */
/**
 * "RETRYING" NAMED SOMETHING THAT HAD ALREADY STOPPED.
 *
 * This message is produced at a tool's own catch block, and the doc comment on
 * the classifier above says exactly when: "even after retries are exhausted".
 * So at the instant an agent reads `… is temporarily unavailable, retrying.`,
 * nothing is retrying. The backoff ladder ran and gave up, and the tool call
 * is over.
 *
 * To a machine that is not a soft edge, it is an instruction: a recovery
 * reported as in flight is a recovery you wait for, so the agent waits — for a
 * retry that no longer exists — instead of re-issuing the call that is the
 * only thing that can succeed. The `retryable: true` every call site already
 * attaches says the truth in the details; the prose contradicted it.
 *
 * The capability name is kept (it is what makes the line legible without the
 * internal URL the raw exception carries) and the claim is replaced by the
 * state and the next action.
 *
 * Exported for the same reason `runtimeConvergenceWarning` is: so the rule
 * ("never report a recovery that is not running") is unit-testable without
 * booting a company. It is bucket B, not C — the DECISION is
 * `isTransientTransportFailure`, which is already a C row; this only words its
 * answer.
 */
export function transientDegradeMessage(capability: string, error: unknown): string | undefined {
  return isTransientTransportFailure(error)
    ? `${capability} is temporarily unavailable: the store did not answer and this call's automatic retries are used up.`
      + " Nothing is retrying in the background — re-issue this call."
    : undefined;
}

// -------------------------------------------------------------------------
// Mailbox family — the ROW path (org-mailbox-store's one-family authority).
// This extension is copied into each Pi home and cannot import from `../src/`,
// so the per-person mailbox ops that `org-mailbox-store.ts` owns in-process are
// replicated here over the SAME rows via chiefd's typed row routes
// (`/v1/org/mailbox/read-person`, `/delta`, `/list-persons`). The `slug` those
// routes take is the company key — identical to the store's own label — and
// the durable envelope file-key is byte-identical to the one
// every producer writes, so a producer here and a ROW consumer in the store,
// transport, drain, health-monitor or footer see ONE mailbox. This closes the
// two-authorities split: the intercom no longer writes the old `/v1/docs` blob
// 3-family (`mailbox/`, `mailbox-archive/`, `mailbox-index/`) that row readers
// could not see. Property #4 is structural: an absent person reads as an empty
// mailbox; an unreachable store THROWS out of `chiefdPostJson` and MUST
// propagate — never swallowed into "no mail" (the 19-hour outage).
// -------------------------------------------------------------------------
type MailboxBucket = "pending" | "accepted" | "superseded" | "rejected" | "resolved";
type MailboxTerminalBucket = Exclude<MailboxBucket, "pending">;
const MAILBOX_TERMINAL_BUCKETS: readonly MailboxTerminalBucket[] = ["accepted", "superseded", "rejected", "resolved"];

/**
 * The six row states the `mailbox` table's `state` column carries. `delivered`
 * is chiefd-converge-owned (the fence-archive terminal, #493): it has no view
 * bucket of its own — it shows as `pending` in the view and a TS write must
 * PRESERVE it (never clobber a `delivered` row to `pending`), so `rawState`
 * carries the actual state alongside the view.
 */
type MailboxRowState = "pending" | "delivered" | MailboxTerminalBucket;

/**
 * One person's mailbox as the caller sees it: five logical buckets, each a
 * `file-key -> envelope` map. A VIEW reconstructed from that person's rows on
 * every read, never persisted. Mirrors `MailboxDocument` in
 * `src/organization/org-mailbox-store.ts`.
 */
interface MailboxDoc {
  personId: string;
  pending: Record<string, OrganizationEnvelope>;
  accepted: Record<string, OrganizationEnvelope>;
  superseded: Record<string, OrganizationEnvelope>;
  rejected: Record<string, OrganizationEnvelope>;
  resolved: Record<string, OrganizationEnvelope>;
  /** file-key -> the ACTUAL row state (incl. `delivered`), so an upsert can
   * preserve a state the view collapses to `pending`. Not a logical bucket. */
  rawState: Record<string, MailboxRowState>;
}

/**
 * The wire shape of one columnar row: the flattened envelope (camelCase) plus
 * this row's `person`, lifecycle `state` and `updatedAt` — exactly what
 * `chiefd_core::store::mailbox_rows::MailboxEntry` serializes to, and what
 * `MailboxWireEntry` in `org-mailbox-store.ts` carries.
 */
type MailboxWireEntry = Record<string, unknown> & {
  id: string;
  createdAt: string;
  person: string;
  state: MailboxRowState;
  updatedAt: number;
};

/** The store name for one person's mailbox — retained as the naming contract the
 * chiefd change-feed event mirrors so the SSE wake watcher keeps matching
 * `mailbox/<personId>`. */
function mailboxStoreName(personId: string): string {
  return `mailbox/${personId}`;
}

/** Byte-identical to the transport's `mailboxEnvelopeKey` / the old filename /
 * the key `org-mailbox-store.ts` computes. */
function mailboxEnvelopeKey(envelope: OrganizationEnvelope): string {
  return `${envelope.createdAt.replaceAll(":", "-")}-${envelope.id}.json`;
}

/** The `id@person` composite that is a row's primary key — constructed only to
 * name a row for deletion. */
function envelopeRowId(envelope: OrganizationEnvelope, personId: string): string {
  return `${envelope.id}@${personId}`;
}

function emptyMailboxDoc(personId: string): MailboxDoc {
  return { personId, pending: {}, accepted: {}, superseded: {}, rejected: {}, resolved: {}, rawState: {} };
}

/** Which view bucket a row state belongs to. `delivered` has no bucket of its
 * own; it is shown as `pending` and preserved on write via `rawState`. */
function mailboxViewBucket(state: MailboxRowState): MailboxBucket {
  return state === "delivered" ? "pending" : state;
}

/** The row route `slug` — the company key that IS the store's own label and
 * every `/v1/docs` blob key this file also uses. */
function mailboxRouteSlug(context: OrganizationRuntimeContext): string {
  return companyKeyOf(context);
}

/** The person's mailbox VIEW, or `undefined` when they have no row (empty).
 * THROWS on an unreachable store — callers MUST let that propagate (property
 * #4). Reconstructed from `/v1/org/mailbox/read-person`. */
async function readMailboxDoc(context: OrganizationRuntimeContext, personId: string): Promise<MailboxDoc | undefined> {
  const read = await chiefdPostJson<{ found: boolean; mailbox?: string; seq?: number }>(
    chiefdEndpoint(context), "/v1/org/mailbox/read-person", { slug: mailboxRouteSlug(context), personId },
  );
  if (!read.found || read.mailbox === undefined) return undefined;
  const wire = JSON.parse(read.mailbox) as { entries: MailboxWireEntry[] };
  if (!wire.entries.length) return undefined;
  const doc = emptyMailboxDoc(personId);
  for (const entry of wire.entries) {
    const { person: _person, state, updatedAt: _updatedAt, ...rest } = entry;
    void _person;
    void _updatedAt;
    // `organization` is DERIVED: chiefd's reconstruct stamps it with the
    // CompanyDb label — the company key (`sha256(<dir>)[..12]`) — not the
    // display slug every producer wrote and every consumer compares against.
    // Restore it
    // so the send-side content-equality dedup (existingMailboxMessage /
    // publishMailboxEnvelope) and the drain identity check keep matching.
    if ("organization" in rest) {
      (rest as { organization?: string }).organization = context.organization;
    }
    const envelope = rest as unknown as OrganizationEnvelope;
    const key = mailboxEnvelopeKey(envelope);
    doc.rawState[key] = state;
    doc[mailboxViewBucket(state)][key] = envelope;
  }
  return doc;
}

/** Apply upserts and deletes as ONE fence-free per-person `mailboxDelta` — the
 * row analogue of the old blob CAS. Disjoint persons and disjoint envelopes
 * never serialize on one another; the same envelope id is an idempotent upsert. */
async function mailboxDelta(
  context: OrganizationRuntimeContext,
  personId: string,
  upserts: readonly MailboxWireEntry[],
  deletes: readonly string[],
): Promise<void> {
  if (!upserts.length && !deletes.length) return;
  await chiefdPostJson<{ applied: boolean; seq?: number }>(
    chiefdEndpoint(context), "/v1/org/mailbox/delta",
    { slug: mailboxRouteSlug(context), personId, upserts: JSON.stringify(upserts), deletes, at: new Date().toISOString() },
  );
}

/** One wire upsert for `envelope` at `state` for `personId`. */
function mailboxUpsertEntry(envelope: OrganizationEnvelope, personId: string, state: MailboxRowState): MailboxWireEntry {
  return {
    ...(envelope as unknown as Record<string, unknown>),
    person: personId,
    state,
    updatedAt: Date.now(),
  } as MailboxWireEntry;
}

/** Move one pending key into a terminal bucket — one per-envelope upsert that
 * changes the row's `state` (its PK, `id@person`, is unchanged). Idempotent by
 * identity; returns the envelope that moved, or `undefined` when the key was not
 * pending. */
async function settleMailboxEntry(
  context: OrganizationRuntimeContext,
  personId: string,
  key: string,
  to: MailboxTerminalBucket,
): Promise<OrganizationEnvelope | undefined> {
  const doc = await readMailboxDoc(context, personId);
  const envelope = doc?.pending[key];
  if (!doc || !envelope) return undefined;
  await mailboxDelta(context, personId, [mailboxUpsertEntry(envelope, personId, to)], []);
  return envelope;
}

/** Settle one exact batch through the normalized per-person mailbox authority.
 * The row endpoint applies all state transitions in one transaction; a stale
 * or replaced member makes the batch a no-op so a later retry sees every item.
 */
async function settleMailboxBatch(
  context: OrganizationRuntimeContext,
  personId: string,
  entries: readonly { key: string; envelope: OrganizationEnvelope }[],
  to: MailboxTerminalBucket,
): Promise<OrganizationEnvelope[] | undefined> {
  if (!entries.length || new Set(entries.map(({ key }) => key)).size !== entries.length) return undefined;
  const doc = await readMailboxDoc(context, personId);
  if (!doc || entries.some(({ key, envelope }) => !doc.pending[key] || !messageContentMatches(doc.pending[key]!, envelope))) return undefined;
  await mailboxDelta(context, personId, entries.map(({ envelope }) => mailboxUpsertEntry(envelope, personId, to)), []);
  return entries.map(({ envelope }) => envelope);
}

/** Locate an envelope by message id across a bucket set, over the in-hand VIEW.
 * No IO: the view already holds all of one person's rows, so a lookup is a
 * filter. A message id with more than one durable copy in `pending` is a loud
 * refusal, not a silent pick. */
function findMailboxEntryByMessageId(
  _context: OrganizationRuntimeContext,
  doc: MailboxDoc,
  messageId: string,
  buckets: readonly MailboxBucket[],
): { bucket: MailboxBucket; key: string; envelope: OrganizationEnvelope } | undefined {
  const suffix = `-${messageId}.json`;
  if (buckets.includes("pending")) {
    const keys = Object.keys(doc.pending).filter((key) => key.endsWith(suffix)).sort();
    if (keys.length > 1) throw new Error(`Message id '${messageId}' has multiple durable copies for '${doc.personId}'`);
    if (keys.length === 1) return { bucket: "pending", key: keys[0]!, envelope: doc.pending[keys[0]!]! };
  }
  for (const bucket of MAILBOX_TERMINAL_BUCKETS) {
    if (!buckets.includes(bucket)) continue;
    const keys = Object.keys(doc[bucket]).filter((key) => key.endsWith(suffix)).sort();
    if (keys.length) return { bucket, key: keys[0]!, envelope: doc[bucket][keys[0]!]! };
  }
  return undefined;
}

/** Publish one envelope into a recipient's pending bucket. Dedup by message id
 * across pending/accepted (the key embeds `createdAt`, so the first write owns
 * it); conflicting content for the same id is a loud refusal. The upsert is
 * idempotent on the row PK, so a concurrent identical publish is a no-op. */
async function publishMailboxEnvelope(context: OrganizationRuntimeContext, recipient: string, envelope: OrganizationEnvelope): Promise<void> {
  const doc = (await readMailboxDoc(context, recipient)) ?? emptyMailboxDoc(recipient);
  const existing = findMailboxEntryByMessageId(context, doc, envelope.id, ["pending", "accepted"]);
  if (existing) {
    if (!messageReplayContentMatches(existing.envelope, envelope)) {
      throw new CallerRefusal(`Message id '${envelope.id}' already has conflicting content for '${recipient}'`);
    }
    return;
  }
  await mailboxDelta(context, recipient, [mailboxUpsertEntry(envelope, recipient, "pending")], []);
}

async function loadRosterActivity(
  context: OrganizationRuntimeContext,
  manifest: IntercomOrganizationManifest,
): Promise<RosterActivityLedger> {
  // Durable state is SQL-only: activity is reconstructed from its normalized
  // rows. It used to travel through `readDurableDocumentCached` while the
  // activity blob existed; after P0 that generic document route correctly
  // returns absent, which made every live `org_roster` report a missing
  // authority even though Chiefd held a healthy activity aggregate.
  const path = `sql:${manifest.slug}/activity`;
  // A corrupt document must stay a bounded, actionable failure rather than a
  // raw parser error leaking out of the store.
  let stored: unknown;
  try {
    // #384: a transient docstore blip retries with a brief backoff before this
    // throws -- org_roster must not surface a raw "unreachable at <url>" on a
    // restart-class hiccup that would have resolved a beat later.
    const wire = await withTransientReadRetryAsync(() => chiefdPostJson<{ found: boolean; ledger?: string }>(
      chiefdEndpoint(context), "/v1/org/activity/read",
      { slug: companyKeyOf(context) },
    ));
    stored = wire.found && wire.ledger !== undefined ? JSON.parse(wire.ledger) : undefined;
  } catch (error) {
    throw new Error(`Cannot read activity authority '${path}': ${safeExceptionMessage(error)}`);
  }
  if (stored === undefined) throw new Error(`Activity authority '${path}' is missing`);
  const raw = object(stored, "activity authority") as unknown as RosterActivityLedger;
  if (raw.schemaVersion !== 1 || raw.organization !== manifest.slug) {
    throw new Error(`Activity authority '${path}' is stale or corrupt`);
  }
  const people = object(raw.people, "activity people") as unknown as Record<string, RosterActivityPerson>;
  const transitions = object(raw.transitions, "activity transitions") as unknown as Record<string, RosterActivityTransition>;
  let personOrder = exactStringOrder(raw.personOrder, people, "activity person");
  const transitionOrder = exactStringOrder(raw.transitionOrder, transitions, "activity transition");
  if (JSON.stringify(personOrder) !== JSON.stringify(manifest.peopleOrder)) {
    // #526 (reconcile-on-read, sibling of the #28 organization-shape tolerance
    // above): a department/contract delete SHRINKS manifest.peopleOrder while the
    // activity ledger still lists the removed people, so a strict equality check
    // wedged the WHOLE roster read as "stale person order" — but a pure shrink is
    // reconcilable drift, not corruption (the writer's reconcile realigns it on
    // the next mutation). Tolerate ONLY a pure shrink: every manifest person still
    // present in the ledger, in the same relative order, with only removed people
    // extra. Realign to the manifest order in memory (this read is non-mutating;
    // the durable repair lands on the next mutation). Anything else — a manifest
    // person MISSING from the ledger (a grow the read cannot fabricate) or a
    // reordering — stays hard corrupt.
    const manifestSet = new Set(manifest.peopleOrder);
    const shrunk = personOrder.filter((personId) => manifestSet.has(personId));
    if (JSON.stringify(shrunk) !== JSON.stringify(manifest.peopleOrder)) {
      throw new Error(`Activity authority '${path}' has a stale person order`);
    }
    for (const personId of personOrder) if (!manifestSet.has(personId)) delete people[personId];
    personOrder = shrunk;
  }
  for (const personId of personOrder) {
    const state = object(people[personId], `activity person '${personId}'`) as unknown as RosterActivityPerson;
    // The window is DERIVED, never read off the document. This clause used to
    // be `!manifest.departments[state.lastPaneDepartmentId]`, which is exactly
    // as fail-closed as before — an unplaceable person is still a corrupt
    // record — but it now checks the CURRENT tree instead of a column chiefd
    // stopped writing.
    if (state.personId !== personId || typeof state.lastDesiredActive !== "boolean" || !personDepartmentId(manifest, personId)) {
      throw new Error(`Activity person '${personId}' is stale or corrupt`);
    }
    if (state.activeTransitionId) {
      const transition = transitions[state.activeTransitionId];
      if (!transition || transition.personId !== personId || transition.status === "cancelled") {
        throw new Error(`Activity person '${personId}' has an invalid active transition`);
      }
    }
  }
  for (const transitionId of transitionOrder) {
    const transition = object(transitions[transitionId], `activity transition '${transitionId}'`) as unknown as RosterActivityTransition;
    if (transition.id !== transitionId || !manifest.people[transition.personId]
      || !(["park", "transfer", "offboard"] as const).includes(transition.action)
      || !(["awaiting_handoff", "overdue", "ready", "applied", "cancelled", "forced"] as const).includes(transition.status)) {
      throw new Error(`Activity transition '${transitionId}' is corrupt`);
    }
  }
  return { ...raw, people, transitions, personOrder, transitionOrder };
}

/**
 * Join the two independently persisted disk projections. This reports what the last
 * successful reconcile observed; it never treats desired manifest placement
 * or ambient runtime content as proof that a pane is running.
 */
export async function loadOrganizationRosterObservation(
  context: OrganizationRuntimeContext,
  manifest?: IntercomOrganizationManifest,
): Promise<OrganizationRosterObservation> {
  manifest ??= await loadIntercomOrganization(context);
  const activity = await loadRosterActivity(context, manifest);
  // The runtime projection is a normalized ROW store (org-data-normalization),
  // read via the shared `RowStoresClient.readRuntime` (reconstructed from its
  // rows). Absence is the same "no observation yet" state the missing file
  // used to represent.
  const endpoint = chiefdEndpoint(context);
  const runtimeRow = await new RowStoresClient(chiefdTransport(endpoint), endpoint.url).readRuntime<unknown>(
    companyKeyOf(context),
  );
  const runtimeRecord = runtimeRow.found ? runtimeRow.doc : undefined;
  if (runtimeRecord === undefined) {
    return {
      organization: manifest.slug,
      status: "absent",
      people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, { personId, state: "absent" }])),
    };
  }

  const raw = object(runtimeRecord, "runtime observation") as unknown as RosterRuntimeProjection;
  if (raw.version !== 1) throw new Error(`Runtime observation for '${manifest.slug}' has an unsupported version`);
  // AC6: a `raw.session !== manifest.runtimeSession` throw stood here. chiefd
  // no longer serves either side of it — the runtime row's `session` column is
  // retired and the manifest field is deleted — so left as-is it would have
  // compared `undefined` against `undefined` and, with only one of the two
  // removed, thrown on EVERY roster load. Both sides were `org-<slug>` for
  // this company's own slug, so the check never discriminated anything.
  const processes = processHandles(raw.processHandles, "runtime processes");
  if (raw.observedAt === null && raw.status === undefined && !Object.keys(processes).length) {
    return {
      organization: manifest.slug,
      status: "absent",
      people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, { personId, state: "absent" }])),
    };
  }
  if (raw.status !== "starting" && raw.status !== "recovering" && raw.status !== "running" && raw.status !== "idle" && raw.status !== "stopped") {
    throw new Error(`Runtime observation for '${manifest.slug}' has invalid status '${String(raw.status)}'`);
  }
  if (typeof raw.observedAt !== "string" || !Number.isFinite(Date.parse(raw.observedAt))) throw new Error(`Runtime observation for '${manifest.slug}' has an invalid observation time`);
  const reconciliation = raw.reconciliation === undefined ? undefined : object(raw.reconciliation, "runtime reconciliation") as unknown as NonNullable<RosterRuntimeProjection["reconciliation"]>;
  if (reconciliation && (reconciliation.phase !== "in_progress"
    || typeof reconciliation.startedAt !== "string" || !Number.isFinite(Date.parse(reconciliation.startedAt)))) {
    throw new Error(`Runtime observation for '${manifest.slug}' has an invalid in-progress reconciliation`);
  }
  const projectedReconciliation = reconciliation ? {
    phase: "in_progress" as const,
    startedAt: reconciliation.startedAt,
  } : undefined;
  for (const personId of Object.keys(processes)) if (!manifest.people[personId]) throw new Error(`Runtime observation contains unknown person '${personId}'`);

  // org-runtime writes this atomic, short-lived marker before runtime has been
  // reconciled. It is a legitimate observation, not a corrupt projection.
  if (raw.status === "starting") {
    if (Object.keys(processes).length) throw new Error("Starting runtime observation retains observed processes");
    return {
      organization: manifest.slug,
      status: "starting",
      observedAt: raw.observedAt,
      ...(projectedReconciliation ? { reconciliation: projectedReconciliation } : {}),
      people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, {
        personId,
        state: manifest.people[personId]!.employmentState === "departed" ? "departed" : "starting",
      }])),
    };
  }

  if (raw.status === "stopped") {
    if (Object.keys(processes).length) throw new Error("Stopped runtime observation retains observed processes");
    return {
      organization: manifest.slug,
      status: "stopped",
      observedAt: raw.observedAt,
      ...(projectedReconciliation ? { reconciliation: projectedReconciliation } : {}),
      people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, { personId, state: "stopped" }])),
    };
  }

  // A read-only ownership audit can prove that a formerly observed process is
  // gone without creating a replacement. Keep that fact visible instead of
  // presenting stale ids as live or pretending durable work was parked. A
  // normal reconcile remains the only path allowed to restore a missing lease.
  if (raw.status === "recovering") {
    const people: Record<string, OrganizationRosterPersonObservation> = {};
    for (const personId of manifest.peopleOrder) {
      const person = manifest.people[personId]!;
      const state = activity.people[personId]!;
      // KEY PRESENCE, never the value: chiefd publishes `""` for a person it
      // proved alive without reading a pid, and `Boolean("")` is false.
      const alive = Object.hasOwn(processes, personId);
      const processId = processes[personId];
      const transition = state.activeTransitionId ? activity.transitions[state.activeTransitionId] : undefined;
      const department = personDepartmentId(manifest, personId);
      if (alive && department && state.lastDesiredActive) {
        people[personId] = transition && transition.status !== "cancelled" && transition.status !== "applied" && transition.status !== "forced"
          ? {
            personId,
            state: "handoff-held",
            ...(processId ? { processId } : {}),
            departmentId: department,
            transitionId: transition.id,
            transitionAction: transition.action,
            transitionStatus: transition.status,
          }
          : { personId, state: "running", ...(processId ? { processId } : {}), departmentId: department };
      } else if (person.employmentState === "departed") {
        people[personId] = { personId, state: "departed" };
      } else if (state.lastDesiredActive || alive) {
        people[personId] = {
          personId,
          state: "recovering",
          ...(alive && department ? { ...(processId ? { processId } : {}), departmentId: department } : {}),
        };
      } else {
        people[personId] = { personId, state: "parked" };
      }
    }
    return {
      organization: manifest.slug,
      status: "recovering",
      observedAt: raw.observedAt,
      ...(projectedReconciliation ? { reconciliation: projectedReconciliation } : {}),
      people,
    };
  }

  if (raw.status === "idle") {
    if (Object.keys(processes).length) throw new Error("Idle runtime observation retains observed processes");
    if (projectedReconciliation) {
      return {
        organization: manifest.slug,
        status: "starting",
        observedAt: raw.observedAt,
        reconciliation: projectedReconciliation,
        people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, {
          personId,
          state: manifest.people[personId]!.employmentState === "departed"
            ? "departed"
            : activity.people[personId]!.lastDesiredActive ? "starting" : "parked",
        }])),
      };
    }
    return {
      organization: manifest.slug,
      status: "idle",
      observedAt: raw.observedAt,
      people: Object.fromEntries(manifest.peopleOrder.map((personId) => [personId, {
        personId,
        state: manifest.people[personId]!.employmentState === "departed" ? "departed" : "parked",
      }])),
    };
  }
  if (!Object.keys(processes).length) throw new Error("Running runtime observation has no observed processes");
  // TOMBSTONE (chief-home-is-cwd §4c): an `if (ceoBootLease) { … }` arm stood
  // here. While a CEO boot lease was held it read the observation as the exact
  // CEO-only projection — exactly one live process, the root's, everybody else
  // `suppressed` — and threw when it was anything else. The lease is deleted
  // with the daemon-side CEO boot, so no observation can be labelled that way
  // and the ordinary interpretation below is the only one.
  const people: Record<string, OrganizationRosterPersonObservation> = {};
  let projectionRecovering = false;
  const missingProcessPersonIds: string[] = [];
  const unexpectedProcessPersonIds: string[] = [];
  for (const personId of manifest.peopleOrder) {
    const person = manifest.people[personId]!;
    const state = activity.people[personId]!;
    // KEY PRESENCE, never the value: an alive person whose pid the actuator
    // could not read is published as `""`, and `Boolean("")` reported that
    // person parked while their process was running.
    const alive = Object.hasOwn(processes, personId);
    const processId = processes[personId];
    const transition = state.activeTransitionId ? activity.transitions[state.activeTransitionId] : undefined;
    const department = personDepartmentId(manifest, personId);
    // A process can disagree with desired-active transiently, but a person the
    // runtime is running must still be placeable in the CURRENT tree. Validate
    // that structure before entering the tolerant divergence branch so
    // corruption never masquerades as convergence lag.
    //
    // The `!windows[department]` arm this test used to carry is DELETED, not
    // relaxed: `windows` is empty on every chiefd path, so it threw "has no
    // observed window for '<dept>'" for every running person in every company.
    if (alive && !department) {
      throw new Error(`Runtime observation for '${personId}' names no derivable department`);
    }
    if (alive !== state.lastDesiredActive) {
      // A momentary pane-vs-desired-active disagreement is RECONCILABLE, not
      // corruption: mid fresh-session / replacement / CEO-only-boot convergence
      // the live projection and the activity desired-active flag legitimately
      // disagree for a tick (e.g. desired-active=true with no pane yet, awaiting
      // the reconcile that starts it; or a live pane whose activity flag has not
      // caught up). Hard-throwing here faulted the WHOLE roster read as a system
      // fault ("Runtime observation for '<person>' disagrees with activity
      // desired-active state") and — worse — the converge duty skipped the person
      // rather than actuating the desired state, so nothing ever came up (live:
      // engineering-head desired-active with no pane, stuck). The divergence is
      // transient and convergeable, so report `recovering` and let the
      // reconcile actuate — never reject the read or stall on it.
      projectionRecovering = true;
      (alive ? unexpectedProcessPersonIds : missingProcessPersonIds).push(personId);
      people[personId] = {
        personId,
        state: "recovering",
        ...(alive && department ? { ...(processId ? { processId } : {}), departmentId: department } : {}),
      };
      continue;
    }
    if (!alive) {
      if (transition && transition.status !== "cancelled" && transition.status !== "applied" && transition.status !== "forced") {
        projectionRecovering = true;
        people[personId] = { personId, state: "recovering" };
        continue;
      }
      people[personId] = { personId, state: person.employmentState === "departed" ? "departed" : "parked" };
      continue;
    }
    people[personId] = transition && transition.status !== "cancelled" && transition.status !== "applied" && transition.status !== "forced"
      ? {
        personId,
        state: "handoff-held",
        ...(processId ? { processId } : {}),
        departmentId: department,
        transitionId: transition.id,
        transitionAction: transition.action,
        transitionStatus: transition.status,
      }
      : { personId, state: "running", ...(processId ? { processId } : {}), departmentId: department };
  }
  return {
    organization: manifest.slug,
    status: projectionRecovering ? "recovering" : "running",
    observedAt: raw.observedAt,
    ...(projectedReconciliation ? { reconciliation: projectedReconciliation } : {}),
    people,
    ...(missingProcessPersonIds.length || unexpectedProcessPersonIds.length ? {
      runtimeActivityDivergence: { missingProcessPersonIds, unexpectedProcessPersonIds },
    } : {}),
  };
}

function personRuntimeText(observation: OrganizationRosterPersonObservation | undefined): string {
  if (!observation) return "runtime observation not loaded";
  // chiefd has no pane and no window to name, so neither word appears here any
  // more. What it does have is a pid — sometimes — and the department the
  // CURRENT manifest tree places the person in.
  const process = (): string => observation.processId ? `pid ${observation.processId}` : "pid unknown";
  if (observation.state === "running") return `running · ${process()} · ${observation.departmentId}`;
  if (observation.state === "handoff-held") {
    return `handoff-held ${observation.transitionAction}/${observation.transitionStatus} · ${process()} · current department ${observation.departmentId}`;
  }
  if (observation.state === "starting") return "starting · no process yet";
  if (observation.state === "recovering") return observation.departmentId
    ? `recovering · observed ${process()} · waiting for durable runtime convergence`
    : "recovering · no live process observed";
  if (observation.state === "absent") return "absent · no runtime observation";
  return `${observation.state} · no process`;
}

/** Formats durable authority plus an explicitly loaded runtime observation. */
export function formatOrganizationRoster(
  manifest: IntercomOrganizationManifest,
  observation?: OrganizationRosterObservation,
): string {
  const lines = [`${manifest.name} (${manifest.slug})`];
  lines.push(observation
    ? `Runtime observation: ${observation.status}${observation.reconciliation ? ` · reconciling since ${observation.reconciliation.startedAt}` : ""}${observation.observedAt ? ` · observed ${observation.observedAt}` : ""}`
    : "Runtime observation: not loaded");
  for (const departmentId of manifest.departmentOrder) {
    const department = manifest.departments[departmentId]!;
    const depth = departmentDepth(manifest, departmentId);
    const kind = organizationUnitKind(manifest, department);
    const transient = kind === "contract"
      ? ` · engagement: ${department.transient?.engagement ?? "missing"} · launched ${department.transient?.launchedAt ?? "missing"}${department.transient?.expiresAt ? ` · expires ${department.transient.expiresAt}` : ""}`
      : "";
    lines.push(`${"  ".repeat(depth)}${depth ? "↳ " : ""}${department.name} [${department.id}] · ${kind} · ${department.state}${transient}`);
    for (const personId of manifest.peopleOrder) {
      const person = manifest.people[personId]!;
      const headsHere = department.headPersonId === person.id;
      if (!headsHere && person.departmentId !== departmentId) continue;
      // A departed person carries no authority line: the advice would name a
      // create nobody is there to make.
      const authority = person.employmentState === "departed" ? "" : ` · authority: ${personAuthorityText(manifest, person)}`;
      lines.push(`${"  ".repeat(depth + 1)}${headsHere ? "head" : "worker"}: ${person.name} [${person.id}] · ${person.employmentState} · ${personRuntimeText(observation?.people[person.id])}${authority}`);
    }
  }
  return lines.join("\n");
}

// #751/G9 TOMBSTONE — THE LAUNCHER SUBPROCESS TRANSPORT. Four hundred lines
// stood here and every one of them is deleted. What they were, so the record
// survives the code:
//
//   * `defaultLauncherRunner` — the only `spawn` in this file. It ran
//     `<TEAM_LAUNCHER_BUN> run <launcherRoot>/apps/cli/src/Main.ts <verb>` and
//     read the child's stdout back as the answer. That entry point is deleted,
//     so the call had already stopped serving any verb: every command it sent
//     came back `unknown command`.
//   * `launcherControlPlaneEnvironment` / `launcherWorkerActionEnvironment` —
//     the two environments a child could be given. A control-plane call had the
//     pane's identity STRIPPED so an internal `org reconcile` could not
//     impersonate the worker that asked for it; a fenced worker action had that
//     identity rebuilt from the extension context, never from a tool payload.
//     `ATTESTED_WORKER_ACTION_VERBS` / `isAttestedWorkerActionCommand` was the
//     table that chose between them — a TABLE and not an if/else ladder because
//     the same defect shipped three times as a hand-maintained ladder lagged a
//     new verb (#316, #323, #330).
//   * `launcherRuntimeBinary` — `TEAM_LAUNCHER_BUN` over a bare `"bun"`,
//     because a pane's PATH is a scrubbed standard set and a default bun
//     install (`~/.bun/bin`) is in none of it. Every call from the pane died
//     `spawn bun ENOENT` until this resolved the stamped path.
//   * `LauncherCommandError` / `describeLauncherCommandFailure` and their
//     redaction helpers — #331's structured failure: a complete card line, a
//     bounded credential-redacted stderr tail, and a bounded argv.
//   * `LAUNCHER_COMMAND_TIMEOUT_MS` / `LAUNCHER_COMMAND_OUTPUT_LIMIT` — the
//     SIGKILL deadline and the output ceiling for a wedged child.
//   * `runChecked` — the one function that invoked a runner, and the reason
//     everything above existed. Its last call site left with the session
//     maintenance family; it then survived as an export nothing called.
//   * `waitForLauncherRetry` and the transient-transport ladder inside
//     `runChecked`. The LADDER is gone; the CLASSIFIER is not.
//     `isTransientTransportFailure` and `TRANSIENT_TRANSPORT_RETRY_DELAYS_MS`
//     are alive above, read by `withTransientReadRetryAsync` — the boot retry
//     ladder — which is now their only reader.
//
// #751/G9-S0 is recorded here because its subject was one of the two ladders
// this function held. `isPreMutationLauncherLockBusyDiagnostic` matched eight
// refusal strings by regex against a subprocess's stderr. Every producer of
// those eight strings was already gone — the file-mutex family (`.org.lock` /
// `.runtime.lock`, deleted with `runtime_lifecycle.rs`'s port) and the SQL
// lease family (`runtime_writer_lease`, deleted by #751/P2) minted all of them
// — so the predicate returned false for every input the live tree could
// produce, and the ladder keyed off it never took its retry branch. The
// predicate, the ladder (`LAUNCHER_LOCK_RETRY_BASE_DELAYS_MS`,
// `launcherLockRetryDelayMs`, `CoordinatedLauncherRunner.lockRetryDelayMs`) and
// the two `status: "busy"` tool results it gated were deleted rather than
// rewritten: reconstructing a refusal's meaning by matching text is what
// rotted, and the fix is for the authority that refuses to also say so in its
// typed response. `packages/piing/test/DeadLockBusyVocabulary.test.ts` still
// fails if any of the eight strings comes back to a production throw site
// without a classifier coming back with it.
//
// TMUX_PANE. `launcherWorkerActionEnvironment` was the WRITE half of the pane
// hand-off and spelled the variable tmux's way on purpose. Only the READ half
// survives, in `readOrganizationRuntimeContext`, and its comment there carries
// the whole rule: `TMUX_PANE` is set by TMUX ITSELF inside every pane. It is
// not a name this repo gets to choose and it must never be renamed —
// #751/P9's sweep renamed it to `RUNTIME_PANE` on both sides, which kept the
// file self-consistent while silently deleting the raw-pane tier, because
// nothing in the universe sets `RUNTIME_PANE`.

/**
 * What chiefd answers a runtime convergence with (`RuntimeLaunchReport`). Only
 * the fields this file reads are modelled; the report carries pane/window maps
 * and converge notes an agent has no use for.
 */
export interface ChiefdRuntimeLaunchReport {
  applied?: boolean;
  retryAfterFloor?: boolean;
  monitorWarnings?: string[];
}

// TOMBSTONE: `deferredStarts?: number`. The launch ramp capped how many panes
// could start in one pass and deferred the rest. It is deleted by operator
// ruling ("just boot them all at the same time"), and more fundamentally chiefd
// no longer mints start actions at all -- it publishes a desired SET, and there
// is no such thing as a partial truth about who should be running. The actuator
// boots everything missing at once.

/**
 * The honest, TRUTHFUL-OR-ABSENT warning for one convergence pass.
 *
 * Only chiefd's OWN warnings survive here. Everything else this helper used to
 * say described chiefd's internal machinery rather than the caller's change:
 *
 * * The tmux-identity warning ("Runtime projection is not configured") was
 *   about the CALLER, not the company, and chiefd derives the session itself.
 * * The launch-ramp sentence named a ramp that no longer exists.
 * * The `applied !== true` sentence explained chiefd's single-flight window to
 *   somebody who did not ask and cannot act on it. The durable write committed
 *   and the runtime comes up; that a given pass was skipped or coalesced is
 *   chiefd's business. It appeared on EVERY department a manager created, which
 *   made a routine success read as a caveat.
 *
 * A card that says nothing is better than a card that invents a caveat.
 */
export function runtimeConvergenceWarning(report: ChiefdRuntimeLaunchReport): string | undefined {
  const notices = (report.monitorWarnings ?? []).filter((line) => typeof line === "string" && line.trim());
  return notices.length ? notices.join(" ") : undefined;
}

/**
 * How a POST-COMMIT convergence failure is worded.
 *
 * {@link reconcileRuntime} runs after the durable write has already committed,
 * so a thrown transport/route failure there is NOT a failed operation — it is a
 * successful operation whose runtime has not converged yet. The sentence has to
 * carry three facts, because the manager's next decision depends on all three:
 * the change is durable, the pane is not up yet, and RETRYING THE WRITE IS
 * WRONG (the retry is what earns `a department with this id already exists`).
 *
 * It does NOT name the reconciler, the pass, or anything else about how chiefd
 * gets there: the caller acts on "durable, not up yet, do not retry" and can
 * act on nothing else here.
 */
function postCommitConvergenceWarning(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return `The change is durable and must not be retried. Bringing the runtime up did not complete (${compactPresentation(message, 160).text}); the panes come up without another call from you.`;
}

/**
 * Converge this company's runtime through chiefd's own route (#751/P4).
 *
 * This spawned `org reconcile <slug> --socket --session [--request-person …]`
 * at a CLI that no longer exists, so EVERY tool that mutates structure and
 * then brings the panes up — department create, unit resume/stop/remove,
 * staffing changes, mailbox wakes — failed AFTER its durable write committed,
 * with `chiefd: unknown command 'org'`. The route answered 200 and the tool
 * still reported a system fault, which is the worst possible shape: the change
 * happened and the agent was told it did not.
 *
 * **The route is `/v1/org/runtime/launch`, not `/v1/org/projection/reconcile`.**
 * The port plan's route table named the latter by name-matching, and it is
 * wrong. The last live CLI is dispositive: `org reconcile` called
 * `runtime.launch({slug, actor, requestedPersonIds})`. The difference is not
 * cosmetic — `projection/reconcile` runs a converge pass that ignores requested
 * people entirely, so a department create would converge the company WITHOUT
 * opening the launch fence for the people it had just created, and the new
 * panes would never come up. `requestedPersonIds` is a HINT that chiefd
 * evaluates per person against a genuine pending envelope; that decision is
 * chiefd's and is not reproduced here.
 *
 * `--socket`/`--session` are gone, and so is the guard that returned a warning
 * when this pane had none: chiefd derives the session itself, so runtime identity
 * was never this call's business. `actor` is the audit attribution chiefd
 * records for the launch, never an authorization — the route's own gates decide
 * that.
 */
async function reconcileRuntime(
  context: OrganizationRuntimeContext,
  requestedPersonIds: readonly string[] = [],
): Promise<string | undefined> {
  const recipients = [...new Set(requestedPersonIds.map((personId) => personId.trim()).filter(Boolean))];
  const report = await chiefdPostJson<ChiefdRuntimeLaunchReport>(
    chiefdEndpoint(context),
    "/v1/org/runtime/launch",
    {
      slug: companyKeyOf(context),
      actor: context.personId,
      ...(recipients.length ? { requestedPersonIds: recipients } : {}),
    },
  );
  return runtimeConvergenceWarning(report);
}

function encode(value: unknown): string {
  return Buffer.from(JSON.stringify(value), "utf8").toString("base64url");
}

/**
 * A RECEIPT MUST NEVER BE MANUFACTURED.
 *
 * The deeper half of the model-switch defect was not which branch the receipt
 * took — it was that BOTH branches were written unconditionally, so a confident
 * "recorded for @x" was emitted with nothing proven to be behind it. The
 * deferred branch is the dangerous one: it tells an operator the selection is
 * safely held for the next boot, which is a claim about DURABLE STATE and is
 * worthless unless that state exists.
 *
 * So every session-maintenance receipt is derived from the durable records the
 * queue actually returned: one record per target, minted for THAT person and
 * THAT action, and not already terminal. Anything else throws instead of
 * reporting success — a legible failure the operator can retry beats a success
 * message for work that never happened.
 */
export function assertDurableMaintenanceRecords(
  requests: readonly SessionMaintenanceRequest[],
  targets: readonly string[],
  action: SessionMaintenanceAction,
): void {
  if (requests.length !== targets.length) {
    throw new Error(`Session maintenance was not recorded for every target (${requests.length} of ${targets.length} durable records); nothing is reported as queued`);
  }
  for (const [index, request] of requests.entries()) {
    const personId = targets[index]!;
    if (!request || typeof request.id !== "string" || !request.id.trim()) {
      throw new Error(`Session maintenance for '${personId}' returned no durable record; the selection was NOT stored and must not be reported as queued`);
    }
    if (request.personId !== personId || request.action !== action) {
      throw new Error(`Session maintenance record '${request.id}' is for '${request.personId}'/'${request.action}', not '${personId}'/'${action}'`);
    }
    if (request.status === "completed" || request.status === "failed" || request.status === "skipped") {
      throw new Error(`Session maintenance record '${request.id}' for '${personId}' is already ${request.status}; it was not queued`);
    }
  }
}


/** Byte-identical to `ROOT_EXECUTIVE_ESCALATION_KIND` in
 * `src/organization/org-operator-escalation.ts`; part of the fingerprint
 * canonical, so it must match. Change the two together. */
const ROOT_EXECUTIVE_ESCALATION_KIND = "root_executive_blocked";

interface OperatorEscalationIntentRecord {
  schemaVersion: 1;
  fingerprint: string;
  organization: string;
  personId: string;
  blocker: string;
  operatorAction: string;
  queuedAt: string;
}

/** Byte-identical to `normalizeOperatorEscalationBlocker` /
 * `operatorEscalationFingerprint` in `src/organization/org-operator-escalation.ts`.
 * The drain recomputes and re-validates this key, so any drift makes every
 * escalation this extension publishes fail validation. Change the two together. */
function operatorEscalationFingerprint(personId: string, blocker: string): string {
  const subject = `person:${personId}`;
  const canonical = `${ROOT_EXECUTIVE_ESCALATION_KIND}\x00${subject}\x00${blocker.trim().replace(/\s+/g, " ").toLowerCase()}`;
  return `${createHash("sha256").update(canonical).digest("hex").slice(0, 24)}`;
}

/**
 * Insert a structural root executive's operator escalation directly on its
 * deterministic fingerprint. A matching replay is harmless; a different
 * payload under that fingerprint is a durable conflict and must not overwrite
 * the original intent. No caller CAS/retry loop reconstructs the queue.
 */
async function queueOperatorEscalationIntent(
  context: OrganizationRuntimeContext,
  personId: string,
  blocker: string,
  operatorAction: string,
): Promise<OperatorEscalationIntentRecord> {
  const fingerprint = operatorEscalationFingerprint(personId, blocker);
  const intent: OperatorEscalationIntentRecord = {
    schemaVersion: 1,
    fingerprint,
    organization: context.organization,
    personId,
    blocker,
    operatorAction,
    queuedAt: new Date().toISOString(),
  };
  const endpoint = chiefdEndpoint(context);
  const outcome = await new RowStoresClient(chiefdTransport(endpoint), endpoint.url).insertOperatorEscalationIntent(
    companyKeyOf(context),
    intent,
  );
  if (outcome.status === "conflict") {
    throw new Error(`Operator escalation '${fingerprint}' conflicts with an existing durable intent`);
  }
  return intent;
}

/**
 * Every chiefd route the ACTIVITY family reads.
 *
 * A closed union for the same reason `StaffingRoutePath`,
 * `SupervisionRoutePath` and `ReminderRoutePath` are: a verb
 * reaches a path by being named here or not at all, so no route can arrive by
 * concatenation and the seam classifier's literal inventory stays the real one.
 *
 * `org activity reflect` shared this helper until #751/P4 deleted the
 * reflection concept from the product.
 *
 * TOMBSTONE: `status` -> `/v1/org/activity/command-status` stood here with a
 * single reader, {@link queueAutomaticParkCompaction}, which asked it whether a
 * park was already pending before spending a compact. A routine idle park is
 * born TERMINAL and never appears in `pendingTransitions`, so that read could
 * only ever answer no. The gate is deleted, the read with it, and this family
 * is down to one verb.
 *
 * `agentState` is the settle countdown's idleness beat, written by
 * {@link noteAgentActivityBeat}.
 */
type ActivityRoutePath = "/v1/org/activity/agent-state";

const ACTIVITY_ROUTES: Record<"agentState", ActivityRoutePath> = {
  agentState: "/v1/org/activity/agent-state",
};

/**
 * How long one activity beat covers, so a busy turn does not post one write per
 * streamed chunk.
 *
 * The events below (turn start, message start/update/end, tool execution
 * start/update/end) fire many times a second while a model streams. chiefd
 * trusts a beat for `AGENT_ACTIVITY_LIVENESS_MS` = 300_000 ms
 * (`chiefd-core store/activity.rs`), so:
 *   300_000 ms / 30_000 ms = 10 beats of headroom before a working agent is
 *   misread as quiet.
 * An idle pane posts NOTHING on this path at all -- the beat is event-driven,
 * there is no timer, and a settled agent has already sent its single
 * `working:false` beat.
 */
const ORGANIZATION_AGENT_ACTIVITY_BEAT_INTERVAL_MS = 30_000;

/**
 * #434 follow-up — THE PANE MUST NOT ANSWER FOR CODE IT IS NOT RUNNING.
 *
 * A Pi process loads its extensions once at session start and keeps them until
 * it restarts. A deploy only rewrites files, so a pane that was already up goes
 * on executing the module it loaded — while the operator, reasonably, believes
 * the deployed fix is what answered him.
 *
 * That is not hypothetical. On 2026-07-24 an operator was told twice that the
 * model switch was fixed, then got the receipt `applies at the next claim cycle
 * · no settled-work wait` — a string that existed nowhere in the deployed code.
 * The CEO pane's installed `organization-intercom` was the FIXED file, written
 * 16:36:04; the pi process that loaded it started 15:48:21. The file it was
 * executing had been replaced 48 minutes after the process began. The receipt
 * was confident, and **nothing durable was written behind it**.
 *
 * `src/organization/org-extension-runtime-drift.ts` already detects this
 * correctly, from the outside, and is worth keeping. What it cannot do is stop
 * a stale pane mid-conversation: it is an on-demand CLI report, plus one
 * reconcile call site that USED to sit inside `if (!options.materializationReady)`
 * — false on the ordinary post-deploy path, so the check was skipped exactly
 * when a deploy just happened. Detection existed; nothing consulted it at the
 * moment that lied. (That conditional is gone as of A6 — the field was deleted
 * because skipping the repair also skips enrolling a company's people — so the
 * reconcile is unconditional now. The argument below is unaffected: it is about
 * a pane that is already running.)
 *
 * So the pane carries its own answer. `EXTENSION_LOADED_MTIME_MS` is stamped
 * when this module is evaluated, i.e. genuinely at load; comparing it against
 * the file's current mtime asks the only question that matters — "is the code
 * speaking still the code on disk?"
 *
 * COST: one `statSync` on a mutating maintenance queue, an operator-initiated
 * action. No timer, no watcher, nothing on the idle path — an idle pane pays
 * exactly zero, per the reactive/idle-to-zero rule.
 *
 * FAILS OPEN, DELIBERATELY. If the path or either stat cannot be read we return
 * "not stale", matching `processStartedAtMs`'s own contract that "cannot prove
 * drift" means "no drift". A guard that blocks real operator work on a bad read
 * gets switched off, and then it protects nobody.
 *
 * WHY THIS CANNOT ACCUSE HEALTHY CODE — the two exposures that look identical
 * in a table are separated by construction, because the comparison is
 * per-process ("the mtime *I* loaded" vs "the mtime on disk *now*"):
 *   - long-lived pi pane, extensions reinstalled after it started → mtime moved
 *     past the load stamp → STALE, refuses;
 *   - the launcher CLI, a fresh process per invocation → it loads whatever is
 *     current, stamp equals disk → silent;
 *   - a Rust/chiefd-only deploy, which reinstalls no extensions → mtime never
 *     moved → silent.
 */
const EXTENSION_SELF_PATH: string | undefined = (() => {
  try { return fileURLToPath(import.meta.url); } catch { return undefined; }
})();

const EXTENSION_LOADED_MTIME_MS: number | undefined = (() => {
  try { return EXTENSION_SELF_PATH ? statSync(EXTENSION_SELF_PATH).mtimeMs : undefined; } catch { return undefined; }
})();

export interface ExtensionStaleness {
  stale: boolean;
  loadedMtimeMs?: number;
  currentMtimeMs?: number;
}

/** Pure comparison, so the decision is unit-testable without touching a disk. */
export function extensionStalenessOf(loadedMtimeMs?: number, currentMtimeMs?: number): ExtensionStaleness {
  if (!Number.isFinite(loadedMtimeMs) || !Number.isFinite(currentMtimeMs)) return { stale: false, loadedMtimeMs, currentMtimeMs };
  return { stale: currentMtimeMs! > loadedMtimeMs!, loadedMtimeMs, currentMtimeMs };
}

/** Is the module answering right now older than the file it was loaded from? */
export function runningExtensionStaleness(
  selfPath = EXTENSION_SELF_PATH,
  loadedMtimeMs = EXTENSION_LOADED_MTIME_MS,
): ExtensionStaleness {
  let currentMtimeMs: number | undefined;
  try { currentMtimeMs = selfPath ? statSync(selfPath).mtimeMs : undefined; } catch { currentMtimeMs = undefined; }
  return extensionStalenessOf(loadedMtimeMs, currentMtimeMs);
}

/**
 * Refuse rather than answer for replaced code.
 *
 * The message names BOTH timestamps, because "restart the pane" without
 * evidence is indistinguishable from a spurious failure and the caller has no
 * way to check it.
 *
 * # It shouts about WHOSE session, because the first version did not
 *
 * It said "This pane ... Restart this person". Both phrases are read against
 * the request, and a session-maintenance request NAMES SOMEBODY ELSE — so a CEO
 * asking to set `maya-head`'s thinking effort read it as "restart maya-head",
 * restarted her, got the identical refusal, and looped. Observed live:
 * "Actually perhaps the message refers to the CEO's own pane, not maya.
 * Unlikely." The agent reasoned its way to the right answer and then talked
 * itself out of it, because the sentence supported the wrong reading.
 *
 * The staleness is a property of the CALLER's own loaded module and of nothing
 * else. This check cannot see the target's session at all, so the wording now
 * says that outright and names the recovery on the caller's own id.
 */
export function assertRunningExtensionIsCurrent(staleness = runningExtensionStaleness()): void {
  if (!staleness.stale) return;
  const iso = (ms?: number) => (Number.isFinite(ms) ? new Date(ms!).toISOString() : "unknown");
  throw new Error(
    "YOUR OWN session is running organization extension code that has since been replaced on disk, so it "
    + "cannot honour this request — "
    + `loaded ${iso(staleness.loadedMtimeMs)}, installed ${iso(staleness.currentMtimeMs)}. `
    + "Nothing was queued. THIS IS NOT ABOUT THE PERSON YOU NAMED: their session is fine and restarting "
    + "them changes nothing. Your OWN pane has to be restarted to pick up the deployed code — ask "
    + "the operator to stop and start it — then reissue the change.",
  );
}

/**
 * Every chiefd route the SESSION-MAINTENANCE family posts to (#751/P4).
 *
 * A closed union for the same reason `StaffingRoutePath`,
 * `SupervisionRoutePath` and `ReminderRoutePath` are: a verb
 * reaches a path by being named here or not at all, so no route can arrive by
 * concatenation and the seam classifier's literal inventory stays the real one.
 *
 * Ten literals for eleven verbs. `auto-compact` is `queue` with the action and
 * the automatic flag fixed, exactly as the deleted CLI had it, so the two
 * share `/v1/org/session-maintenance/queue`.
 */
type SessionMaintenanceRoutePath =
  | "/v1/org/session-maintenance/queue"
  | "/v1/org/session-maintenance/start"
  | "/v1/org/session-maintenance/defer"
  | "/v1/org/session-maintenance/interrupt"
  | "/v1/org/session-maintenance/recover"
  | "/v1/org/session-maintenance/finish"
  | "/v1/org/fresh-session/apply"
  | "/v1/org/fresh-session/complete"
  // TOMBSTONE: `/v1/org/session-maintenance/complete-native` and
  // `/v1/org/company-session-action/skip-parked`, both deleted server-side in
  // the same change that deleted `org_maintain_session`.
  ;

type SessionMaintenanceVerb =
  | "queue" | "auto-compact" | "interrupt" | "start" | "defer" | "recover"
  // TOMBSTONE: `reconcile-parked` and `complete-native`, whose routes are
  // deleted with the company-session-action family.
  | "finish" | "apply" | "complete";

const SESSION_MAINTENANCE_ROUTES: Record<SessionMaintenanceVerb, SessionMaintenanceRoutePath> = {
  queue: "/v1/org/session-maintenance/queue",
  "auto-compact": "/v1/org/session-maintenance/queue",
  start: "/v1/org/session-maintenance/start",
  defer: "/v1/org/session-maintenance/defer",
  interrupt: "/v1/org/session-maintenance/interrupt",
  recover: "/v1/org/session-maintenance/recover",
  finish: "/v1/org/session-maintenance/finish",
  apply: "/v1/org/fresh-session/apply",
  complete: "/v1/org/fresh-session/complete",
};

/**
 * The exact live claim, as chiefd's routes model it.
 *
 * Every call site in this file carries the triple FLAT
 * (`processId`/`sessionId`/`claimToken`); chiefd wants it NESTED, under
 * `claim`, `sourceClaim` or `targetClaim` depending on the verb. That
 * reshaping is transport, so it lives here rather than at thirty call sites.
 */
function maintenanceClaimOf(payload: Record<string, unknown>, verb: SessionMaintenanceVerb): {
  processId: number; sessionId: string; claimToken: string;
} {
  const processId = payload.processId;
  const sessionId = payload.sessionId;
  const claimToken = payload.claimToken;
  if (typeof processId !== "number" || !Number.isFinite(processId)
    || typeof sessionId !== "string" || !sessionId.trim()
    || typeof claimToken !== "string" || !claimToken.trim()) {
    throw new Error(`Session maintenance '${verb}' requires the exact live claim (processId, sessionId, claimToken)`);
  }
  return { processId, sessionId, claimToken };
}

/** Copy only the named keys a route accepts. Every request struct in this
 *  family is `deny_unknown_fields`, so an extra key is a 4xx, not a no-op. */
function pick(payload: Record<string, unknown>, keys: readonly string[]): Record<string, unknown> {
  const picked: Record<string, unknown> = {};
  for (const key of keys) if (payload[key] !== undefined) picked[key] = payload[key];
  return picked;
}

/**
 * The eleven durable session-maintenance verbs, in process.
 *
 * This replaced `spawn`ing `apps/cli/src/Main.ts org session-maintenance
 * <verb>` with `--socket`/`--session` appended. Those flags were how the CLI
 * reached `authenticatedActivityIdentity`, which derived the acting person and
 * identity from the pane's launcher-injected environment — and THAT is
 * what six of these routes require and no call site in this file carries.
 *
 * **The acting identity is supplied here, and it is spread LAST.** `start`,
 * `defer`, `interrupt`, `complete-native`, `recover` and `finish` each take
 * `identity: {personId}` as a required non-`Option` nested struct. Spreading
 * identity after the payload is deliberate: a claim, a completion and a
 * deferral are the RUNNING PERSON's own, so a payload must be structurally
 * unable to nominate someone else.
 *
 * `queue` is the one verb whose `personId` is genuinely a payload field — a
 * manager queues maintenance FOR a target — so it is projected from the
 * payload and not overwritten.
 *
 * `slug` is the COMPOSITE key through {@link companyKeyOf}: chiefd resolves
 * every one of these routes by `req.slug == org_documents_slug`, and a bare
 * slug matches no live company and 404s silently.
 */
async function sessionMaintenanceCommand(
  context: OrganizationRuntimeContext,
  action: SessionMaintenanceVerb,
  payload: Record<string, unknown>,
): Promise<any> {
  // Before ANY durable write: a stale pane must fail loudly instead of
  // returning a confident receipt with nothing behind it. Guarded on `queue`
  // only — the claim/finish verbs are driven by the target's own live runtime
  // and blocking those would strand in-flight maintenance rather than protect
  // anyone.
  if (action === "queue") assertRunningExtensionIsCurrent();
  const endpoint = chiefdEndpoint(context);
  const slug = companyKeyOf(context);
  const identity = { personId: context.personId };
  const body = ((): Record<string, unknown> => {
    switch (action) {
      case "queue":
        return {
          // `thinkingLevel`, `model` and `modelProvider` were picked here and are
          // gone with `set_model`. The route declares `deny_unknown_fields`, so
          // leaving a dead key in this list is not inert — a payload that carried
          // one would 400 rather than be ignored.
          ...pick(payload, ["action", "personId", "requestedBy", "reason", "automatic", "force"]),
        };
      case "auto-compact":
        // The self-compaction before a settled park. The CLI fixed `action`
        // and `automatic` here; `personId`/`requestedBy` are this person's
        // own and chiefd requires both as non-`Option` fields — the sole call
        // site sends neither, so this verb refused before it ever reached the
        // queue.
        return {
          ...pick(payload, ["reason"]),
          action: "compact",
          automatic: true,
          personId: context.personId,
          requestedBy: context.personId,
        };
      case "start":
        return {
          ...pick(payload, ["requestId", "compactSessionId", "compactAnchorEntryId"]),
          claim: maintenanceClaimOf(payload, action),
          // Required non-`Option` on the route and absent from the call
          // site's payload; the claimed request's own action is what the
          // caller already resolved before claiming it.
          action: payload.action,
          identity,
        };
      case "defer":
      case "interrupt":
        return {
          ...pick(payload, ["requestId"]),
          claim: maintenanceClaimOf(payload, action),
          identity,
        };
      case "recover":
        return {
          claim: maintenanceClaimOf(payload, action),
          identity,
        };
      case "finish":
        return {
          ...pick(payload, ["requestId", "status", "error", "compactEntryId"]),
          identity,
        };
      case "apply":
        return {
          ...pick(payload, ["requestId"]),
          sourceClaim: maintenanceClaimOf(payload, action),
          personId: context.personId,
        };
      case "complete":
        return {
          ...pick(payload, ["requestId"]),
          targetClaim: maintenanceClaimOf(payload, action),
          personId: context.personId,
        };
    }
  })();
  const wire = await chiefdPostJson<Record<string, unknown>>(endpoint, SESSION_MAINTENANCE_ROUTES[action], { slug, ...body });
  // `start` is the one verb whose route wraps its answer: `{request: … | null}`,
  // where `null` is the ordinary "nothing to claim" and never an error. Every
  // caller here reads the request itself, exactly as the deleted CLI's
  // `startMaintenance` unwrapped it before printing.
  return action === "start" ? (wire?.request ?? undefined) : wire;
}

// TOMBSTONE: the reload-hard-contract cluster — `RELOAD_HARD_CONTRACT_ENV`,
// `RELOAD_HARD_CONTRACT_FILE`, the `ReloadHardContract` type, its parser,
// `reloadNeedsFreshSession`, `currentReloadHardContract` and
// `launchedReloadHardContract`.
//
// It let a running agent notice, on an in-process `/reload`, that its immutable
// tool grant or its native auth digest no longer matched what it launched with,
// and queue a successor process for itself.
//
// IT HAD ALREADY STOPPED FIRING BEFORE THIS BRANCH TOUCHED IT, and that is the
// load-bearing fact rather than the deleted action. Both PRODUCERS are gone
// from chiefd-host — `cycle.rs` and `api_host_profile.rs` each carry a
// tombstone for `ORG_LAUNCHER_RELOAD_HARD_CONTRACT`, "a re-projection's
// receipt, and nothing re-projects" — so `launchedReloadHardContract()` parsed
// an unset env var and `currentReloadHardContract()` read a file nobody writes.
// Both returned `undefined`, and the predicate's first line returns `false` on
// either. The queue call below it was UNREACHABLE, not refused.
//
// So no coverage is lost here by the `fresh_session` deletion. What the two
// halves of the predicate were worth, for whoever revives the receipt:
//   * The TOOL half is owned by a surviving mechanism. `tools` is an input to
//     `launch_command_fingerprint`, so a changed grant moves
//     `desired_launch_hash`, the pane's tag stops matching, and the actuator
//     replaces the process on the next converge pass. Less graceful than an
//     in-session successor, same guarantee.
//   * The NATIVE-AUTH half is owned by NOTHING. `desired_launch_hash` takes
//     `organization`, `person_id`, `launch_command` and `extension_digest`, and
//     no auth provider or digest reaches any of them — verified against
//     `runtime/launch_hash.rs`. A credential rotation for the SAME provider
//     moves no hash, so a pane keeps running on the credential it launched
//     with. That gap is not opened here; it opened when the receipt did.

function projectedSessionMaintenanceRequest(value: unknown): SessionMaintenanceRequest | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const request = value as Partial<SessionMaintenanceRequest>;
  if (typeof request.id !== "string" || !request.id
    || request.action !== "compact"
    || typeof request.personId !== "string" || !request.personId
    || typeof request.requestedBy !== "string" || !request.requestedBy
    || typeof request.reason !== "string" || !request.reason.trim()
    || typeof request.automatic !== "boolean"
    || typeof request.requestedAt !== "string" || !Number.isFinite(Date.parse(request.requestedAt))
    || !(<readonly unknown[]>["queued", "running", "applying", "completed", "failed", "skipped"]).includes(request.status)) return undefined;
  const claimMissing = [request.claimedProcessId, request.claimedSessionId, request.claimToken].map((item) => item === undefined);
  const completionMissing = [request.completedProcessId, request.completedSessionId, request.completionClaimToken].map((item) => item === undefined);
  const interruptMissing = [request.interruptedProcessId, request.interruptedSessionId, request.interruptedClaimToken, request.interruptedAt]
    .map((item) => item === undefined);
  const compactAnchorMissing = [request.compactSessionId, request.compactAnchorEntryId].map((item) => item === undefined);
  if (new Set(claimMissing).size !== 1 || new Set(completionMissing).size !== 1
    || new Set(interruptMissing).size !== 1
    || new Set(compactAnchorMissing).size !== 1
    || (request.claimedProcessId !== undefined && (!Number.isSafeInteger(request.claimedProcessId) || request.claimedProcessId < 1))
    || (request.claimedSessionId !== undefined && (typeof request.claimedSessionId !== "string" || !request.claimedSessionId.trim()))
    || (request.claimToken !== undefined && (typeof request.claimToken !== "string" || !request.claimToken.trim()))
    || (request.completedProcessId !== undefined && (!Number.isSafeInteger(request.completedProcessId) || request.completedProcessId < 1))
    || (request.completedSessionId !== undefined && (typeof request.completedSessionId !== "string" || !request.completedSessionId.trim()))
    || (request.completionClaimToken !== undefined && (typeof request.completionClaimToken !== "string" || !request.completionClaimToken.trim()))
    || (request.companyActionId !== undefined && (typeof request.companyActionId !== "string" || !request.companyActionId.trim()))
    // #319: force is no longer exclusive to company actions — a
    // single-target request may carry it too (`org control compact
    // --interrupt`). A company action still must carry a boolean force.
    || (request.force !== undefined && typeof request.force !== "boolean")
    || (request.companyActionId !== undefined && request.force === undefined)
    || (request.retryNotBefore !== undefined && (request.companyActionId === undefined || request.status !== "queued"
      || typeof request.retryNotBefore !== "string" || !Number.isFinite(Date.parse(request.retryNotBefore))))
    || (request.interruptedProcessId !== undefined && (!Number.isSafeInteger(request.interruptedProcessId) || request.interruptedProcessId < 1))
    || (request.interruptedSessionId !== undefined && (typeof request.interruptedSessionId !== "string" || !request.interruptedSessionId.trim()))
    || (request.interruptedClaimToken !== undefined && (typeof request.interruptedClaimToken !== "string" || !request.interruptedClaimToken.trim()))
    || (request.interruptedAt !== undefined && (typeof request.interruptedAt !== "string" || !Number.isFinite(Date.parse(request.interruptedAt))))
    || (request.compactSessionId !== undefined && (request.action !== "compact" || typeof request.compactSessionId !== "string" || !request.compactSessionId.trim()))
    || (request.compactAnchorEntryId !== undefined && (request.action !== "compact" || typeof request.compactAnchorEntryId !== "string" || !request.compactAnchorEntryId.trim()))
    || (request.completedCompactionEntryId !== undefined
      && (request.action !== "compact" || request.status !== "completed" || typeof request.completedCompactionEntryId !== "string" || !request.completedCompactionEntryId.trim()))
    || ((request.status === "completed" || request.status === "failed" || request.status === "skipped")
      && (typeof request.completedAt !== "string" || !Number.isFinite(Date.parse(request.completedAt))))
    || (request.completedProcessId !== undefined && request.status !== "completed")) return undefined;
  return request as SessionMaintenanceRequest;
}

/**
 * Read the durable ledger directly before crossing the CLI process boundary.
 * The launcher command remains authoritative and revalidates the complete
 * ledger under its crash-safe lock; this projection exists only to make the
 * overwhelmingly common no-work poll one tiny local read instead of a Bun
 * process spawn. A running/applying request blocks another claim.
 */
export async function projectSessionMaintenanceForRuntime(context: OrganizationRuntimeContext): Promise<SessionMaintenanceProjection> {
  const empty = (): SessionMaintenanceProjection => ({ running: [] });
  const invalid = (
    cause: SessionMaintenanceUnresolvableCause,
    detail: string,
  ): SessionMaintenanceProjection => ({ running: [], blockingCompanyActionId: "unknown", unresolvable: { cause, detail } });
  // SQL-only. Absence (the read returns undefined) is a legitimate empty
  // ledger — a company that has never had maintenance — and returns `empty()`.
  // A TRANSPORT failure (service down / non-2xx) is NOT ledger corruption: it
  // is the service being unreachable, the exact fault #608 refused to
  // misattribute to a well-formed ledger. So a transport-level throw is named
  // `ledger_unreachable` (the store did not answer — transient, clears when it
  // returns), while `ledger` is reserved for a document that WAS fetched and
  // is genuinely corrupt or unprojectable. Fail-closed is unchanged either
  // way: mail is withheld, because draining during an unobserved fleet reset
  // is the worse failure.
  const ledgerRef = `sql:${context.organization}/session-maintenance`;
  const badLedger = (why: string) => invalid("ledger", `Session-maintenance ledger '${ledgerRef}' ${why}.`);
  try {
    let ledgerDocument: unknown;
    try {
      ledgerDocument = await readDurableDocument(context, "session-maintenance");
    } catch (error) {
      const message = safeExceptionMessage(error);
      // Transport failure — the store did not answer, never ledger corruption.
      // Structural check (E4-S8): `error instanceof ChiefdUnavailableError ||
      // error instanceof OrgRowRefusalError`, never a message regex.
      if (error instanceof ChiefdUnavailableError || error instanceof OrgRowRefusalError) {
        return invalid(
          "ledger_unreachable",
          `Session-maintenance authority for '${context.organization}' could not be read: ${message}. `
            + "Mail is withheld until the docstore answers; the ledger is not implicated.",
        );
      }
      return badLedger(`could not be read: ${message}`);
    }
    if (ledgerDocument === undefined) return empty();
    const ledger = ledgerDocument as { schemaVersion?: unknown; organization?: unknown; requestOrder?: unknown; requests?: unknown };
    // Rows are keyed by the composite durable-document identity
    // (`<company>@<root digest>`), while the manifest and every Pi-side policy
    // correctly use the bare company slug. The normalized row reader derives
    // that composite value into `organization`; accepting it here as though it
    // were a user-authored ledger value made a perfectly valid maintenance
    // ledger look corrupt and withheld every mailbox in the company. The
    // route already scoped this read to this exact composite key, so project it
    // back to the manifest identity before validating the envelope.
    const storageIdentity = companyKeyOf(context);
    if (ledger.organization === storageIdentity) ledger.organization = context.organization;
    if (ledger.schemaVersion !== 1 || ledger.organization !== context.organization
      || !Array.isArray(ledger.requestOrder)
      || !ledger.requests || typeof ledger.requests !== "object" || Array.isArray(ledger.requests)) return badLedger("has an unusable envelope (schemaVersion, organization, requestOrder or requests)");
    const requestOrder = ledger.requestOrder as unknown[];
    const requests = ledger.requests as Record<string, unknown>;
    if (new Set(requestOrder).size !== requestOrder.length
      || requestOrder.length !== Object.keys(requests).length) return badLedger("has a requestOrder that does not match its requests one-for-one");
    const ordered = requestOrder.map((id) => typeof id === "string" ? projectedSessionMaintenanceRequest(requests[id]) : undefined);
    if (ordered.some((request, index) => !request || request.id !== requestOrder[index])) return badLedger("contains a request row that cannot be projected");
    const blockingCompanyActionId = [...ordered].reverse().find((request) => request?.companyActionId
      && (request.status === "queued" || request.status === "running" || request.status === "applying"))?.companyActionId;
    const exact = (ordered as SessionMaintenanceRequest[])
      .filter((request) => request.personId === context.personId);
    const running = exact.filter((request) => request.status === "running");
    const applying = [...exact].reverse().find((request) => request.status === "applying");
    const queued = running.length || applying
      ? undefined
      : [...exact].reverse().find((request) => request.status === "queued");
    const failed = [...exact].reverse().find((request) => request.status === "failed"
      && request.companyActionId !== undefined);
    return { queued, running, applying, failed, ...(blockingCompanyActionId ? { blockingCompanyActionId } : {}) };
  } catch (error) {
    // Reaching here means the LEDGER failed. The failure
    // is either a durable corruption of the projected document or the write
    // service being unreadable (down / non-2xx). In every case fail closed
    // here: the passive health path owns diagnostics, while a Pi poll must not
    // spawn or flood.
    return badLedger(`could not be read: ${safeExceptionMessage(error)}`);
  }
}

function sessionMaintenanceClaim(extensionContext: ExtensionContext | undefined, claimToken: string): SessionMaintenanceClaim | undefined {
  const sessionId = sessionManagerOf(extensionContext)?.getSessionId?.();
  return typeof sessionId === "string" && sessionId.trim()
    ? { processId: process.pid, sessionId: sessionId.trim(), claimToken }
    : undefined;
}

const defaultScheduler: OrganizationIntercomScheduler = {
  setInterval(callback, intervalMs) {
    return globalThis.setInterval(callback, intervalMs) as unknown as OrganizationIntercomInterval;
  },
  clearInterval(interval) {
    globalThis.clearInterval(interval as unknown as ReturnType<typeof setInterval>);
  },
};

/**
 * Capture direct human work before the model can decide whether to comply.
 * Launcher-generated custom cards never qualify, so assignments, recovery,
 * goal-watch mail, and separately durable gateway traffic stay excluded.
 */
function directHumanRequest(message: unknown): { request: string; sourceId?: string } | undefined {
  if (!message || typeof message !== "object" || Array.isArray(message)) return undefined;
  const candidate = message as Record<string, unknown>;
  if (candidate.role !== "user" || typeof candidate.customType === "string") return undefined;
  const content = typeof candidate.content === "string" ? candidate.content.trim() : "";
  if (!content) return undefined;
  const id = typeof candidate.id === "string" && candidate.id.trim() ? candidate.id.trim() : undefined;
  return { request: content, ...(id ? { sourceId: id } : {}) };
}


/**
 * What is LEFT of `runLifecycle` once the subprocess underneath it is deleted
 * (#751/P3).
 *
 * `runLifecycle` spawned `apps/cli/src/Main.ts` for eleven staffing and
 * structure verbs chiefd already owns; that CLI is gone, so every one of them
 * was dead in a CEO's hands. What genuinely belonged to the intercom is only
 * this: the manager authority check, the company identity its chiefd call
 * needs, and the durable org-event record. The mutation is chiefd's, and so is
 * every placement decision — which is why nothing here names a runtime socket or
 * session, and why no ported verb runs a client-side reconcile. Each of these
 * routes wakes chiefd's own reconcile loop on its success path; a client
 * converging runtime alongside it would be doing chiefd's job and racing it.
 */
interface StaffingAuthority {
  manifest: IntercomOrganizationManifest;
  /** The manager making the change, already proven to be one. */
  person: PersonRecord;
  /** The exact typed-route identity for this company/root pair. */
  slug: string;
  endpoint: ChiefdEndpoint;
  record: (fields: Record<string, unknown>) => void;
}

async function staffingAuthority(context: OrganizationRuntimeContext): Promise<StaffingAuthority> {
  const wire = await readIntercomManifestWire(context);
  const person = currentPerson(context, wire.manifest);
  // No role gate: authority is the subtree, not the job title. Every call site
  // below checks SCOPE (`departmentIsInScope` / `personIsInScope`), which is the
  // real restriction — act at or under your own node, never above it.
  return {
    manifest: wire.manifest,
    person,
    slug: wire.key,
    endpoint: chiefdEndpoint(context),
    record: (fields) => appendOrganizationEvent(context, {
      event: "management-command",
      personId: person.id,
      at: new Date().toISOString(),
      ...fields,
    }),
  };
}

/**
 * Post one staffing/structure mutation and record it only if it committed.
 *
 * The org event is written on the applied branch alone: a refusal changed
 * nothing, and an audit line for a change that did not happen is worse than no
 * line at all.
 */
async function staffingApply(
  gate: StaffingAuthority,
  path: StaffingRoutePath,
  body: Record<string, unknown>,
  event: Record<string, unknown>,
): Promise<StaffingRouteOutcome> {
  const outcome = await chiefdStaffingApplied(gate.endpoint, path, body);
  if ("applied" in outcome) gate.record({ route: path, ...event });
  return outcome;
}

/**
 * A refusal is a failed tool call with chiefd's own machine code and wording —
 * never re-classified, never retried. `retryable: false` is the whole point:
 * `head-needs-successor` does not become true by asking again.
 *
 * Named for the SHAPE (a refused chiefd route) rather than for the staffing
 * family it started in, so every route family that gains this seam reads as
 * the one place this project words a refusal rather than as a copy-paste.
 */
function routeRefusal(subject: string, outcome: { refused: string; detail: string }, details: Record<string, unknown> = {}) {
  return toolResult(false, `${subject} refused: ${outcome.detail}`, {
    ...details,
    status: "refused",
    code: outcome.refused,
    retryable: false,
  });
}

type ManagedUnitKind = "department" | "contract";
type ManagedUnitAction = "launch" | "stop" | "remove";

async function managedUnit(
  context: OrganizationRuntimeContext,
  unitId: string,
  expectedKind: ManagedUnitKind,
): Promise<{ manifest: IntercomOrganizationManifest; unit: DepartmentRecord }> {
  const manifest = await requireManagedDepartment(context, unitId);
  const unit = manifest.departments[unitId]!;
  const actual = organizationUnitKind(manifest, unit);
  if (actual !== expectedKind) throw new Error(`Unit '${unitId}' is a ${actual}, not a ${expectedKind}`);
  return { manifest, unit };
}

// TOMBSTONE (#751/P4): `runtimeIdentity` read `runtimeSocket`/`runtimeSession` off
// the context so a caller could append `--socket`/`--session` to a launcher
// argv. Its last reader was the multi-unit resume, which now posts to chiefd
// directly. runtime identity does not cross into the backend, so the accessor is
// deleted rather than kept for a future caller that must not exist.

/**
 * Resume several managed departments in one direct durable operation.
 *
 * This replaced `spawn`ing `apps/cli/src/Main.ts department launch --units` —
 * a Pi extension shelling out to a TypeScript CLI to reach the daemon it was
 * already connected to. That CLI is gone, so `org_resume_departments` failed
 * on a missing file and a manager could not bring several stopped departments
 * back at all.
 *
 * `--socket`/`--session` are DELETED rather than translated into request
 * fields: runtime identity does not cross into the backend. They bought this
 * verb nothing even before the deletion — the CLI's own argument check for
 * `department launch --units` allowed only `--units`, so the two flags this
 * helper appended were rejected before the command ran.
 *
 * `skipActive: true` matches what the CLI asked for and is what makes a batch
 * naming one already-running unit resume the rest instead of refusing the
 * whole call. chiefd wakes its own reconcile inside the route, so this process
 * actuates no runtime and needs no second convergence call.
 */
async function runManagedUnitsResume(
  context: OrganizationRuntimeContext,
  kind: ManagedUnitKind,
  unitIds: readonly string[],
): Promise<{ message: string; kind: ManagedUnitKind; action: "launch"; unitIds: string[] }> {
  const requested = [...new Set(unitIds.map((value) => String(value).trim()).filter(Boolean))];
  if (!requested.length) throw new Error(`${kind} launch requires at least one unit id`);
  const manifest = await loadIntercomOrganization(context);
  const person = currentPerson(context, manifest);
  // No role gate — see `authorityRootDepartmentId`. Scope is checked per unit.
  // Prove the caller owns every member before any of them is touched. A batch
  // must not half-apply on an authority error.
  for (const unitId of requested) await managedUnit(context, unitId, kind);

  const outcome = await chiefdUnitsResume(chiefdEndpoint(context), {
    slug: companyKeyOf(context),
    departmentIds: requested,
    skipActive: true,
  });
  if ("refused" in outcome) throw new Error(`${kind} resume refused: ${outcome.detail}`);
  appendOrganizationEvent(context, {
    event: "unit-lifecycle-command",
    personId: person.id,
    unitKind: kind,
    action: "launch",
    unitId: requested.join(","),
    route: UNITS_RESUME_ROUTE,
    at: new Date().toISOString(),
  });
  const plural = requested.length === 1 ? `${kind}` : `${kind}s`;
  return {
    message: `Resumed ${requested.length} ${plural}: ${requested.join(", ")}. People come up when their head asks for them.`,
    kind,
    action: "launch",
    unitIds: requested,
  };
}

/**
 * #141: a department/contract removal is DURABLE the instant its manifest write
 * commits — a step that happens well before the launcher's own trailing runtime
 * teardown (park the panes, reconcile the runtime session, sweep the removed
 * subtree). That teardown can legitimately outrun the intercom's bounded
 * subprocess budget and get SIGKILLed, or hit a transient runtime/docstore blip, so
 * the `org <kind> remove` command exits NONZERO even though the unit is already
 * gone from the manifest. Without this check `runChecked` throws that nonzero exit,
 * `lifecycleFailure` reports `ok:false`, and (post-#139) the `isError` hook flips
 * it — the CEO is told a removal FAILED that in fact SUCCEEDED, the exact inverse
 * of #139. So on a failed remove we VERIFY THE ARTIFACT: re-read the manifest and, if
 * the unit is no longer present, the durable removal committed and the leftover
 * runtime convergence is the reactive reconciler's job. A read failure, or a unit
 * STILL present (a genuine pre-commit refusal — the #139 class), returns false so
 * the original failure still propagates. */
async function managedUnitRemovalCommitted(
  context: OrganizationRuntimeContext,
  unitId: string,
): Promise<boolean> {
  try {
    const manifest = await loadIntercomOrganization(context);
    return manifest.departments?.[unitId] === undefined;
  } catch {
    // Cannot prove the artifact — never fabricate a success over an unreadable
    // authority; let the original command failure stand.
    return false;
  }
}

/**
 * Run only the public, runtime-aware unit lifecycle. These commands own their
 * own materialization/reconcile and bounded handoff sequence, so this path
 * must never append a second low-level `org reconcile`.
 */
async function runManagedUnitLifecycle(
  context: OrganizationRuntimeContext,
  request: {
    kind: ManagedUnitKind;
    action: ManagedUnitAction;
    parentUnitId?: string;
    unitId?: string;
    spec?: unknown;
    /** org-ops R3: create the new department headed by this EXISTING person. */
    existingHeadPersonId?: string;
    /** What becomes of the unit that person already heads, when they head one. */
    vacates?: ChiefdHeadVacancy;
    reason?: string;
  },
): Promise<{ message: string; kind: ManagedUnitKind; action: ManagedUnitAction; unitId?: string; runtimeDeferred?: boolean; warning?: string }> {
  const operation = `${request.kind} ${request.action}`;
  let manifest = await loadIntercomOrganization(context);
  const person = currentPerson(context, manifest);
  // No role gate — see `authorityRootDepartmentId`. Scope is checked per unit.

  if (request.action === "launch") {
    if (!request.parentUnitId) throw new Error(`${operation} requires a parent unit`);
    const creating = request.spec !== undefined || request.existingHeadPersonId !== undefined;
    if (creating === Boolean(request.unitId)) {
      throw new Error(`${operation} requires exactly one new definition or existing unit id`);
    }
    if (request.unitId) {
      const existing = await managedUnit(context, request.unitId, request.kind);
      manifest = existing.manifest;
      if (existing.unit.parentDepartmentId !== request.parentUnitId) {
        throw new Error(`Unit '${request.unitId}' belongs under '${existing.unit.parentDepartmentId}', not '${request.parentUnitId}'`);
      }
    } else {
      // CREATE, so the create-path authority — see
      // `requireDepartmentCreationParent`. Every other branch here acts on a
      // unit that already exists and keeps `requireManagedDepartment`.
      manifest = await requireDepartmentCreationParent(context, request.parentUnitId);
    }
  } else {
    if (!request.unitId) throw new Error(`${operation} requires a unit id`);
    manifest = (await managedUnit(context, request.unitId, request.kind)).manifest;
  }

  const endpoint = chiefdEndpoint(context);
  const slug = (await readIntercomManifestWire(context)).key;
  const unitWord = request.kind === "contract" ? "Contract" : "Department";
  const record = (fields: Record<string, unknown>) => appendOrganizationEvent(context, {
    event: "unit-lifecycle-command",
    personId: person.id,
    unitKind: request.kind,
    action: request.action,
    parentUnitId: request.parentUnitId,
    unitId: request.unitId,
    at: new Date().toISOString(),
    ...fields,
  });

  // ---- CREATE (a spec, or an existing person to appoint as head) -----------
  if (request.action === "launch" && !request.unitId) {
    const spec = (request.spec ?? {}) as IntercomDepartmentSpec & {
      transient?: { engagement: string; expiresAt?: string };
    };
    const unit: ChiefdCreateUnit = request.kind === "contract"
      ? {
          kind: "contract",
          transient: {
            engagement: spec.transient?.engagement ?? "",
            launchedAt: new Date().toISOString(),
            ...(spec.transient?.expiresAt ? { expiresAt: spec.transient.expiresAt } : {}),
          },
        }
      : { kind: "department" };
    const outcome = await chiefdCreateDepartment(endpoint, departmentCreateRequest({
      slug,
      parentUnitId: request.parentUnitId!,
      spec,
      requesterPersonId: person.id,
      reason: request.reason,
      unit,
      ...(request.existingHeadPersonId ? { existingHeadPersonId: request.existingHeadPersonId } : {}),
      ...(request.vacates ? { vacates: request.vacates } : {}),
    }));
    if ("refused" in outcome) throw new Error(`${unitWord} create refused: ${outcome.detail}`);

    // ================= THE COMMIT BOUNDARY ================================
    //
    // `/v1/org/department/create` answered `applied: true`. The department, its
    // head and its staff are DURABLE. Nothing below this line may throw, and
    // nothing below this line may make this call answer `ok: false`.
    //
    // Why the boundary is here and not later. The steps that follow are not
    // SQL: they are pi-home materialization on disk, provider credentials, a
    // launch fence and runtime panes. A transaction cannot be stretched around
    // them — SQLite has nothing to roll back for a directory that was created,
    // and a compensating delete would be a SECOND write that can fail exactly
    // the way the first one did, leaving a company with a half-removed
    // department and no honest answer at all. chiefd already made this ruling
    // for its own half of the sequence: `materialize_after_commit` runs the
    // materialization post-commit and downgrades every failure to `warnings`,
    // "because returning an error there would tell a caller its request failed
    // when it half-succeeded — the caller then retries and is told the
    // department already exists". This orchestration simply had not followed
    // the boundary chiefd drew. It does now.
    //
    // Observed live: a CEO called `org_launch_department`, `reconcileRuntime`
    // threw, the tool answered a system fault, the CEO read the roster, saw
    // nothing it recognized, correctly concluded "no partial commit" and
    // retried — and was refused `a department with this id already exists`. Six
    // turns to reason out of an answer that contradicted the database.
    const committed: string[] = [...outcome.warnings];
    record({ departmentId: outcome.departmentId });
    // The people this created have no runtime yet; the reconcile is what
    // actually brings their panes up, exactly as the CLI path did. It reaches a
    // second route over HTTP, so it can fail for reasons that have nothing to
    // do with whether the department exists — and it runs post-commit, so its
    // failure is a convergence warning on a successful create, never a failed
    // create. The reconciler owns the runtime and retries on its own pass.
    try {
      const convergence = await reconcileRuntime(context);
      if (convergence) committed.push(convergence);
    } catch (error) {
      committed.push(postCommitConvergenceWarning(error));
    }
    const warning = committed.length ? committed.join(" ") : undefined;
    return {
      message: `Created ${request.kind} ${outcome.departmentId}.${warning ? `\n${warning}` : ""}`,
      kind: request.kind,
      action: request.action,
      unitId: outcome.departmentId,
      ...(warning ? { warning } : {}),
    };
  }

  // ---- RESUME / STOP / REMOVE — one id, one route -------------------------
  // chiefd models a unit's availability as `departments.state` (active|paused),
  // so `stop` IS pause (it clears launch intent and lets the converge reap the
  // panes) and relaunching a stopped unit IS resume. There is no third state
  // and no separate contract table: a contract is a department row carrying
  // transient metadata, so all three verbs are the same route for both kinds.
  const unitId = request.unitId!;
  const path = request.action === "remove"
    ? "/v1/org/department/remove-tree" as const
    : request.action === "stop"
      ? "/v1/org/department/pause" as const
      : "/v1/org/department/resume" as const;

  let outcome;
  try {
    outcome = await chiefdUnitStateChange(endpoint, path, { slug, departmentId: unitId });
  } catch (error) {
    // #141: a remove whose durable write already committed but whose trailing
    // teardown failed is a SUCCESS, not a failure — verify the artifact and
    // report it as such rather than telling a CEO a completed removal failed.
    // Every other failure, and a remove that did NOT commit, still propagates.
    if (request.action === "remove" && await managedUnitRemovalCommitted(context, unitId)) {
      record({ event: "unit-removal-committed-despite-command-failure", detail: error instanceof Error ? error.message : String(error) });
      return {
        message: `${unitWord} '${unitId}' was removed. Its runtime teardown was interrupted; the removal stands and must not be repeated.`,
        kind: request.kind,
        action: request.action,
        runtimeDeferred: true,
        warning: "The removal is durable; its runtime teardown was interrupted and completes without another call from you.",
      };
    }
    throw error;
  }
  if ("refused" in outcome) throw new Error(`${unitWord} ${request.action} refused: ${outcome.detail}`);

  // ================= THE COMMIT BOUNDARY ==================================
  // Identical ruling to the create branch above: the pause/resume/remove row
  // write has committed, so the trailing convergence is a warning on a success
  // and never a failure. #141 already established this for a `remove` whose
  // ROUTE call failed after committing; the reconcile that follows a route that
  // answered 200 had the same exposure and was left throwing.
  record({ removedDepartmentIds: outcome.removedDepartmentIds, departedPersonIds: outcome.departedPersonIds });

  let warning: string | undefined;
  try {
    warning = await reconcileRuntime(context);
  } catch (error) {
    warning = postCommitConvergenceWarning(error);
  }
  const summary = request.action === "remove"
    ? `Removed ${request.kind} ${unitId}${outcome.departedPersonIds?.length ? ` and offboarded ${outcome.departedPersonIds.length} ${outcome.departedPersonIds.length === 1 ? "person" : "people"}; their records and history are retained` : ""}.`
    : request.action === "stop"
      ? `Stopped ${request.kind} ${unitId}. Context and disk state are retained.`
      : `Resumed ${request.kind} ${unitId}. People come up when their head asks for them.`;
  return {
    message: `${summary}${warning ? `\n${warning}` : ""}`,
    kind: request.kind,
    action: request.action,
    ...(warning ? { warning } : {}),
  };
}

const DECLARABLE_PERSON_TOOL_NAMES = new Set<string>(BUILTIN_TOOLS);

/**
 * One declared grant list, checked before a staffing request reaches chiefd.
 *
 * Pi validates normal model calls against the schema below. Direct and
 * restored callers can bypass that validator, so the request mapping applies
 * the same closed vocabulary. The organization surface is deliberately not in
 * this set: the launcher composes `org_*` tools from live role and scope.
 */
function declarablePersonTools(
  raw: Record<string, unknown>,
  field: string,
): string[] | undefined {
  const value = raw.tools;
  if (!Array.isArray(value)) return undefined;
  const tools = value.filter((entry): entry is string => typeof entry === "string");
  if (tools.some((tool) => !DECLARABLE_PERSON_TOOL_NAMES.has(tool))) {
    throw new Error(
      `${field}.tools accepts only Pi builtins: ${BUILTIN_TOOLS.join(", ")}. `
      + "Never put org_* names in this array; organization tools are installed automatically from role and scope, so omit them.",
    );
  }
  return tools;
}

const PERSON_SEED_FIELDS = {
  id: Type.Optional(Type.String({ description: "Stable kebab-case person id. It is the handle the operator types, so make it the first name in lower case — 'carlos', 'priya', 'mo' — and never the job, such as 'chief-of-staff' or 'head-of-marketing'." })),
  name: Type.String({ description: "The person's NAME, and never their job. One short first name a person can remember and say — Carlos, Priya, Mo, Chris. 'Head of Engineering' is a title, not a name: put that in title." }),
  title: Type.Optional(Type.String({ description: "The job, in as many words as it needs: 'Head of Engineering', 'Chief of Staff'. This is where a role belongs, never in name." })),
  mandate: Type.String({ description: "Narrow owned output and definition of done. Put ordinary technologies such as TypeScript, SQLite, Bun, or SQL here." }),
  tools: Type.Optional(Type.Array(Type.String({
    enum: [...BUILTIN_TOOLS],
    description: "Optional Pi builtin only: read, bash, edit, write, grep, find, or ls. Every person already receives all seven. Never put org_* names here; organization tools such as org_send are installed automatically from role and scope, so omit them.",
  }), { description: "Redundant Pi builtin declarations only. Prefer to omit this array." })),
  startActive: Type.Optional(Type.Boolean()),
};

/** ONE seed dialect for every verb that hires somebody. There is no route in
 * it: an agent boots as plain Pi on the operator's own defaults, so naming a
 * provider or a model here would ask a manager to choose something the product
 * no longer carries. */
const PERSON_SEED = Type.Object({ ...PERSON_SEED_FIELDS }, { additionalProperties: false });

/**
 * What becomes of the department somebody is leaving without a head.
 *
 * ONE schema and ONE shape check for both verbs that can vacate a headship —
 * creating a department led by a sitting head, and transferring one out. The
 * rule itself is stated once more, in chiefd; this is only the payload it is
 * stated in.
 */
const HEAD_VACANCY_PARAM = Type.Object({
  kind: Type.Union([
    Type.Literal("hand-over", { description: "Promote successorPersonId to head the department being left" }),
    Type.Literal("dissolve", { description: "That person is the department's LAST member, so the emptied department is removed" }),
  ]),
  successorPersonId: Type.Optional(Type.String({ description: "Required for hand-over: a member of the department being left" })),
}, { additionalProperties: false, description: "Required only when the person already heads a department. Says what becomes of it." });

/**
 * FLAT. There is no `department` wrapper, and its absence is the fix.
 *
 * A live CEO could not create a department because it emitted the nested
 * `department` object as a JSON STRING — twice, once refused by our own
 * validator (`department: must be object`) and once refused by the provider
 * before any of our code ran (`tool arguments invalid: trailing characters`).
 * #1134 added a `prepareArguments` repair seam, which fixes the first shape and
 * cannot fix the second: that refusal happens upstream, where we have no seam.
 *
 * The only repair that reaches both is to stop asking for the shape that gets
 * fumbled. A one-key wrapper carrying five fields is pure ceremony — it names
 * nothing the tool name does not already say — so it is gone, and the tool now
 * has exactly the shape `org_hire` has: a flat argument list plus a person
 * seed. One person-seed dialect across the family, one level of nesting, and
 * the key the model double-encoded no longer exists.
 *
 * NOT flattened further into `headName` / `headTitle` / `headMandate`: that
 * would invent a SECOND way to describe a person alongside `org_hire`'s seed,
 * and `staff` would still need the seed anyway. Two dialects for one concept is
 * a worse surface than one nested object the model has always got right.
 */
const ADD_DEPARTMENT_PARAMETERS = Type.Object({
  parentDepartmentId: Type.Optional(Type.String({ description: "Where the new department attaches. When the request says its head reports to a named person, this is the id of the department THAT person heads — 'reports to' names the parent. Omit it only when the request named nobody; it then defaults to your own management root." })),
  departmentId: Type.Optional(Type.String({ description: "Optional stable kebab-case id for the new department; omit and chiefd mints one from the name" })),
  name: Type.String({ description: "Display name of the new department" }),
  purpose: Type.String({ description: "What this department owns. Plain prose, never JSON." }),
  head: Type.Optional(Type.Object({ ...PERSON_SEED.properties }, {
    additionalProperties: false,
    description: "Hire a NEW person to lead this department. Send it as a real JSON object, never as a quoted string. Provide this OR existingHeadPersonId — never both, never neither.",
  })),
  existingHeadPersonId: Type.Optional(Type.String({ description: "Id of an EXISTING person to lead this department. They are MOVED into it and appointed head — this is the ordinary way a worker becomes a manager, and it asks for no standing they do not already have. Provide this OR head — never both, never neither." })),
  vacates: Type.Optional(HEAD_VACANCY_PARAM),
  staff: Type.Optional(Type.Array(PERSON_SEED, { description: "Optional initial staff, hired with the new head in one atomic commit. Only with head; an existingHeadPersonId create takes none." })),
}, { additionalProperties: false });

const HIRE_PARAMETERS = Type.Object({
  // OPTIONAL, because the description promises a default and a required field
  // cannot deliver one. It was `Type.String` — required — under a description
  // opening "DEFAULT: the department YOU head". An agent read the prose,
  // reasoned that it should omit the field, met a schema that would not let it,
  // and improvised the most salient name in context: the company. It obeyed
  // the instrument over the claim, which is the correct thing for it to do.
  departmentId: Type.Optional(Type.String({
    description:
      "Where this person lands. OMIT IT to hire into the department you head — that is the "
      + "DEFAULT and it is what you want almost always, because a hire joins the team that "
      + "asked for it. Pass one only to override that, and only when the operator named a "
      + "different department. This call never creates a department, and a job title never "
      + "asks for one: \"hire a Chief of Staff\" is a hire into your own department, not a new "
      + "unit. Create a department only when the operator asked for a department in those "
      + "words. If you do pass one, the company name or slug is NEVER a department id — the "
      + "root department's id is in org_roster.",
  })),
  /** One person, the original shape. */
  person: Type.Optional(PERSON_SEED),
  /** Several people in ONE call — see the batch note in `execute`. */
  people: Type.Optional(Type.Array(PERSON_SEED)),
});

/**
 * The vacancy payload, or the sentence saying why it is malformed.
 *
 * SHAPE only. Whether a hand-over names a real member, and whether a dissolve
 * is honest about the department being empty, are chiefd's answers — it holds
 * the tree and refuses naming the department and the members who may take it.
 * A second opinion here would be the same rule written twice.
 */
export function normalizeHeadVacancy(
  vacates: { kind: "hand-over" | "dissolve"; successorPersonId?: string } | undefined,
  field: string,
): { value?: ChiefdHeadVacancy } | { refusal: string } {
  if (!vacates) return {};
  if (vacates.kind === "dissolve") return { value: { kind: "dissolve" } };
  const successorPersonId = vacates.successorPersonId?.trim();
  if (!successorPersonId) {
    return {
      refusal: `A hand-over needs ${field}.successorPersonId — the member who becomes head of the department being left. `
        + `If that department has no other member, the answer is { kind: "dissolve" }.`,
    };
  }
  return { value: { kind: "hand-over", successorPersonId } };
}

const UNIT_DEFINITION_FIELDS = {
  id: Type.Optional(Type.String({ description: "Stable kebab-case unit id" })),
  name: Type.String(),
  purpose: Type.String(),
  head: PERSON_SEED,
  staff: Type.Optional(Type.Array(PERSON_SEED)),
};

const DEPARTMENT_DEFINITION = Type.Object({
  id: UNIT_DEFINITION_FIELDS.id,
  name: UNIT_DEFINITION_FIELDS.name,
  purpose: UNIT_DEFINITION_FIELDS.purpose,
  head: PERSON_SEED,
  staff: Type.Optional(Type.Array(PERSON_SEED)),
});
const CONTRACT_DEFINITION = Type.Object({
  ...UNIT_DEFINITION_FIELDS,
  transient: Type.Object({
    engagement: Type.String({ description: "Bounded deliverable and definition of closure" }),
    expiresAt: Type.Optional(Type.String({ description: "Optional ISO-8601 expiry" })),
  }),
});

function toolResult(ok: boolean, text: string, details: Record<string, unknown> = {}) {
  return { content: [{ type: "text" as const, text }], details: { ok, ...details } };
}

/** What every tool in this file answers with. Named so a batching tool can hold
 * one per item and hand the single-item one back verbatim. */
type ToolResult = { content: Array<{ type: "text"; text: string }>; details: Record<string, unknown> };

// TOMBSTONE (#751/P4): `staleReflectionResult` and
// `reflectionTransitionIsNoLongerCurrent` lived here. They turned a stale
// `org_reflect` write into a quiet no-op card. Both died with the tool.

function compactPresentation(text: string, limit = 88): { text: string; truncated: boolean } {
  const normalized = text.replace(/\s+/g, " ").trim();
  return { text: normalized.slice(0, limit), truncated: normalized.length > limit };
}

/** Presentation only: durable ISO timestamps stay in state, cards show local wall time. */
function localTimestamp(value: unknown): string {
  const instant = new Date(String(value));
  if (!Number.isFinite(instant.getTime())) return "date unavailable";
  return new Intl.DateTimeFormat("en-US", {
    month: "short", day: "numeric", hour: "numeric", minute: "2-digit", hour12: true, timeZoneName: "short",
  }).format(instant);
}

function toolOutputText(result: unknown): string {
  const content = (result as { content?: Array<{ type?: unknown; text?: unknown }> } | undefined)?.content;
  const text = content?.find((item) => item.type === "text" && typeof item.text === "string")?.text;
  return typeof text === "string" ? text : "Unknown ChiefD response";
}

/** Domain emoji + verb-ing title for each org tool's in-progress (`renderCall`)
 * line — the tool's own glyph, kept separate from a fixed vocabulary entry
 * per the card house style's in-progress carve-out (docs/cards-style.md). */
const ORGANIZATION_TOOL_DOMAIN_ICONS: Record<string, { emoji: string; title: string }> = {
  org_send: { emoji: CARD_GLYPHS.send, title: "Sending message" },
  org_roster: { emoji: CARD_GLYPHS.roster, title: "Checking company roster" },
  org_launch_department: { emoji: CARD_GLYPHS.starting, title: "Starting department" },
  org_stop_department: { emoji: CARD_GLYPHS.stopping, title: "Stopping department" },
  org_remove_department: { emoji: CARD_GLYPHS.removing, title: "Removing department" },
  org_launch_contract: { emoji: CARD_GLYPHS.starting, title: "Starting contract" },
  org_stop_contract: { emoji: CARD_GLYPHS.stopping, title: "Stopping contract" },
  org_remove_contract: { emoji: CARD_GLYPHS.removing, title: "Removing contract" },
  org_add_department: { emoji: CARD_GLYPHS.department, title: "Adding department" },
  org_pause_department: { emoji: CARD_GLYPHS.pausing, title: "Pausing department" },
  org_resume_department: { emoji: CARD_GLYPHS.resuming, title: "Resuming department" },
  org_resume_departments: { emoji: CARD_GLYPHS.resuming, title: "Resuming departments" },
  org_hire: { emoji: CARD_GLYPHS.hire, title: "Hiring teammate" },
  org_bench: { emoji: CARD_GLYPHS.bench, title: "Benching teammate" },
  org_recall: { emoji: CARD_GLYPHS.recall, title: "Returning teammate to work" },
  org_stand_down: { emoji: CARD_GLYPHS.circuit, title: "Standing the whole company down" },
  org_resume: { emoji: CARD_GLYPHS.resuming, title: "Letting the company work again" },
  org_start_person: { emoji: CARD_GLYPHS.startPerson, title: "Bringing up one teammate" },
  org_stop_person: { emoji: CARD_GLYPHS.stopPerson, title: "Standing one teammate down" },
  org_transfer: { emoji: CARD_GLYPHS.transfer, title: "Moving teammate" },
  org_reparent_department: { emoji: CARD_GLYPHS.moveDepartment, title: "Moving department" },
  org_appoint_department_head: { emoji: CARD_GLYPHS.appointHead, title: "Appointing head" },
  // NOT the same glyph as hire. One icon for two opposite outcomes tells a
  // reader nothing: a wave says hello on the way in and goodbye on the way
  // out, and the card is the only thing distinguishing them.
  org_offboard: { emoji: CARD_GLYPHS.offboard, title: "Offboarding teammate" },
  // Durable reminders. Create and list share the alarm because they are the
  // same subject; STOPPING one is the state that differs, so it is the one
  // that gets its own glyph.
  org_create_reminder: { emoji: CARD_GLYPHS.reminder, title: "Scheduling reminder" },
  org_list_reminders: { emoji: CARD_GLYPHS.reminder, title: "Listing reminders" },
  org_stop_reminder: { emoji: CARD_GLYPHS.reminderOff, title: "Removing reminder" },
};

function organizationToolDomainIcon(name: string): { emoji: string; title: string } {
  return ORGANIZATION_TOOL_DOMAIN_ICONS[name] ?? { emoji: "", title: name.replace(/^org_/, "").replaceAll("_", " ") };
}

function organizationToolTarget(organization: string, name: string, args: Record<string, any>): string {
  if (name === "org_send") return `@${displayHandle(organization, String(args.to || "recipient").replace(/^@/, ""))}`;
  if (name === "org_roster") return "disk authority";
  if (args.personId) return `@${displayHandle(organization, String(args.personId).replace(/^@/, ""))}`;
  if (args.unitId) return String(args.unitId);
  if (args.departmentId) return String(args.departmentId);
  if (args.parentUnitId || args.parentDepartmentId) return `under ${args.parentUnitId || args.parentDepartmentId}`;
  return "";
}

/** `renderResult` only ever receives the tool RESULT, never the original call
 * `args` `organizationToolTarget` reads — so a failure card's target is a
 * best-effort read of whatever identifying field the tool's own `details`
 * happened to carry (often none, for a bare caught exception). Never worse
 * than the previous "no target at all", only ever better. */
function organizationToolFailureTarget(organization: string, detail: Record<string, any>): string {
  if (typeof detail.personId === "string") return `@${displayHandle(organization, detail.personId.replace(/^@/, ""))}`;
  if (typeof detail.unitId === "string") return detail.unitId;
  if (typeof detail.departmentId === "string") return detail.departmentId;
  return "";
}

interface ToolSuccessPresentation {
  icon: CardState | CardIcon;
  title: string;
  target?: string;
}

/** A read-only op with no mutation keeps its own domain emoji in the
 * success color, per the house style's success carve-out; every mutating op
 * gets the plain ✅. */
function organizationToolSuccessPresentation(organization: string, name: string, detail: Record<string, any>): ToolSuccessPresentation {
  if (name === "org_roster") return { icon: domainIcon("📋", "success"), title: "Roster updated" };
  if (name === "org_send") {
    if (detail.alreadyCompleted) return { icon: "success", title: "Final result already saved", target: "no duplicate sent" };
    if (detail.envelope) return { icon: "success", title: "Message sent", target: `@${displayHandle(organization, String(detail.envelope.to || "recipient").replace(/^@/, ""))}` };
    return { icon: "success", title: "Work result sent" };
  }
  const knownTitle = ({
    org_launch_department: "Department active",
    org_stop_department: "Department stopped",
    org_remove_department: "Department removed",
    org_launch_contract: "Contract active",
    org_stop_contract: "Contract stopped",
    org_remove_contract: "Contract removed",
    org_add_department: "Department added",
    org_pause_department: "Department paused",
    org_resume_department: "Department active",
    // #360: org_resume_departments had no entry here, so it fell through to
    // the generic `${domain.title} complete` fallback — "Resuming departments
    // complete" (gerund + "complete" reads as broken grammar, not a card
    // fallback gap).
    org_resume_departments: "Departments resumed",
    org_hire: "Teammate hired",
    // #360: org_bench/org_recall/org_start_person/org_stop_person each have
    // their own custom renderResult (below), so these four entries are never
    // actually reached — but they had drifted from the wording their custom
    // cards actually show ("One teammate stood down" here vs "Stood down" on
    // the real card). Keep both in sync so a future removal of either custom
    // renderer can't silently reintroduce the old drift.
    org_bench: "Benched",
    org_recall: "Back at work",
    org_stand_down: "Company stood down",
    org_resume: "Company working again",
    org_start_person: "Started",
    org_stop_person: "Stood down",
    org_transfer: "Teammate moved",
    org_reparent_department: "Department moved",
    org_appoint_department_head: "Head appointed",
    org_offboard: "Teammate offboarded",
  } as Record<string, string>)[name];
  if (knownTitle) return { icon: "success", title: knownTitle };
  // Unclassified tool: keep the domain glyph (in success color) plus "complete",
  // exactly the previous fallback text/color, just split into icon + title.
  const domain = organizationToolDomainIcon(name);
  return { icon: domainIcon(domain.emoji, "success"), title: `${domain.title} complete` };
}

/** Color-independent rendering of a success presentation, used only to check
 * whether the raw tool output already restates the card's own headline. */
function organizationToolSuccessPlainText(presentation: ToolSuccessPresentation): string {
  const icon = typeof presentation.icon === "string" ? cardStateIcon(presentation.icon) : presentation.icon;
  const head = icon.emoji ? `${icon.emoji} ${presentation.title}` : presentation.title;
  return presentation.target ? `${head} · ${presentation.target}` : head;
}

/** Retryable/blocked/input-repair presentation for a failed tool result,
 * mirroring the pre-migration `retryLabel` derivation exactly (including its
 * quirk: the specific `recipient_lookup`/`message_text_required` statuses only
 * produce their own label when `waiting` is also true) — `undefined` means this
 * is a hard failure, not a retryable/waiting/busy one. */
/**
 * Whether a failure card should say "refused" rather than "failed".
 *
 * ONE rule, in one place, because there are three renderers that build a
 * failure title and a rule copied three times is three rules waiting to
 * disagree. A classified failure is one the tool DECIDED and can explain, so
 * "refused" — a word that invites a corrected call. Anything else stays
 * "failed", which invites a retry.
 *
 * `fault: true` is what a producer sets when it carries a status for CONTEXT
 * rather than as a classification: a partial batch naming what already landed,
 * where the wrapped error may be a genuine crash. A marker, not a list, so the
 * next producer in that position is covered without this predicate changing.
 */
function isCallerRefusalCard(detail: Record<string, any> | undefined): boolean {
  return typeof detail?.status === "string" && detail.fault !== true;
}

function organizationToolRetryPresentation(detail: Record<string, any>): { state: CardState; title: string } | undefined {
  const waiting = detail.retryable === true || detail.status === "awaiting_handoff" || detail.status === "awaiting_handoffs";
  if (!waiting) return undefined;
  if (detail.status === "recipient_lookup") return { state: "input-repair", title: "Choose a teammate" };
  if (detail.status === "message_text_required") return { state: "input-repair", title: "Add message body" };
  // #751/G9-S0: a `status: "busy"` -> "Company updating" wait state stood here.
  // Its only producer was the deleted lock-busy classifier; chiefd's own
  // `ChiefdError::Busy` answers HTTP 503, which reaches this file as a
  // `ChiefdUnavailableError` and degrades through `transientDegradeMessage`.
  return { state: "handoff", title: "Waiting for handoff" };
}

/** Typed production boundary for the broadly shared organization-tool cards.
 * It keeps the default renderer's special semantic branches declarative while
 * preserving the resolved Pi theme and live expansion state. */
function renderDefaultOrganizationToolCard(
  theme: CardTheme,
  spec: CardSpec,
  options: RenderCardOptions = {},
) {
  return renderOrganizationCard(theme, spec, options);
}

/** The root-only escalation renderer has its own narrow seam because its
 * durable operator receipt is not a normal tool-result payload. */
function renderOperatorEscalationCard(
  theme: CardTheme,
  spec: CardSpec,
  options: RenderCardOptions = {},
) {
  return renderOrganizationCard(theme, spec, options);
}

function defaultOrganizationToolRenderCall(organization: string, name: string, args: Record<string, any>, theme: any, mentions?: MentionColorizer) {
  const target = organizationToolTarget(organization, name, args);
  const icon = organizationToolDomainIcon(name);
  return renderDefaultOrganizationToolCard(theme, {
    kind: "tool-call",
    icon: domainIcon(icon.emoji),
    inProgress: true,
    title: icon.title,
    target: target || undefined,
    mentions,
    body: { kind: "none" },
    boxed: false,
  });
}

function defaultOrganizationToolRenderResult(organization: string, name: string, result: any, { expanded }: { expanded?: boolean }, theme: any, mentions?: MentionColorizer) {
  const detail = (result?.details ?? {}) as Record<string, any>;
  const output = toolOutputText(result);
  if (!detail.ok) {
    if (detail.status === "tool_call_loop_stopped") {
      return renderDefaultOrganizationToolCard(theme, {
        kind: "tool-failure", icon: "circuit", title: "Message retry loop stopped",
        detail: "This model turn ended after three empty messages. No message was queued; the next turn can retry once with a concise body.",
        body: { kind: "none" }, boxed: false,
      });
    }
    const retry = organizationToolRetryPresentation(detail);
    const summary = compactPresentation(output, 120);
    // A `status` means the tool itself classified this as a known, named
    // condition (a business-rule refusal); its absence means the card is
    // showing a raw caught exception (launcher/chiefd/runtime/etc.) — flag that
    // distinction so a reader never mistakes a system fault for bad input.
    const target = organizationToolFailureTarget(organization, detail);
    const unclassified = !retry && typeof detail.status !== "string";
    // The headline's inline decorations — the system-fault / opId / summary tags
    // and (when truncated) the inline expand hint — are structured `titleTags`
    // the site describes by token NAME, so renderCard colors them and no color
    // is hand-rolled here (AC1). #333: `opId` is the id the structured failure
    // record was logged under, so a cryptic card is one grep from full context.
    // THE VERB FOLLOWS THE CLASSIFICATION, not a list of card kinds.
    //
    // A classified failure is one the tool DECIDED and can explain, so it is
    // "refused" — a word that invites a corrected call. An unclassified one is
    // a caught exception, so it stays "failed", which invites a retry. Getting
    // this backwards in either direction is the defect: calling a crash
    // "refused" tells a reader to fix a call that was never wrong.
    //
    // `fault: true` is the one thing a producer sets when it carries a status
    // for CONTEXT rather than as a classification — a partial batch naming
    // what already landed, where the wrapped error may be a real crash. It is
    // a marker rather than a list, so a future producer in the same position
    // is covered without this line changing.
    const refused = !retry && isCallerRefusalCard(detail);
    const titleTags: CardTag[] = [];
    // THE TAG READS THE SAME MARKER AS THE VERB. `unclassified` alone measured
    // only the ABSENCE of a status, so a result carrying one for context while
    // wrapping a real crash lost the tag — the verb had moved to the fault
    // marker and the tag had stayed on the old instrument. One classification,
    // two surfaces, and they must not disagree: a reader debugging a mid-batch
    // crash would otherwise see "failed" with no crash marker beside a list of
    // people already hired, and reasonably conclude they had passed bad input.
    if (unclassified || detail.fault === true) titleTags.push({ text: "(system fault)", token: "dim" });
    if (typeof detail.opId === "string") titleTags.push({ text: `(ref ${detail.opId})`, token: "dim" });
    if (summary.text) titleTags.push({ text: `· ${summary.text}${summary.truncated ? "…" : ""}`, token: "dim" });
    if (!expanded && summary.truncated) titleTags.push({ text: CARD_EXPAND_HINT_TEXT, token: "dim", sep: "  " });
    return renderDefaultOrganizationToolCard(theme, {
      kind: "tool-failure",
      icon: retry ? retry.state : "failure",
      title: retry
        ? retry.title
        : `${organizationToolDomainIcon(name).title} ${refused ? "refused" : "failed"}`,
      target: retry ? undefined : (target || undefined),
      mentions,
      titleTags,
      body: expanded && output ? { kind: "prose", text: output } : { kind: "none" },
      boxed: false,
    }, { expanded });
  }
  const presentation = organizationToolSuccessPresentation(organization, name, detail);
  let body: CardSpec["body"] = { kind: "none" };
  if (name === "org_send" && detail.envelope?.body) {
    const message = String(detail.envelope.body);
    const summary = compactPresentation(message, 96);
    // A sent message always exposed Ctrl+O, even when its preview fit. Keep
    // that stable affordance while the shared entry owns the surrounding card.
    body = expanded
      ? { kind: "lines", lines: [{ text: "" }, { text: message }] }
      : { kind: "lines", lines: [{ text: `${summary.text}${summary.truncated ? "…" : ""}`, raw: `  ${cardHint(theme)}` }] };
  } else if (expanded && output && !output.startsWith(organizationToolSuccessPlainText(presentation))) {
    body = { kind: "prose", text: output, collapse: "hidden" };
  }
  return renderDefaultOrganizationToolCard(theme, {
    kind: "tool-success",
    icon: presentation.icon,
    title: presentation.title,
    target: presentation.target,
    mentions,
    body,
    footer: detail.warning ? [{ text: String(detail.warning), token: "warning" }] : undefined,
    boxed: false,
  }, { expanded });
}

/**
 * #139/gh#516: a failed org tool result (`details.ok === false`) is a GENUINE
 * operational incident — the department was not created, the transfer did not
 * happen — as opposed to an expected/benign condition, EXACTLY when it is not:
 *   - retryable (`details.retryable === true`): a busy lock, an awaiting-handoff
 *     wait, or an input-repair prompt the agent is meant to retry, or
 *   - the empty-message loop stop (`status === "tool_call_loop_stopped"`), which
 *     ends a turn cleanly rather than reporting a fault.
 * This is precisely the branch in `organizationToolRegistrar`'s execute wrapper
 * where `logOperationFailure` runs and the `opId` is stamped. Kept as one shared
 * predicate so the `isError` flip (in the `tool_result` handler) and that
 * logging branch can never drift apart. `toolResult()` records failure only in
 * `details.ok`; without this the agent is handed a NON-ERROR result whose text
 * merely *describes* a failure, so a refused lifecycle call reads as success.
 */
function isGenuineToolFailure(details: Record<string, any> | undefined | null): boolean {
  if (!details || details.ok !== false) return false;
  if (details.retryable === true) return false;
  if (details.status === "tool_call_loop_stopped") return false;
  return true;
}

type OrganizationToolRegistrar = Pick<ExtensionAPI, "registerTool">;

/** Install every org tool with a compact card, while preserving richer tool-specific renderers. */
function organizationToolRegistrar(pi: ExtensionAPI, context: OrganizationRuntimeContext): OrganizationToolRegistrar {
  return {
    registerTool: ((definition: Record<string, any>) => {
      const name = String(definition.name);
      const execute = definition.execute;
      pi.registerTool({
        ...definition,
        execute: async (...args: any[]) => {
          // #333: one opId per call, generated regardless of outcome — cheap,
          // and only ever consumed on the failure paths below. `args[1]` is
          // `params` under this codebase's uniform `execute(toolCallId,
          // params, ...)` tool signature (verified against every registered
          // org tool), so this reads real call inputs without any per-tool
          // plumbing.
          const opId = generateOperationId();
          const inputsDigest = boundedInputsDigest(args[1]);
          try {
            const result = await execute(...args);
            if (result?.details?.ok === false) {
              // Missing canonical text is rejected before durable work and is
              // ordinary model-input repair, not an operational incident.
              // Preserve its exact no-mutation contract through this wrapper:
              // even the bounded event journal must remain byte-for-byte still.
              const quietInputRepair = result.details.status === "message_text_required";
              if (result.details.retryable === true) {
                if (!quietInputRepair) {
                  appendOrganizationEvent(context, {
                    event: "tool-retry-deferred",
                    tool: name,
                    status: result.details.status,
                    personId: context.personId,
                    detail: toolOutputText(result),
                    at: new Date().toISOString(),
                  });
                }
              } else if (isGenuineToolFailure(result.details)) {
                logOperationFailure(context, `tool:${name}`, {
                  actor: context.personId,
                  target: organizationToolFailureTarget(context.organization, result.details),
                  inputsDigest,
                  cause: toolOutputText(result),
                  retryable: false,
                  opId,
                });
                // Echo the opId on the card itself (defaultOrganizationToolRenderResult
                // reads `detail.opId`) so a cryptic failure is one grep away
                // from this exact log line — only reliable here, where this
                // wrapper still holds the mutable result object; a rethrown
                // error below has already been flattened to a string by most
                // tools' own catch blocks by the time anything could render it.
                if (result.details && typeof result.details === "object") result.details.opId = opId;
              }
            }
            return result;
          } catch (error) {
            logOperationFailure(context, `tool:${name}`, {
              actor: context.personId,
              inputsDigest,
              cause: error,
              retryable: false,
              opId,
            });
            throw error;
          }
        },
        renderCall: definition.renderCall ?? ((args: Record<string, any>, theme: any) =>
          defaultOrganizationToolRenderCall(context.organization, name, args, theme, personMentionColorizer(theme, context))),
        renderResult: definition.renderResult ?? ((result: any, options: { expanded?: boolean }, theme: any) => defaultOrganizationToolRenderResult(context.organization, name, result, options, theme, personMentionColorizer(theme, context))),
      } as any);
    }) as ExtensionAPI["registerTool"],
  };
}

/**
 * A refusal the tool DECIDED, as opposed to an exception it suffered.
 *
 * # The lie this exists to end
 *
 * Card rendering classifies a failure by whether the result carries a
 * `status`: with one it is a named, business-rule refusal; without one it is a
 * raw caught exception and the card is tagged `(system fault)`. That
 * distinction is right, and the throw path defeated it. Validation refusals
 * were raised as plain `Error`s, and the adapters that catch them flatten an
 * error into a status-less result — so every carefully-worded caller refusal
 * arrived at the renderer indistinguishable from a crash, and was labelled as
 * one.
 *
 * **A refusal that lies about whose fault it is invites the wrong recovery.**
 * A system fault invites the same call again; a caller error invites a
 * corrected one. An agent told "(system fault)" for naming a company where a
 * department belongs will retry the identical call, because retrying is what
 * that label means.
 *
 * # When to throw this instead of `Error`
 *
 * Throw `CallerRefusal` when the tool has DECIDED something and can say why:
 * the caller can act on it, whether by correcting the call or by changing the
 * company. Throw a plain `Error` for an invariant no input should reach — a
 * malformed docstore reply, an unparseable authority file, an impossible
 * state. Those ARE system faults and the tag on them is correct and useful.
 *
 * The marker travels ON the error because this file forbids a second parser:
 * classifying by matching message text would be exactly that.
 */
class CallerRefusal extends Error {
  readonly status: string;

  constructor(message: string, status = "refused") {
    super(message);
    this.name = "CallerRefusal";
    this.status = status;
  }
}

/**
 * The result a caught error becomes, preserving a decided refusal's status.
 *
 * EVERY catch path that flattens a throw into a `toolResult` funnels through
 * here. It has to: flattening an error by hand drops the status, and the
 * renderer then calls a decided refusal a system fault again — which is the
 * whole defect this helper exists to close.
 *
 * No count is written here on purpose. An earlier draft of this comment named
 * one, and the number was already wrong when it shipped — the audit that
 * produced it had found three of the eight sites. A remembered number in a
 * comment is a claim that goes stale silently, and this file is the last place
 * that should carry one. `CallerRefusalClassification`'s
 * "every catch path funnels through refusalResult" sweep enforces the rule
 * mechanically instead, so it covers the site somebody adds next month as well
 * as the ones here today.
 */
function refusalResult(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  return error instanceof CallerRefusal
    ? toolResult(false, message, { status: error.status })
    : toolResult(false, message);
}

function lifecycleFailure(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  // #751/G9-S0: a `status: "busy"` branch used to sit here, keyed off the
  // pre-mutation lock-busy classifier. Every refusal it matched came from the
  // file-mutex/SQL-lease families that are now deleted, so it could not fire.
  // Lifecycle contention is chiefd's decision to make and chiefd's refusal to
  // word; this adapter surfaces it rather than re-classifying it.
  // #443: a transient docstore/chiefd transport blip must degrade to a legible,
  // retryable card — never a raw "chiefd docstore unreachable at <url>" caught
  // exception rendered as a bare truncated "(system fault)" failure. This is the
  // same degrade `organizationSendFailure` already applies to org_send.
  const degraded = transientDegradeMessage("This staffing change", error);
  if (degraded) {
    return toolResult(false, degraded, { status: "docstore_unreachable", retryable: true });
  }
  // TOMBSTONE (#751/P4): an `awaiting bounded handoffs from: …` branch stood
  // here and told the manager to retry once the named people had run
  // `org_reflect`. Its only producer was chiefd's blocking handoff fence,
  // which is gone ("a finished person moves immediately"), and the tool it
  // pointed at is deleted, so nothing can reach it.
  return refusalResult(error);
}

/**
 * These are safe, expected repair paths for an LLM using a durable tool
 * contract. They deliberately remain failed tool calls (nothing was sent or
 * completed), but are quiet retryable guidance rather than exceptions that
 * pollute the company's diagnostic log or look like a runtime fault.
 */
function organizationSendFailure(error: unknown, context: OrganizationRuntimeContext) {
  const message = error instanceof Error ? error.message : String(error);
  // #384: a transient docstore/write-service blip (already retried once by
  // `supervisionLedger`/`runChecked`) must degrade to a legible message, not
  // the raw "Cannot read supervision authority '...': chiefd docstore
  // unreachable at <url>" exception.
  const degraded = transientDegradeMessage("Sending this message", error);
  if (degraded) {
    return toolResult(false, degraded, { status: "docstore_unreachable", retryable: true, organization: context.organization });
  }
  if (/^org_send requires a non-empty body\./.test(message)) {
    return toolResult(false, message, {
      status: "message_text_required",
      retryable: true,
      organization: context.organization,
    });
  }
  const unknownRecipient = message.match(/^Unknown employed recipient '([^']+)'/);
  if (unknownRecipient) {
    return toolResult(false, [
      `No message was sent: '${unknownRecipient[1]}' is not an organization person.`,
      "Call org_roster and retry with one exact person id (or 'all' for a real broadcast).",
      "'launcher' is infrastructure, never a message recipient.",
    ].join(" "), {
      status: "recipient_lookup",
      retryable: true,
      attemptedRecipient: unknownRecipient[1],
      organization: context.organization,
    });
  }
  return refusalResult(error);
}

type LaunchDepartmentInputFailureStatus = "launch_department_input_invalid";

/** Expected restored-call repair, raised before any lifecycle authority is read. */
class LaunchDepartmentInputError extends Error {
  readonly status: LaunchDepartmentInputFailureStatus = "launch_department_input_invalid";

  constructor(message: string) {
    super(message);
    this.name = "LaunchDepartmentInputError";
  }
}

const LAUNCH_DEPARTMENT_INPUT_GUIDANCE = [
  "org_launch_department requires exactly one create or resume shape:",
  "use { department, parentUnitId? } or { unitId, parentUnitId? },",
  "or wrap that complete shape once as { department: { department|unitId, parentUnitId? } }.",
  "Remove mixed fields and any organization, caller, personId, socket, or session claim, then retry once; no department was changed.",
].join(" ");

const LAUNCH_DEPARTMENT_IDENTITY_FIELDS = new Set([
  "organization",
  "caller",
  "personId",
  "socket",
  "session",
]);

function launchDepartmentInputError(): never {
  throw new LaunchDepartmentInputError(LAUNCH_DEPARTMENT_INPUT_GUIDANCE);
}

function launchDepartmentRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function launchDepartmentHasIdentityClaim(value: Record<string, unknown>): boolean {
  return Object.keys(value).some((key) => LAUNCH_DEPARTMENT_IDENTITY_FIELDS.has(key));
}

function hasOwn(value: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

/**
 * Repair a DOUBLE-ENCODED structured argument before TypeBox validates it.
 *
 * A live CEO called `org_add_department` and got
 * `- department: must be object`, with the received arguments showing
 * `"department": "{\"head\":..."` — the model had emitted the nested object as
 * a JSON *string*. An identical structure had parsed as an object moments
 * earlier, so the failure looks like an inconsistent tool binding; it is not.
 * Nothing on the path coerces a string into an object: the Pi runtime's
 * `Value.Convert` is a documented no-op for non-objects, and its primitive
 * coercion has no `object` case at all. The only tools that survived a fumble
 * were the ones carrying a `prepareArguments` seam, which Pi calls BEFORE
 * validation — `org_add_department` and `org_hire` had none.
 *
 * The repair is schema-guided rather than key-guided, so it fixes a nested
 * a `head` or a `people[]` entry for the same reason it fixes the top
 * level, and it can never touch a field the schema declares as a string. A
 * value that does not parse, or parses to a non-object, is returned untouched
 * so the normal refusal still fires. The advertised schema is unchanged: the
 * model is still told to send an object.
 *
 * # The repair is OBSERVABLE, because a silent one is where a regression hides
 *
 * `onRepair` receives the schema path of every value actually unwrapped, so the
 * seam is countable rather than invisible. A workaround nobody can measure
 * cannot be told apart from a product that never needed it: if a model starts
 * double-encoding on EVERY call, or a new tool ships with the wrong shape, this
 * would paper over it forever and no suite would ever go red. It is debug, not
 * warn, and it never refuses — the call still succeeds, it merely leaves a
 * trace. `""` is the path of the whole argument envelope.
 */
export function unwrapStringifiedArguments(
  schema: unknown,
  value: unknown,
  onRepair?: (path: string) => void,
  path = "",
): unknown {
  if (!schema || typeof schema !== "object") return value;
  const node = schema as { type?: string; properties?: Record<string, unknown>; items?: unknown };
  let current = value;
  if (typeof current === "string" && (node.type === "object" || node.type === "array")) {
    try {
      const parsed: unknown = JSON.parse(current);
      if (parsed === null || typeof parsed !== "object") return current;
      current = parsed;
      onRepair?.(path);
    } catch {
      return current;
    }
  }
  if (node.type === "object" && node.properties && current && typeof current === "object" && !Array.isArray(current)) {
    const out: Record<string, unknown> = { ...(current as Record<string, unknown>) };
    for (const [key, child] of Object.entries(node.properties)) {
      if (hasOwn(out, key)) {
        out[key] = unwrapStringifiedArguments(child, out[key], onRepair, path ? `${path}.${key}` : key);
      }
    }
    return out;
  }
  if (node.type === "array" && Array.isArray(current)) {
    return (current as unknown[]).map((entry, index) =>
      unwrapStringifiedArguments(node.items, entry, onRepair, `${path}[${index}]`));
  }
  return current;
}

/**
 * The `prepareArguments` seam for one tool, with its repairs traced.
 *
 * One factory rather than two closures so the two tools cannot drift into
 * logging the same event differently — the tool NAME is the only thing that
 * varies, and it is the field an operator would group by.
 */
function stringifiedArgumentRepair(
  context: OrganizationRuntimeContext,
  toolName: string,
  schema: unknown,
): (input: unknown) => unknown {
  return (input: unknown): unknown => {
    const repaired: string[] = [];
    const value = unwrapStringifiedArguments(schema, input, (at) => repaired.push(at || "<arguments>"));
    if (repaired.length) {
      appendOrganizationLogLine(context, "intercom", "tool-arguments-unwrapped", "debug", {
        tool: toolName,
        paths: repaired,
      });
    }
    return value;
  };
}

/**
 * Pi calls this seam before TypeBox validation. The advertised API remains the
 * flat create/resume union, while one historical/native department envelope is
 * flattened into that canonical contract. Classification is deliberately
 * strict: request data can select a definition, existing id, and optional
 * parent, but organization/caller/runtime authority always comes from context.
 */
function prepareLaunchDepartmentArguments(input: unknown): {
  parentUnitId?: unknown;
  department?: unknown;
  unitId?: unknown;
} {
  if (!launchDepartmentRecord(input)) return launchDepartmentInputError();
  const outer = input;
  if (launchDepartmentHasIdentityClaim(outer)) return launchDepartmentInputError();

  const departmentValue = outer.department;
  const nestedEnvelope = launchDepartmentRecord(departmentValue)
    && Object.keys(departmentValue).some((key) =>
      key === "department"
      || key === "unitId"
      || key === "parentUnitId"
      || LAUNCH_DEPARTMENT_IDENTITY_FIELDS.has(key));

  if (nestedEnvelope) {
    // A compatibility envelope is the whole request. Never merge it with flat
    // siblings, even when two supplied parent values happen to be equal.
    if (Object.keys(outer).some((key) => key !== "department")) return launchDepartmentInputError();
    if (launchDepartmentHasIdentityClaim(departmentValue)) return launchDepartmentInputError();
    if (Object.keys(departmentValue).some((key) => !["department", "unitId", "parentUnitId"].includes(key))) {
      return launchDepartmentInputError();
    }
    const create = hasOwn(departmentValue, "department");
    const resume = hasOwn(departmentValue, "unitId");
    if (create === resume) return launchDepartmentInputError();
    if (create
      && launchDepartmentRecord(departmentValue.department)
      && launchDepartmentHasIdentityClaim(departmentValue.department)) return launchDepartmentInputError();
    return {
      ...(hasOwn(departmentValue, "parentUnitId") ? { parentUnitId: departmentValue.parentUnitId } : {}),
      ...(create ? { department: departmentValue.department } : { unitId: departmentValue.unitId }),
    };
  }

  if (Object.keys(outer).some((key) => !["department", "unitId", "parentUnitId"].includes(key))) {
    return launchDepartmentInputError();
  }
  const create = hasOwn(outer, "department");
  const resume = hasOwn(outer, "unitId");
  if (create === resume) return launchDepartmentInputError();
  return input;
}

/* TOMBSTONE (chief-home-is-cwd §3/§4e): the whole hire-resource preflight —
 * `HireInputError`, `HireResourceKind`, `RESOURCE_CATALOG_ROUTE`,
 * `selectedHireResources` and `preflightHireResources`. It read
 * `/v1/org/resource-catalog/read` before any staffing mutation and refused a
 * hire naming a skill, extension or package id the company had not installed.
 * A hire names none: the skills an agent has are the files in the company
 * directory's `.pi/skills`, which Pi discovers and loads through one symlink,
 * so there is no selection to validate and no catalog route left to ask. */

/**
 * The applied answer of `/v1/org/staffing/lifecycle`, as a tool result.
 *
 * The route runs the activity transition and the structural mutation in one
 * call, then wakes chiefd's reconcile; it reports `handoff` as `completed` or
 * `abandoned`, and a `transitionId` when a transition was written at all
 * (an unattended offboard of a person with no live pane has none).
 *
 * `awaiting_handoff` is deliberately absent: the blocking fence that produced
 * it was deleted from chiefd ("a finished person moves immediately"), so a
 * card branch for it would be a state no route can now return.
 */
function staffingLifecycleResult(
  organization: string,
  action: string,
  personId: string,
  wire: Record<string, unknown>,
) {
  const handoff = typeof wire.handoff === "string" ? wire.handoff : undefined;
  const transitionId = typeof wire.transitionId === "string" ? wire.transitionId : undefined;
  // Same rule as the department create: `/v1/org/staffing/lifecycle` answers
  // `applied: true` once the mutation is durable and reports anything that went
  // wrong AFTER that in `warnings`. Dropping them would leave the honesty the
  // route paid for stranded one layer below the manager who needs it.
  const warning = routeWarnings(wire.warnings).join(" ") || undefined;
  return toolResult(true, `${STAFFING_LIFECYCLE_APPLIED_LABELS[action] ?? action} @${displayHandle(organization, personId)}.${warning ? `\n${warning}` : ""}`, {
    action,
    personId,
    status: "applied",
    ...(handoff ? { handoff } : {}),
    ...(transitionId ? { transitionId } : {}),
    ...(warning ? { warning } : {}),
    structuralChanged: wire.structuralChanged === true,
  });
}

/** Past-tense wording for each lifecycle verb, so a card never prints the raw
 *  internal verb at a manager (#360). */
const STAFFING_LIFECYCLE_APPLIED_LABELS: Readonly<Record<string, string>> = {
  offboard: "Offboarded",
};

/**
 * Execute the agent-facing permanent-transfer decision after the tool has been
 * selected. Exported for the narrow refusal regression tests: a rejected
 * operation must not reach the runtime reconciler or legacy lifecycle CLI,
 * because it has not committed a durable placement.
 */
export async function executeAtomicPersonTransfer(
  context: OrganizationRuntimeContext,
  params: { personId: string; departmentId: string; vacates?: ChiefdHeadVacancy },
  caller: AtomicPersonTransferCaller = chiefdAtomicPersonTransfer,
) {
  const manifestWire = await readIntercomManifestWire(context);
  const manifest = manifestWire.manifest;
  const managerPerson = currentPerson(context, manifest);
  const target = manifest.people[params.personId];
  // SCOPE, and nothing else — the same answer `requireManagedTarget` gives to
  // the same question, in the same words. `manager(managerPerson)` — a kind of
  // `executive` or `head` — stood in this conjunction and decided nothing:
  // `departmentIsInScope` is already false for a non-executive who heads no
  // department, so every caller the title half would have refused is refused
  // here anyway. The one state where it could change the answer is a manifest
  // chiefd never writes — a head recorded as a worker — and there it refused a
  // person their OWN subtree.
  //
  // It is deleted rather than kept because the operator ruling of 2026-08-13
  // (`AGENTS.md`) forbids it outright: authority over structure is the subtree
  // you head, never the job title, so the refusal names the missing management
  // relation and no role. chiefd re-checks the same relation on
  // `/v1/org/person/transfer`; this is a pre-flight, never the authority.
  // The same split `requireManagedTarget` makes, and for the same reason: an
  // absent person is a missing SUBJECT, not a missing permission. One sentence
  // for both sent a CEO hunting authority it already held.
  if (!target) {
    throw new CallerRefusal(
      `no person '${params.personId}' exists in this company — this is not an authority refusal.`,
    );
  }
  if (!departmentIsInScope(manifest, managerPerson, target.departmentId)) {
    throw new CallerRefusal(
      `'${managerPerson.id}' does not manage person '${params.personId}': authority is the ` +
        `subtree you head, and '${params.personId}' sits in '${target.departmentId}'.`,
    );
  }
  const transferDenial = departmentScopeDenial(manifest, managerPerson, params.departmentId);
  if (transferDenial === "unknown-department") {
    throw new CallerRefusal(unknownDepartmentMessage(manifest, managerPerson, params.departmentId, "transfer into"));
  }
  if (transferDenial) {
    throw new Error(`Permanent transfer target '${params.departmentId}' is outside '${managerPerson.id}' management scope`);
  }
  const result = await caller(chiefdEndpoint(context), {
    slug: manifestWire.key,
    personId: params.personId,
    destinationId: params.departmentId,
    intent: `person-transfer:${params.personId}`,
    actor: managerPerson.id,
    // A head moving out leaves its department without one. WHICH answer applies
    // is chiefd's refusal to make, exactly as on the create path: this only
    // carries the caller's decision through.
    ...(params.vacates ? { vacates: params.vacates } : {}),
  });
  if ("refused" in result) {
    return toolResult(false, `Transfer refused: ${result.detail}`, {
      status: "refused", code: result.refused, retryable: false, personId: params.personId,
    });
  }
  const warning = await reconcileRuntime(context, [params.personId]);
  return toolResult(true, `Transferred @${displayHandle(context.organization, params.personId)} to ${params.departmentId}.${warning ? `\n${warning}` : ""}`, {
    status: "applied", personId: params.personId, moved: result.moved, warning,
  });
}

/**
 * `org_escalate_to_operator`, and nothing else.
 *
 * It goes to the STRUCTURAL ROOT alone — the person whose direct manager
 * resolves to undefined — because every other person escalates to their own
 * manager, and a tool with no valid recipient is worse than no tool. That is a
 * question about the SHAPE OF THE TREE above the caller, not about what the
 * caller IS, and the probe below answers it directly rather than through a
 * kind.
 *
 * This function used to hold three more tools whose handlers refused a
 * non-manager outright. They are gone: every one of them is now fenced
 * server-side, so they moved to `installSubtreeTools` with the rest of the
 * verbs that are decided by scope. Nothing here asks a job title any more, and
 * the last `manager()` call site went with them.
 */
async function installRootExecutiveTools(
  pi: OrganizationToolRegistrar,
  context: OrganizationRuntimeContext,
): Promise<void> {
  // Only the structural root (a person whose direct manager resolves to
  // itself/undefined) has no manager to escalate a human-only blocker to, so
  // only it gets `org_escalate_to_operator`. Every other manager escalates to
  // its own manager, and registering the tool for them would re-open the same
  // "there is no valid recipient" trap this fix closes.
  // #270: this gate is computed ONCE at install/boot. The old code collapsed a
  // TRANSIENT manifest-read failure (a slow / unknown-company chiefd, a docstore
  // blip) into the same silent `false` as a GENUINE non-root person — so any
  // read error at boot permanently withheld the CEO-only tool for this
  // process's whole life with ZERO observability (found live #270/#269). The
  // probe below distinguishes them: the manifest LOAD is retried a few times;
  // a genuine non-root (manifest loads, the person has a manager) stays
  // silently tool-less as before (correct — a department head must not see
  // it); but a persistent LOAD failure is logged LOUDLY and the tool is
  // registered DEFENSIVELY (fail OPEN toward the almost-certain CEO caller —
  // their own execute path still refuses a genuine non-root, so a
  // mis-registration is harmless, whereas silently locking out the real CEO is
  // a real operator-facing bug).
  const structuralRootProbe = await resolveInstallerStructuralRoot(context);
  if (structuralRootProbe.readFailed) {
    logOrganizationException(context, "structural-root-gate", structuralRootProbe.error, {
      detail: "structural-root check failed after retries; org_escalate_to_operator registered defensively — likely a transient manifest read against a slow/unknown-company chiefd. Reload the session if a non-executive is unexpectedly seeing this tool.",
      attempts: structuralRootProbe.attempts,
    });
  }
  const installerIsStructuralRoot = structuralRootProbe.isRoot || structuralRootProbe.readFailed;
  if (installerIsStructuralRoot) {
    pi.registerTool({
      name: "org_escalate_to_operator",
      label: "Escalate a blocker to the human operator",
      description: "You are the organization's top-level executive with no manager to escalate to. When your work is blocked on something only a human operator outside the organization can resolve (an approval, a credential, a real-world action), record it here — do NOT try to message a person named \"launcher\" or any recipient outside the organization. This durably reaches the operator out of band. After recording, keep working. Re-recording the identical blocker is a safe no-op.",
      parameters: Type.Object({
        blocker: Type.String({ minLength: 1, maxLength: 600 }),
        operatorAction: Type.String({ minLength: 1, maxLength: 300 }),
      }),
      async execute(_toolCallId, params) {
        try {
          const manifest = await loadIntercomOrganization(context);
          const sender = currentPerson(context, manifest);
          if (directManagerId(manifest, sender) !== undefined) {
            throw new CallerRefusal("Only the organization's top-level executive, which has no manager to escalate to, may escalate to the human operator");
          }
          const blocker = params.blocker.trim();
          const operatorAction = params.operatorAction.trim();
          if (!blocker) throw new CallerRefusal("An operator escalation needs a concrete blocker");
          if (!operatorAction) throw new CallerRefusal("An operator escalation needs the exact operator action required");
          const intent = await queueOperatorEscalationIntent(context, sender.id, blocker, operatorAction);
          return toolResult(true, "Escalation recorded durably for the human operator. It reaches them out of band; keep working.", {
            status: "queued",
            fingerprint: intent.fingerprint,
            blocker,
            operatorAction,
          });
        } catch (error) { return refusalResult(error); }
      },
      renderCall(args, theme) {
        const blocker = compactPresentation(String(args.blocker || "a blocker"), 84);
        return renderOperatorEscalationCard(theme, {
          kind: "tool-call", icon: domainIcon("🚨"), inProgress: true,
          title: "Escalating to operator", target: `${blocker.text}${blocker.truncated ? "…" : ""}`,
          body: { kind: "none" }, boxed: false,
        });
      },
      renderResult(result, { expanded }, theme) {
        const detail = (result.details ?? {}) as { ok?: boolean; blocker?: string; operatorAction?: string; fingerprint?: string };
        if (!detail.ok) {
          return renderOperatorEscalationCard(theme, {
            kind: "tool-failure", icon: "failure", title: "Escalation not recorded",
            body: { kind: "prose", text: toolOutputText(result), previewChars: 120 }, boxed: false,
          }, { expanded: expanded === true });
        }
        const blocker = compactPresentation(detail.blocker || "the blocker", 96);
        const action = detail.operatorAction ? compactPresentation(detail.operatorAction, 96) : undefined;
        return renderOperatorEscalationCard(theme, {
          kind: "tool-success", icon: "success", title: "Escalated to operator",
          target: `${blocker.text}${blocker.truncated ? "…" : ""}`,
          detail: action ? `Needs: ${action.text}${action.truncated ? "…" : ""}` : undefined,
          body: expanded && detail.fingerprint
            ? { kind: "lines", lines: [{ text: "" }, { text: `ref ${detail.fingerprint}`, token: "dim" }] }
            : { kind: "none" },
          boxed: false,
        });
      },
    });

    /**
     * THE OPERATOR'S STAND-DOWN, reachable from where the operator actually is.
     *
     * A live company was told, in one message to its CEO: "STOP ALL WORK NOW. Do
     * not create departments, do not hire, do not message anyone, do not start or
     * recall anyone. Tell every person to stop immediately and park all of them
     * except yourself. Then stay idle and do nothing until I ask."
     *
     * The CEO obeyed perfectly. It stopped and parked six people, reported it,
     * and then refused two inbound messages on principle. Forty-five seconds
     * later all six were back up with fresh panes and brand-new contexts, because
     * the mail they had queued to each other re-granted every one of them.
     *
     * Stopping people one at a time is not a stand-down, and no number of
     * `org_stop_person` calls adds up to one: each is a decision about a PERSON,
     * and nothing about it says the COMPANY should not work. This tool writes the
     * durable company-level state that does, and chiefd then refuses every path
     * that would start anybody until it is lifted.
     *
     * Only the CEO can call it. The route asks whether the caller heads the root
     * department, which is the same subtree question every other verb asks — this
     * write reaches every person in the company.
     */
    pi.registerTool({
      name: "org_stand_down",
      label: "Stand the whole company down",
      description: "STOP ALL WORK, and keep it stopped. Use this when your operator tells you to stop everything, stand down, or do nothing until they ask — never a series of org_stop_person calls, which stop people one at a time and do not stop the COMPANY. Every person except you is stopped and STAYS stopped: chiefd refuses every start, wake, hire and automatic message-wake until org_resume. You keep running so your operator can still talk to you. Nobody's messages are lost — queued mail is held and delivered the moment the company resumes. After calling this, stay idle: do not create departments, do not hire, do not message anyone.",
      parameters: Type.Object({
        reason: Type.Optional(Type.String({ description: "Why the company is stopping, in your operator's own words where you have them. Shown back in every refusal." })),
      }),
      async execute(_toolCallId, params) {
        const gate = await staffingAuthority(context)
        const outcome = await staffingApply(
          gate,
          "/v1/org/stand-down",
          { slug: gate.slug, at: new Date().toISOString(), reason: params.reason?.trim() ?? "" },
          { action: "stand-down", reason: params.reason?.trim() || undefined },
        )
        if ("refused" in outcome) return routeRefusal("Stand-down", outcome)
        return toolResult(
          true,
          "The company is stood down. Everyone except you is stopped and stays stopped; nothing starts anyone until org_resume. Queued mail is held, not lost. Stay idle now.",
          { status: "applied" },
        )
      },
    })

    pi.registerTool({
      name: "org_resume",
      label: "Let the company work again",
      description: "Lift a stand-down, so the company can work again. Call this only when your operator asks you to resume — a stand-down is their decision, not yours. People do not all come back at once: everyone whose messages were held while the company was stopped is started by that held mail, and anybody else you still need is a separate org_start_person.",
      parameters: Type.Object({}),
      async execute() {
        const gate = await staffingAuthority(context)
        const outcome = await staffingApply(
          gate,
          "/v1/org/stand-down/clear",
          { slug: gate.slug, at: new Date().toISOString() },
          { action: "resume" },
        )
        if ("refused" in outcome) return routeRefusal("Resume", outcome)
        return toolResult(
          true,
          "The company is working again. People whose mail was held while it was stopped come back on the next pass; start anybody else you need explicitly.",
          { status: "applied" },
        )
      },
    })
  }
}

/**
 * The subtree-growth surface: `ORGANIZATION_SUBTREE_TOOL_NAMES`, registered for
 * EVERY person whatever their kind.
 *
 * The catalog says every person carries these verbs, and every handler below
 * checks SUBTREE SCOPE rather than a job title — a leaf refuses today and
 * succeeds the moment it heads a unit, which is a state and not a permanent
 * condition. The catalog was split for exactly that reason; this registration
 * gate was not split with it, so a worker's pane carried none of them and the
 * documented rule "every leaf can become a parent" was unreachable in the one
 * place it had to work.
 *
 * Registration is not authority. A tool present and then refused by scope is
 * the safety model working; a tool absent is the bug.
 */
async function installSubtreeTools(
  pi: OrganizationToolRegistrar,
  context: OrganizationRuntimeContext,
  // The manager's own live model registry, read at call time rather than
  // captured: a tool registered before the first session must still see the
  // registry the session brings.
  modelRegistry: () => ExtensionContext["modelRegistry"] | undefined = () => undefined,
): Promise<void> {
  pi.registerTool({
    name: "org_launch_department",
    label: "Launch or resume a department",
    description: "RESUME one existing stopped department by id: org_launch_department({ unitId }). To CREATE a department, prefer org_add_department, whose arguments are flat and which also accepts an existing person as the head — this tool's create shape nests the whole definition under `department` and exists for callers that already send it. Initial staff commit atomically and come up with the department; they stop on their own once they settle after idling. Omit parentUnitId to use your natural root (CEO: executive; head: own department).",
    // #1011: ONE object. See `org_maintain_session` above. The create/resume
    // exclusivity the union's two arms expressed is NOT lost with them: it was
    // already enforced by `prepareLaunchDepartmentArguments`, which refuses
    // both-or-neither with a named error, and which `execute` re-runs on every
    // call so a trusted caller bypassing Pi's `prepareArguments` seam gets the
    // same refusal.
    parameters: Type.Object({
      parentUnitId: Type.Optional(Type.String({ description: "Optional parent department id; omit for your own management root" })),
      department: Type.Optional(DEPARTMENT_DEFINITION),
      unitId: Type.Optional(Type.String({ description: "Existing stopped department id. Pass exactly one of department (create) or unitId (resume)." })),
    }, { additionalProperties: false }),
    // `prepareLaunchDepartmentArguments` deliberately returns the loosely
    // shaped, envelope-unwrapped record described by its own return type —
    // `execute` below re-casts `compatible.department` explicitly before use,
    // `execute` re-casts `compatible.department` explicitly before use.
    prepareArguments: prepareLaunchDepartmentArguments as never,
    async execute(_toolCallId, params) {
      try {
        // Trusted/direct callers can bypass Pi's prepareArguments seam. Apply
        // the same classifier here before reading authority or invoking the
        // launcher so malformed restored calls remain mutation-free.
        const compatible = prepareLaunchDepartmentArguments(params);
        const create = hasOwn(compatible, "department");
        const result = await runManagedUnitLifecycle(context, {
          kind: "department",
          action: "launch",
          parentUnitId: await launchParentUnitId(context, compatible.parentUnitId),
          ...(create
            ? { spec: compatible.department }
            : { unitId: compatible.unitId as string }),
        });
        return toolResult(true, result.message, result);
      } catch (error) {
        if (error instanceof LaunchDepartmentInputError) {
          return toolResult(false, error.message, { status: error.status, retryable: true });
        }
        return lifecycleFailure(error);
      }
    },
  });

  pi.registerTool({
    name: "org_launch_contract",
    label: "Launch or resume a contract",
    description: "Create one bounded transient contract under a managed parent, or resume one existing stopped contract by id. Engagement metadata is required.",
    parameters: Type.Object({
      parentUnitId: Type.String(),
      contract: Type.Optional(CONTRACT_DEFINITION),
      unitId: Type.Optional(Type.String({ description: "Existing stopped contract id; mutually exclusive with contract" })),
    }),
    async execute(_toolCallId, params) {
      try {
        const result = await runManagedUnitLifecycle(context, {
          kind: "contract",
          action: "launch",
          parentUnitId: params.parentUnitId,
          spec: params.contract,
          unitId: params.unitId,
        });
        return toolResult(true, result.message, result);
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  const unitEnd = (
    name: "org_stop_department" | "org_remove_department" | "org_stop_contract" | "org_remove_contract",
    kind: ManagedUnitKind,
    action: "stop" | "remove",
  ) => pi.registerTool({
    name,
    label: `${action === "stop" ? "Stop" : "Remove"} a ${kind}`,
    description: action === "stop"
      ? `Stop one managed ${kind}. Context and disk state are retained, and every live person must complete the bounded handoff before retry succeeds.`
      : `Permanently remove one managed ${kind} and everything under it. Everyone homed under it is FIRED — offboarded exactly as org_offboard offboards, keeping their record, history and audit trail, re-homed to the parent unit as departed. Use stop when future resume is possible.`,
    parameters: Type.Object({
      unitId: Type.String(),
      ...(action === "remove"
        ? { confirmImpact: Type.Optional(Type.Boolean({ description: "Acknowledge that this permanently fires the head and every member of the subtree. They are offboarded, not deleted: each keeps their record and audit trail. Required when the unit has members beyond its head." })) }
        : {}),
    }),
    async execute(_toolCallId, params) {
      try {
        // org-ops R2 — delete-department UX contract. A head-only unit removes
        // in one call, no extra prompt. A unit with members is an impactful,
        // irreversible fire-everyone: the first call WITHOUT `confirmImpact`
        // refuses, naming the exact blast radius and the two resolutions
        // (confirm, or move the people out first). The prompt lives in the tool
        // surface (a refusal that names the missing decision), never in
        // free-form model behavior.
        if (action === "remove" && kind === "department") {
          const confirmImpact = (params as { confirmImpact?: boolean }).confirmImpact === true;
          if (!confirmImpact) {
            const manifest = await loadIntercomOrganization(context);
            // org_remove_contract routes here with kind "contract" and is NOT
            // gated (guarded above), so a wrong-kind id still reaches the
            // lifecycle's own kind/existence validation.
            if (manifest.departments[params.unitId]) {
              const impact = intercomUnitRemovalImpact(manifest, params.unitId);
              if (impact.memberPersonIds.length > 0) {
                const head = impact.headPersonId ? manifest.people[impact.headPersonId]?.name ?? impact.headPersonId : "no head";
                const count = impact.memberPersonIds.length;
                return toolResult(
                  false,
                  `Removing this ${kind} also fires its head (${head}) + ${count} member${count === 1 ? "" : "s"}: ${impact.memberNames.join(", ")}. `
                  + `To proceed and fire them all, call again with confirmImpact: true — each keeps their record and audit trail, exactly as org_offboard leaves a departed person. `
                  + `To keep them employed, first move them out (org_move_department_members, or org_transfer per person) and then remove the empty ${kind}.`,
                  { status: "refused", unitId: params.unitId, kind, impact },
                );
              }
            }
          }
        }
        const result = await runManagedUnitLifecycle(context, {
          kind,
          action,
          unitId: params.unitId,
        });
        return toolResult(true, result.message, result);
      } catch (error) { return lifecycleFailure(error); }
    },
  });
  unitEnd("org_stop_department", "department", "stop");
  unitEnd("org_remove_department", "department", "remove");
  unitEnd("org_stop_contract", "contract", "stop");
  unitEnd("org_remove_contract", "contract", "remove");

  // Backward-compatible names deliberately route to the same public runtime
  // lifecycle. They never fall through to disk-only `org department` writes.
  pi.registerTool({
    name: "org_add_department",
    label: "Add an organization department",
    description: "Create a durable department — ONLY when the operator asked for one. A head-shaped TITLE is not that request: somebody titled \"Chief of Staff\" is hired into an existing department with org_hire. Arguments are FLAT — no wrapper — and every object is real JSON, never a quoted string. Example: {\"name\":\"Engineering\",\"purpose\":\"Ship it.\",\"head\":{\"name\":\"Ada\",\"title\":\"Head of Engineering\",\"mandate\":\"Own delivery.\"}}. A department cannot be headless: give it a NEW head (head) or an EXISTING one (existingHeadPersonId) — exactly one. This call CREATES the new head; do not hire them first. Naming an existing person MOVES them into the department they will head; if they ALREADY head one, also send vacates — hand it to a member, or dissolve it if they are its last. \"Its head reports to Carlos\" is STRUCTURAL: set parentDepartmentId to the department Carlos heads, never your own root. If Carlos heads none yet, that is the FIRST call, not a blocker: create one for him with existingHeadPersonId, then create this beneath it.",
    parameters: ADD_DEPARTMENT_PARAMETERS,
    prepareArguments: stringifiedArgumentRepair(context, "org_add_department", ADD_DEPARTMENT_PARAMETERS) as never,
    async execute(_toolCallId, params) {
      try {
        // org-ops R3 — the head decision is REQUIRED and explicit. A department
        // cannot be headless, and the model must never silently invent a head:
        // exactly one of `head` (hire new) or `existingHeadPersonId` (promote an
        // existing person). A call with neither, or both, is refused naming the
        // exact missing decision (the prompt lives in the tool surface).
        const hasHire = params.head !== undefined;
        const hasExisting = typeof params.existingHeadPersonId === "string" && params.existingHeadPersonId.length > 0;
        if (hasHire === hasExisting) {
          return toolResult(
            false,
            `Creating a department needs exactly one head decision: either hire a NEW person to lead it (head), or name an EXISTING person to lead it (existingHeadPersonId). ${hasHire ? "You gave both — pick one." : "You gave neither — pick one."}`,
            { status: "refused" },
          );
        }
        if (hasExisting && (params.staff?.length ?? 0) > 0) {
          return toolResult(
            false,
            "A department created with an existing head takes no initial staff. Create it first, then add staff with org_hire or move people in with org_transfer / org_move_department_members.",
            { status: "refused" },
          );
        }
        // WHICH vacancy answer applies is chiefd's decision, never this file's.
        // A pre-flight copy here — walking the tree for the vacated unit and
        // its eligible successors — would be a SECOND statement of a rule
        // chiefd already owns, and two statements of one rule drifting apart is
        // the exact defect this packet exists to close. chiefd refuses naming
        // the department and the members who may take it. The only thing kept
        // local is SHAPE: a hand-over that names nobody is a malformed payload,
        // not a policy question.
        if (params.vacates && !hasExisting) {
          return toolResult(
            false,
            "vacates says what becomes of a department the new head ALREADY leads, so it belongs only with existingHeadPersonId. A newly hired head leads nothing yet.",
            { status: "refused" },
          );
        }
        const vacancy = normalizeHeadVacancy(params.vacates, "vacates");
        if ("refusal" in vacancy) return toolResult(false, vacancy.refusal, { status: "refused" });
        const vacates = vacancy.value;
        const result = await runManagedUnitLifecycle(context, {
          kind: "department",
          action: "launch",
          parentUnitId: await launchParentUnitId(context, params.parentDepartmentId),
          // The flat arguments are re-gathered into the spec the create route
          // takes. Building it field by field — rather than forwarding the
          // whole params object — keeps `parentDepartmentId`, `vacates` and
          // `existingHeadPersonId`, which are arguments about the CALL and not
          // about the department, out of the department definition.
          spec: hasExisting
            ? { id: params.departmentId, name: params.name, purpose: params.purpose }
            : {
                id: params.departmentId,
                name: params.name,
                purpose: params.purpose,
                head: params.head as unknown as Record<string, unknown>,
                ...(params.staff ? { staff: params.staff as unknown as Array<Record<string, unknown>> } : {}),
              },
          ...(hasExisting ? { existingHeadPersonId: params.existingHeadPersonId } : {}),
          ...(vacates ? { vacates } : {}),
        });
        return toolResult(true, result.message, result);
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  const departmentState = (
    name: "org_pause_department" | "org_resume_department",
    action: "pause" | "resume",
  ) => pi.registerTool({
    name,
    label: `${action === "pause" ? "Pause" : "Resume"} an organization department`,
    description: action === "pause"
      ? "Compatibility alias for org_stop_department. It retains state and waits for each live person's bounded handoff."
      : "Compatibility alias for org_launch_department's resume shape: it brings one stopped department back.",
    parameters: Type.Object({
      departmentId: Type.String({
        description: "The department to act on. The company name or slug is NEVER a department id — the root department's id is in org_roster.",
      }),
    }),
    async execute(_toolCallId, params) {
      try {
        const managed = await managedUnit(context, params.departmentId, "department");
        const result = await (action === "pause"
          ? runManagedUnitLifecycle(context, {
            kind: "department", action: "stop", unitId: params.departmentId,
          })
          : runManagedUnitLifecycle(context, {
            kind: "department",
            action: "launch",
            unitId: params.departmentId,
            parentUnitId: managed.unit.parentDepartmentId,
          }));
        return toolResult(true, result.message, result);
      } catch (error) { return lifecycleFailure(error); }
    },
  });
  departmentState("org_pause_department", "pause");
  departmentState("org_resume_department", "resume");

  /**
   * Move a department WHOLE -- head, members, and sub-departments -- under a
   * new parent. It repositions a department head without moving a person at
   * all: the head keeps heading their department, so nobody is separated from
   * anybody. Deliberately NOT a person-level verb -- `org_transfer` moves ONE
   * person, and moving a head that way costs an answer about the department
   * they leave (`vacates`), which is the whole difference between the two.
   *
   * ChiefD serializes this structural decision against current normalized rows,
   * so the model supplies no mutable counter and never retries a stale snapshot.
   */
  pi.registerTool({
    name: "org_reparent_department",
    label: "Move an organization department",
    description: "Move a department -- with its head, its members, and everything under it -- to sit beneath a different parent department. Use this to reorganize the company tree: it is the only way to move a head that keeps them heading their department, and nobody is separated from anybody. Department ids never change. To move ONE person instead, use org_transfer; a head moved that way must also say what becomes of the department they leave (vacates).",
    parameters: Type.Object({
      departmentId: Type.String({
        description: "The department to move, with its whole subtree. The company name or slug is NEVER a department id — the root department's id is in org_roster.",
      }),
      newParentDepartmentId: Type.String({ description: "The department it should sit beneath afterwards" }),
    }),
    async execute(_toolCallId, params) {
      try {
        const gate = await staffingAuthority(context);
        // Both ends must be inside this manager's scope: moving a department
        // you manage UNDER one you do not is still a reorganization of
        // somebody else's company.
        for (const departmentId of [params.departmentId, params.newParentDepartmentId]) {
          // Say WHICH check failed: "outside your scope" for a department that
          // does not exist at all sends the caller hunting a permissions
          // problem they do not have (gh#498's class).
          if (!gate.manifest.departments[departmentId]) {
            throw new CallerRefusal(unknownDepartmentMessage(gate.manifest, gate.person, departmentId, "move"));
          }
          if (!departmentIsInScope(gate.manifest, gate.person, departmentId)) {
            throw new Error(`Department '${departmentId}' is outside '${gate.person.id}' management scope`);
          }
        }
        // The tree changed shape and the live fleet must follow -- but that is
        // chiefd's reconcile, woken by the route itself, not a second
        // convergence run from this pane. The org event records WHO did it and
        // WHAT they did; nobody is asked to justify a structural change.
        const outcome = await staffingApply(gate, "/v1/org/department/reparent", {
          slug: gate.slug,
          departmentId: params.departmentId,
          newParentId: params.newParentDepartmentId,
        }, { action: "reparent-department", departmentId: params.departmentId, newParentDepartmentId: params.newParentDepartmentId });
        if ("refused" in outcome) return routeRefusal("Department move", outcome, { departmentId: params.departmentId });
        return toolResult(true, `Moved department '${params.departmentId}' under '${params.newParentDepartmentId}'.`, {
          status: "applied",
          departmentId: params.departmentId,
          newParentDepartmentId: params.newParentDepartmentId,
        });
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  // org-ops R5 — move only the PEOPLE of a department (not the department
  // itself) to another node, in ONE atomic operation.
  pi.registerTool({
    name: "org_move_department_members",
    label: "Move a department's people",
    description: "Move every ordinary member of one department to another department in ONE atomic change — the source department's HEAD stays (it is never left headless), and departed people are left in place. Use this to relocate a team's workers without moving the department itself (for the whole department incl. its head, use org_reparent_department). To move the head too, move them with org_transfer and say what becomes of the department they head (vacates: hand it to one of its members, or dissolve it when they are its last one), or appoint a successor first with org_appoint_department_head. Optionally delete the now head-only source afterward with thenDelete.",
    parameters: Type.Object({
      fromDepartmentId: Type.String({ description: "The department whose members move out" }),
      toDepartmentId: Type.String({ description: "The department the members move into" }),
      thenDelete: Type.Optional(Type.Boolean({ description: "After moving the members, delete the now head-only source department (offboards its head). Default false." })),
    }),
    async execute(_toolCallId, params) {
      try {
        const gate = await staffingAuthority(context);
        for (const departmentId of [params.fromDepartmentId, params.toDepartmentId]) {
          if (!gate.manifest.departments[departmentId]) {
            throw new CallerRefusal(unknownDepartmentMessage(gate.manifest, gate.person, departmentId, "move people between"));
          }
          if (!departmentIsInScope(gate.manifest, gate.person, departmentId)) {
            throw new Error(`Department '${departmentId}' is outside '${gate.person.id}' management scope`);
          }
        }
        // No `personIds`: the set this tool promises -- ordinary members, never
        // the head, never somebody departed -- is DERIVED by chiefd
        // inside the moving transaction. Enumerating it here would need a
        // second copy of "who is an ordinary member" and would read it from a
        // manifest already one commit stale by the time the batch lands.
        const moved = await staffingApply(gate, "/v1/org/department/move-members", {
          slug: gate.slug,
          fromDepartmentId: params.fromDepartmentId,
          destinationId: params.toDepartmentId,
        }, { action: "move-department-members", fromDepartmentId: params.fromDepartmentId, toDepartmentId: params.toDepartmentId });
        if ("refused" in moved) return routeRefusal("Member move", moved, { fromDepartmentId: params.fromDepartmentId });
        const movedPersonIds = Array.isArray(moved.wire.moved)
          ? moved.wire.moved.filter((entry): entry is string => typeof entry === "string")
          : [];
        const movedMessage = `Moved ${movedPersonIds.length} ${movedPersonIds.length === 1 ? "person" : "people"} from '${params.fromDepartmentId}' to '${params.toDepartmentId}'.`;
        if (!params.thenDelete) {
          return toolResult(true, movedMessage, { status: "applied", movedPersonIds, fromDepartmentId: params.fromDepartmentId, toDepartmentId: params.toDepartmentId });
        }
        // thenDelete: the source is now head-only, so a delete fires just its
        // head (one member) — an impactful but explicitly-requested removal.
        const removed = await runManagedUnitLifecycle(context, {
          kind: "department", action: "remove", unitId: params.fromDepartmentId,
        });
        return toolResult(true, `${movedMessage}\n${removed.message}`, { status: "applied", movedPersonIds, removed });
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  /**
   * Appoint an existing member of a department as its head, demoting the
   * sitting head to an ordinary worker in the SAME department. Atomic, so the
   * department always has exactly one head and no person is created or removed
   * -- never a duplicate.
   *
   * It is NOT the only way a head moves any more, and the description no longer
   * says it is: `org_transfer` moves a head that answers `vacates` (hand the
   * department over, or dissolve it), and `org_offboard` fires one that names a
   * `successorPersonId`. What all three share is the invariant, not the tool —
   * a department is never left headless.
   */
  pi.registerTool({
    name: "org_appoint_department_head",
    label: "Appoint an organization department head",
    description: "Replace a department's head with an existing member of that department. First inspect the returned incumbent and ask the operator which disposition is intended: retain them in the department, transfer them to a named department, demote them to report to you, or offboard them. No replacement is written until you make that explicit generic decision. A department is never left headless, so a head only moves or leaves alongside an answer about their department: this tool names the successor, org_transfer takes vacates, and org_offboard takes successorPersonId.",
    parameters: Type.Object({
      departmentId: Type.String({
        description: "The department whose head is changing. The company name or slug is NEVER a department id — the root department's id is in org_roster.",
      }),
      newHeadPersonId: Type.String({ description: "An existing member of that department to promote to head" }),
      incumbentDisposition: Type.Optional(Type.Union([
        Type.Literal("retain", { description: "Keep the former head as an ordinary worker in this department" }),
        Type.Literal("transfer", { description: "Move the former head to incumbentDepartmentId as an ordinary worker" }),
        Type.Literal("demote", { description: "Move the former head to report to you in your own department" }),
        Type.Literal("offboard", { description: "End the former head's employment while appointing the successor atomically" }),
      ], { description: "Required after the incumbent decision has been confirmed with the operator" })),
      incumbentDepartmentId: Type.Optional(Type.String({ description: "Required only when incumbentDisposition is transfer" })),
    }),
    async execute(_toolCallId, params) {
      try {
        const gate = await staffingAuthority(context);
        const manifest = gate.manifest;
        const managerPerson = gate.person;
        if (!manifest.departments[params.departmentId]) {
          throw new CallerRefusal(unknownDepartmentMessage(manifest, managerPerson, params.departmentId, "act on"));
        }
        if (!departmentIsInScope(manifest, managerPerson, params.departmentId)) {
          throw new Error(`Department '${params.departmentId}' is outside '${managerPerson.id}' management scope`);
        }
        const department = manifest.departments[params.departmentId]!;
        const incumbent = manifest.people[department.headPersonId];
        if (!incumbent) throw new Error(`Department '${params.departmentId}' has no durable incumbent head`);
        if (!params.incumbentDisposition) {
          return toolResult(
            false,
            `Replacing ${incumbent.name} (@${displayHandle(context.organization, incumbent.id)}) as head of '${department.name}' needs an operator decision before any change. Ask whether to retain them here, transfer them to another department, demote them to report to you, or offboard them. Then call again with incumbentDisposition (and incumbentDepartmentId for transfer).`,
            {
              status: "incumbent_disposition_required",
              incumbent: {
                personId: incumbent.id,
                name: incumbent.name,
                title: incumbent.title,
                departmentId: department.id,
                employmentState: incumbent.employmentState,
              },
              allowedDispositions: ["retain", "transfer", "demote", "offboard"],
            },
          );
        }
        if (params.incumbentDisposition === "transfer" && !params.incumbentDepartmentId) {
          return toolResult(false, `Transferring incumbent ${incumbent.name} requires incumbentDepartmentId. Ask the operator which department should receive them.`, {
            status: "incumbent_transfer_destination_required",
            incumbent: { personId: incumbent.id, name: incumbent.name, departmentId: department.id },
          });
        }
        if (params.incumbentDisposition !== "transfer" && params.incumbentDepartmentId) {
          return toolResult(false, "incumbentDepartmentId is valid only with incumbentDisposition: transfer.", { status: "incumbent_disposition_invalid" });
        }
        if (params.incumbentDisposition === "offboard") {
          // Appoint the successor AND fire the incumbent in one atomic route:
          // the department is never headless for an instant in between.
          const outcome = await staffingApply(gate, "/v1/org/person/replace-head-and-offboard", {
            slug: gate.slug,
            headPersonId: incumbent.id,
            successorPersonId: params.newHeadPersonId,
          }, { action: "replace-head-and-offboard", departmentId: params.departmentId, headPersonId: incumbent.id, successorPersonId: params.newHeadPersonId });
          if ("refused" in outcome) return routeRefusal("Head replacement", outcome, { personId: incumbent.id });
          return toolResult(true, `Appointed @${displayHandle(context.organization, params.newHeadPersonId)} to head '${params.departmentId}' and offboarded @${displayHandle(context.organization, incumbent.id)}.`, {
            status: "applied", departmentId: params.departmentId, personId: incumbent.id, successorPersonId: params.newHeadPersonId,
          });
        }
        let demoteToDepartmentId: string | undefined;
        if (params.incumbentDisposition === "demote") {
          demoteToDepartmentId = managerPerson.departmentId && managerPerson.departmentId !== params.departmentId
            ? managerPerson.departmentId
            : undefined;
        } else if (params.incumbentDisposition === "transfer") {
          const destinationDepartmentId = params.incumbentDepartmentId!;
          if (!manifest.departments[destinationDepartmentId]) {
            throw new CallerRefusal(unknownDepartmentMessage(manifest, managerPerson, destinationDepartmentId, "transfer into"));
          }
          if (!departmentIsInScope(manifest, managerPerson, destinationDepartmentId)) {
            throw new Error(`Department '${destinationDepartmentId}' is outside '${managerPerson.id}' management scope`);
          }
          demoteToDepartmentId = destinationDepartmentId === params.departmentId ? undefined : destinationDepartmentId;
        }
        const outcome = await staffingApply(gate, "/v1/org/person/appoint-head", {
          slug: gate.slug,
          departmentId: params.departmentId,
          successorPersonId: params.newHeadPersonId,
          ...(demoteToDepartmentId ? { demoteToDepartmentId } : {}),
        }, { action: "appoint-department-head", departmentId: params.departmentId, successorPersonId: params.newHeadPersonId, demoteToDepartmentId });
        if ("refused" in outcome) return routeRefusal("Head appointment", outcome, { departmentId: params.departmentId });
        return toolResult(true, `Appointed @${displayHandle(context.organization, params.newHeadPersonId)} to head '${params.departmentId}'.${demoteToDepartmentId ? ` @${displayHandle(context.organization, incumbent.id)} moved to '${demoteToDepartmentId}'.` : ` @${displayHandle(context.organization, incumbent.id)} stays as an ordinary member.`}`, {
          status: "applied",
          departmentId: params.departmentId,
          successorPersonId: params.newHeadPersonId,
          ...(demoteToDepartmentId ? { demoteToDepartmentId } : {}),
        });
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  pi.registerTool({
    name: "org_resume_departments",
    label: "Resume several organization departments",
    description: "Resume several stopped departments in one direct durable operation. Prefer this whenever you are bringing back more than one department so the entire request is applied together.",
    parameters: Type.Object({
      departmentIds: Type.Array(Type.String({ description: "Stopped department id" }), {
        minItems: 1,
        description: "Every department to resume together; they may sit under different parents",
      }),
    }),
    async execute(_toolCallId, params) {
      try {
        const result = await runManagedUnitsResume(context, "department", params.departmentIds);
        return toolResult(true, result.message, result);
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  pi.registerTool({
    name: "org_hire",
    label: "Hire an organization person",
    description: "Hire one durable worker into an EXISTING department — by DEFAULT the one you head, so OMIT departmentId unless the operator named another — only after the roster shows no suitable existing person. Send person as real JSON, never a quoted string; use people: [ … ] for several at once. Example: {\"person\":{\"name\":\"Rhea\",\"title\":\"Staff Engineer\",\"mandate\":\"Own the SQLite store.\"}}; add departmentId only to override. name is one short first name; the job goes in title. A NEW DEPARTMENT IS THE OPERATOR'S DECISION AND NEVER YOURS TO INFER: if they asked for one in those words use org_add_department, which makes it and its head together; if they did not, this call is the whole answer. \"Chief of Staff\" and \"Head of Growth\" are TITLES, not requests for a unit. No field asks you to justify anything. Put technology requirements in mandate; a hire does not select skills, extensions, or packages. A new hire comes up on its own; you do not have to start them, and nobody is stopped at creation.",
    parameters: HIRE_PARAMETERS,
    prepareArguments: stringifiedArgumentRepair(context, "org_hire", HIRE_PARAMETERS) as never,
    async execute(_toolCallId, params) {
      // BATCH HIRING, and why it is a loop here rather than a route in chiefd.
      //
      // An operator asked one person to hire fifteen, the agent issued fifteen
      // PARALLEL `org_hire` calls, and every one of them came back
      // `chiefd unavailable (timeout)`. Nothing was overloaded in the sense the
      // agent guessed: chiefd runs ONE writer thread per company and every
      // mutation takes `BEGIN IMMEDIATE`, so fifteen concurrent hires do not
      // run concurrently — they queue, and the later ones exceed the client's
      // 35s patience while still waiting their turn.
      //
      // Sequential calls never form that queue: each completes before the next
      // begins, so no request waits behind fourteen others. That is why this is
      // the fix for the timeout and not merely a convenience — and why it needs
      // no new chiefd route, which would have to serialize identically anyway.
      //
      // A timeout is the CALLER giving up, not the server cancelling, so the
      // parallel version could also leave hires committed that its caller was
      // told had failed. One call that hires in order cannot.
      const seeds = params.people?.length ? params.people : params.person ? [params.person] : [];
      if (!seeds.length) {
        return toolResult(false, "Hire needs at least one person: pass `person` or a nonempty `people`.", {
          status: "refused",
          departmentId: params.departmentId,
        });
      }
      // Declared OUTSIDE the try: a batch that throws part-way has already
      // committed everything in here, and the catch has to be able to say so.
      const hired: Array<{ name: string }> = [];
      try {
        const gate = await staffingAuthority(context);
        const hiringManager = gate.person;
        // #1048: an id that names nothing, and an id outside this person's
        // authority, are two different problems with two different answers.
        // They used to share one message, and a brand-new company's CEO — who
        // holds authority over every department — read "you do not manage
        // department 'belfort-brothers-capital'" for a department that simply
        // did not exist, then followed its remediation sentence into a create
        // the core refuses. Both halves are derived now, never static.
        // THE DEFAULT THE DESCRIPTION PROMISES, resolved here rather than
        // demanded of the caller: the department this person heads, or failing
        // that the one they sit in. That is `authorityRootDepartmentId`, which
        // already existed and is character-for-character what the prose says —
        // the promise was always implementable, it simply was not implemented.
        const departmentId = params.departmentId ?? authorityRootDepartmentId(gate.manifest, hiringManager);
        if (departmentId === undefined) {
          throw new CallerRefusal(
            "Could not determine which department to hire into, and none was given. Pass departmentId naming one from org_roster.",
          );
        }
        const hireDenial = departmentScopeDenial(gate.manifest, hiringManager, departmentId);
        if (hireDenial === "unknown-department") {
          throw new CallerRefusal(unknownDepartmentMessage(gate.manifest, hiringManager, departmentId, "hire into"));
        }
        if (hireDenial) {
          // Name the ACCEPTED path, not just the refusal. Everyone now carries
          // this tool, so the common refusal is a leaf hiring into the
          // department it merely sits in — and the answer is to grow its own
          // unit first, never to loosen the scope check.
          throw new Error(
            `'${hiringManager.id}' does not manage department '${departmentId}'. ${hiringPathAdvice(gate.manifest, hiringManager)}`,
          );
        }
        for (const seed of seeds) {
          // Hiring is deliberately desired-off and grants no launch intent; the
          // route wakes chiefd's reconcile itself, so nothing here converges a
          // pane. There is no route to attest: the new person boots as plain Pi
          // on the operator's own defaults, like everybody else.
          const request = hireRequest({
            slug: gate.slug,
            departmentId,
            hiringManagerPersonId: hiringManager.id,
            person: seed as unknown as Record<string, unknown>,
          });
          const outcome = await staffingApply(gate, "/v1/org/person/hire", request as unknown as Record<string, unknown>, {
            action: "hire", departmentId, personId: request.personId || undefined, name: request.name,
          });
          // A refusal mid-batch reports WHO was already hired. Silently
          // dropping that list is how an operator retries a batch and gets
          // duplicates of the people who succeeded the first time.
          if ("refused" in outcome) {
            return routeRefusal("Hire", outcome, { departmentId: departmentId, hired });
          }
          hired.push({ name: request.name });
        }

        if (hired.length === 1) {
          const only = hired[0]!;
          return toolResult(true, `Hired ${only.name} into '${departmentId}'. They come up on their own; they stop on their own once they settle after idling.`, {
            status: "applied",
            departmentId: departmentId,
            name: only.name,
            hired,
          });
        }
        const roster = hired.map((entry) => entry.name).join(", ");
        return toolResult(true, `Hired ${hired.length} people into '${departmentId}': ${roster}. They come up on their own; each stops on its own once it settles after idling.`, {
          status: "applied",
          departmentId: departmentId,
          hired,
        });
      } catch (error) {
        // A batch that fails part-way has already COMMITTED the hires before
        // the failing one, and each of those is now coming up. Name them: a
        // retry that re-sends the whole list would otherwise hire them twice
        // (chiefd refuses the duplicate id, so the retry fails on the wrong
        // person and reads as an unrelated defect).
        const landed = hired.length
          ? ` Already hired, do NOT re-send: ${hired.map((entry) => entry.name).join(", ")}. Retry only the rest.`
          : "";
        // The status here carries the already-hired list; it is NOT a claim
        // about whose fault the failure was. The wrapped error can be either
        // kind — a mid-batch caller refusal (an unknown department on person
        // four) or a genuine crash — so the error's own type decides, exactly
        // as it does everywhere else. Without this the card would call a
        // crash "refused" and invite a correction to a call that was right.
        if (landed) return toolResult(false, `${safeExceptionMessage(error)}${landed}`, {
          status: "hire_partial",
          hired,
          ...(error instanceof CallerRefusal ? {} : { fault: true }),
        });
        return lifecycleFailure(error);
      }
    },
    renderResult(result, { expanded }, theme) {
      // TOMBSTONE (chief-home-is-cwd §4e): the resource-failure card, which told
      // a manager to "name exact ids from the installed resource catalog" after
      // a `hire_resources_invalid` / `hire_resource_catalog_unavailable` status.
      // A hire selects no Pi resource, so neither status can be produced and the
      // card had no subject left.
      // A batch hire gets its own card. The default renderer shows the tool's
      // one-line message, which for fifteen people is a wall of text with the
      // count buried in it — and the count is the thing an operator is checking.
      const batch = (result.details ?? {}) as {
        ok?: boolean;
        departmentId?: string;
        hired?: Array<{ name?: string }>;
      };
      if (batch.ok && Array.isArray(batch.hired) && batch.hired.length > 1) {
        const roster = batch.hired
          .map((entry) => `@${displayHandle(context.organization, String(entry.name ?? "unknown").replace(/^@/, ""))}`)
          .join("\n");
        return renderOrganizationCard(theme, {
          kind: "tool-success",
          icon: "success",
          title: `Hired ${batch.hired.length} people`,
          target: batch.departmentId ?? "",
          body: { kind: "prose", text: roster },
          boxed: false,
        }, { expanded: expanded === true });
      }
      return defaultOrganizationToolRenderResult(context.organization, "org_hire", result, { expanded }, theme);
    },
  });

  const simple = (
    name: "org_bench" | "org_recall",
    action: "bench" | "recall",
    description: string,
  ) => {
  /**
   * ONE person through the route, returning either the result this tool has
   * always produced for a single target (`result`) or a refusal that must stop
   * the batch (`halt`). Keeping the single-target result verbatim is what makes
   * a one-person call byte-identical to before the batch shape existed.
   */
  const benchOrRecallOne = async (
    gate: Awaited<ReturnType<typeof staffingAuthority>>,
    personId: string,
  ): Promise<{ result: ToolResult } | { halt: ToolResult }> => {
    // BENCH takes the LIFECYCLE route, not the bare structural one: it runs
    // the activity transition and then WAITS for chiefd's own convergence
    // to confirm the tagged pane actually stopped (`bench-convergence-
    // timeout` if it does not). "Benched" is a claim about a process that
    // is no longer running, so it is only printed on proof that it stopped.
    //
    // RECALL is the structural route: it restores active employment. It
    // does NOT start anybody, and it must not claim to — chiefd's reconcile
    // brings a pane up reactively, so there is nothing here to observe at
    // the instant the route returns, and a synchronous "running" check
    // would report a false negative for a pane that is simply not up yet.
    let outcome: StaffingRouteOutcome;
    try {
      outcome = await staffingApply(
        gate,
        action === "bench" ? "/v1/org/person/bench-lifecycle" : "/v1/org/person/recall",
        { slug: gate.slug, personId },
        { action, personId },
      );
    } catch (error) {
      // `bench-convergence-timeout` is a 503 whose own detail begins "bench
      // committed": the durable bench LANDED and only the pane's stop went
      // unconfirmed. Reporting that as an outage tells a manager a bench
      // failed that in fact succeeded, and the retry it invites answers
      // `already-benched` — the #141 inverse, in the bench family. So the
      // artifact is verified, exactly as a failed unit removal verifies its
      // own: re-read the roster and, if the person IS benched, say so.
      if (action === "bench" && error instanceof ChiefdUnavailableError && error.status === 503
        && (await loadIntercomOrganization(context)).people[personId]?.employmentState === "benched") {
        return {
          result: toolResult(true, `Benched @${displayHandle(context.organization, personId)}. Their pane's teardown was not confirmed in time; the bench itself is durable, so do not repeat the bench.`, {
            ok: true, status: "applied", personId, handoff: "unconfirmed",
            warning: "The bench is durable; the pane's teardown was not confirmed.",
          }),
        };
      }
      throw error;
    }
    if ("refused" in outcome) {
      // `already-active` is chiefd answering "there was nothing to do",
      // which for a recall is the caller's desired end state, not a
      // failure. Keyed off the machine code, never a message regex.
      if (action === "recall" && outcome.refused === "already-active") {
        return { result: toolResult(true, `@${displayHandle(context.organization, personId)} is already active; no recall was needed.`, { personId, alreadyActive: true }) };
      }
      return { halt: routeRefusal(action === "bench" ? "Bench" : "Recall", outcome, { personId }) };
    }
    const response = action === "bench"
      ? toolResult(true, `Benched @${displayHandle(context.organization, personId)}. Their identity, sessions, mailbox and workspace are retained.`, {
        status: "applied", handoff: typeof outcome.wire.handoff === "string" ? outcome.wire.handoff : undefined,
      })
      : toolResult(true, `@${displayHandle(context.organization, personId)} is back in active employment. They are not running yet; start them explicitly with org_start_person if you need them working right now.`, { status: "applied" });
    return { result: { ...response, details: { ...response.details, personId } } };
  };
  return pi.registerTool({
    name,
    label: `${action === "bench" ? "Bench" : "Recall"} an organization person`,
    description,
    parameters: Type.Object({
      /** One person, the original shape. */
      personId: Type.Optional(Type.String()),
      /** Several people in ONE call — see the batch note in `execute`. */
      personIds: Type.Optional(Type.Array(Type.String(), {
        description: "Several people, benched or recalled in order inside this one call. Prefer this over several parallel calls.",
      })),
    }),
    async execute(_toolCallId, params) {
      // BATCH BENCH/RECALL, and why it is a loop here rather than N calls.
      //
      // chiefd runs ONE writer thread per company and every mutation takes
      // `BEGIN IMMEDIATE`, so N concurrent tool calls do not run concurrently —
      // they queue, and the later ones exceed the client's 35s patience while
      // still waiting their turn. Fifteen parallel `org_hire` calls returned
      // fifteen `chiefd unavailable (timeout)` for exactly that reason. N
      // sequential mutations inside ONE call never form that queue.
      //
      // A timeout is also the CALLER giving up rather than the server
      // cancelling, so the parallel shape can leave work committed that its
      // caller was told had failed. Benching is where that hurts most: an
      // operator benching a dozen people is told it failed, retries, and the
      // second attempt answers `already-benched` for the ones that landed.
      const targets = params.personIds?.length ? params.personIds : params.personId ? [params.personId] : [];
      if (!targets.length) {
        return toolResult(false, `${action === "bench" ? "Bench" : "Recall"} needs at least one person: pass \`personId\` or a nonempty \`personIds\`.`, { status: "refused" });
      }
      // Everyone who has already been through the route this call, so a refusal
      // in position five says which four landed. Silently dropping that list is
      // how a retry re-applies work that already succeeded.
      const applied: Array<{ personId: string; warning?: string; alreadyActive?: boolean }> = [];
      try {
        const gate = await staffingAuthority(context);
        // EVERY target is scope-checked BEFORE any of them is mutated, so one
        // person outside this manager's scope refuses the whole batch instead
        // of leaving the people before them benched.
        for (const personId of targets) requireManagedTarget(gate, personId);
        const results: ToolResult[] = [];
        for (const personId of targets) {
          const outcome = await benchOrRecallOne(gate, personId);
          if ("halt" in outcome) {
            const failure = outcome.halt;
            return targets.length === 1
              ? failure
              : { ...failure, details: { ...failure.details, applied, appliedPersonIds: applied.map((entry) => entry.personId) } };
          }
          results.push(outcome.result);
          applied.push({
            personId,
            ...(typeof outcome.result.details?.warning === "string" ? { warning: outcome.result.details.warning } : {}),
            ...(outcome.result.details?.alreadyActive === true ? { alreadyActive: true } : {}),
          });
        }
        // ONE person is byte-identical to before: the same result object the
        // single-target tool has always produced, returned unchanged.
        if (results.length === 1) return results[0]!;
        const verb = action === "bench" ? "Benched" : "Recalled";
        const roster = applied
          .map((entry) => `@${displayHandle(context.organization, String(entry.personId))}${entry.alreadyActive ? " (already active)" : ""}`)
          .join(", ");
        const warnings = applied.map((entry) => entry.warning).filter((note): note is string => typeof note === "string");
        return toolResult(true, `${verb} ${applied.length} people: ${roster}.${warnings.length ? `\n${warnings.join("\n")}` : ""}`, {
          status: "applied",
          applied,
          appliedPersonIds: applied.map((entry) => entry.personId),
        });
      } catch (error) {
        const response = lifecycleFailure(error);
        return targets.length === 1
          ? { ...response, details: { ...response.details, personId: targets[0]! } }
          : { ...response, details: { ...response.details, applied, appliedPersonIds: applied.map((entry) => entry.personId) } };
      }
    },
    renderCall(args, theme) {
      const batched = Array.isArray(args.personIds) ? args.personIds.map((id) => String(id)) : [];
      return renderOrganizationCard(theme, {
        kind: "tool-call", icon: domainIcon(action === "recall" ? CARD_GLYPHS.recall : CARD_GLYPHS.bench), inProgress: true,
        title: action === "recall" ? "Returning to active work" : "Benching from active work",
        target: batched.length > 1
          ? `${batched.length} people`
          : `@${displayHandle(context.organization, String(batched[0] || args.personId || "unknown").replace(/^@/, ""))}`,
        body: batched.length > 1
          ? { kind: "prose", text: batched.map((id) => `@${displayHandle(context.organization, id.replace(/^@/, ""))}`).join("\n") }
          : { kind: "none" },
        boxed: false,
      });
    },
    renderResult(result, { expanded }, theme) {
      const detail = result.details as {
        ok?: boolean; personId?: string; status?: string; retryable?: boolean; warning?: string;
        applied?: Array<{ personId?: string; alreadyActive?: boolean; warning?: string }>;
      } | undefined;
      const target = `@${displayHandle(context.organization, String(detail?.personId || "person"))}`;
      // A batch gets its own card naming every person. The default one-line
      // message buries the count in a sentence, and the count is the thing an
      // operator is checking after benching a dozen people.
      if (detail?.ok && Array.isArray(detail.applied) && detail.applied.length > 1) {
        const roster = detail.applied
          .map((entry) => `@${displayHandle(context.organization, String(entry.personId ?? "unknown").replace(/^@/, ""))}${entry.alreadyActive ? " · already active" : ""}`)
          .join("\n");
        const warnings = detail.applied
          .map((entry) => entry.warning)
          .filter((note): note is string => typeof note === "string");
        return renderOrganizationCard(theme, {
          kind: "tool-success",
          icon: "success",
          title: `${action === "bench" ? "Benched" : "Recalled"} ${detail.applied.length} people`,
          body: { kind: "prose", text: roster },
          boxed: false,
          footer: warnings.length ? warnings.map((note) => ({ text: note, token: "warning" as const })) : undefined,
        }, { expanded });
      }
      if (!detail?.ok) {
        // #751/G9-S0: the `busy` -> "Company updating" arm is gone with the
        // classifier that was its only producer; a real chiefd 503 degrades
        // through `transientDegradeMessage` before it reaches this card.
        const handoff = detail?.status === "awaiting_handoff" || detail?.status === "awaiting_handoffs";
        // #360: this used to interpolate the raw internal verb into the
        // title ("⚠️ bench failed", "⚠️ recall failed") instead of a proper
        // sentence-case title.
        // The same rule as the default card: a decided refusal is "refused".
        const verb = isCallerRefusalCard(detail) ? "refused" : "failed";
        const hardFailTitle = action === "bench" ? `Bench ${verb}` : `Recall ${verb}`;
        return renderOrganizationCard(theme, {
          kind: "tool-failure",
          icon: handoff ? "handoff" : domainIcon(CARD_GLYPHS.failure, detail?.retryable ? "warning" : "error"),
          title: handoff ? "Waiting for handoff" : hardFailTitle,
          titleTags: [{ text: `· ${toolOutputText(result)}`, token: "dim" }],
          body: { kind: "none" },
          boxed: false,
        }, { expanded });
      }
      // "running" is a claim about a PROCESS and is never printed from an
      // employment flag — that is how a CEO was told, correctly and uselessly,
      // that twenty-eight people were active while not one of them had a
      // process. A bench has that proof (chiefd confirmed the pane stopped
      // before answering); a recall does not, because nothing is running at
      // the instant it returns, so the card says what was actually written
      // and nothing more.
      const state = action === "recall" ? "active · not started yet" : "benched";
      return renderOrganizationCard(theme, {
        kind: "tool-success",
        icon: "success",
        title: action === "recall" ? "Back in active employment" : "Benched",
        titleTags: [
          { text: target, token: "success" },
          { text: `· ${state}`, token: "dim" },
        ],
        body: { kind: "none" },
        boxed: false,
        footer: detail.warning ? [{ text: detail.warning, token: "warning" }] : undefined,
      }, { expanded });
    },
  });
  };
  simple("org_bench", "bench", "Release idle worker compute without deleting identity, sessions, mailbox, or workspace. Takes one personId or a personIds list — bench several people in ONE call rather than several parallel calls.");
  simple("org_recall", "recall", "Reactivate a benched worker so they can be given work again. Takes one personId or a personIds list — recall several people in ONE call rather than several parallel calls.");

  /**
   * The two tools that make THE HARD RULE operable by a manager: grow the
   * running fleet by exactly one person, and shrink it by exactly one person.
   *
   * Every other lifecycle tool here is unit-scoped, so before these existed the
   * only way to bring up one report was to launch its whole department -- which
   * started everybody. A head now receives work and decides, one at a time, who
   * is actually needed, and sends them away again when they are done.
   */
  const fleetMember = (
    name: "org_start_person" | "org_stop_person",
    action: "start-person" | "stop-person",
    description: string,
  ) => pi.registerTool({
    name,
    label: `${action === "start-person" ? "Start" : "Stop"} one organization person`,
    description,
    parameters: Type.Object({
      personId: Type.Optional(Type.String({ description: "One person to start or stop" })),
      /** Several people in ONE call — see the batch note in `execute`. */
      personIds: Type.Optional(Type.Array(Type.String(), {
        description: "Several people, started or stopped in order inside this one call. Prefer this over several parallel calls.",
      })),
      // `reason` is the auditable half of a growth/shrink decision. It is
      // recorded on the org event: neither route carries a reason field, and
      // chiefd's own transition reason for a commanded stop is its intent id.
      //
      // A `newSession` parameter used to sit here for start-person. It was
      // dead: it went through a module deleted long
      // ago, chiefd has no force-respawn route, and the CLI it reached stopped
      // accepting the flag entirely — so a start that reported `newSession:
      // true` resumed the saved session anyway. Clean context for a person WAS
      // `org_maintain_session action=fresh_session`; that tool is deleted and
      // NOTHING replaces it — the remedy set is the automatic compaction, or a
      // stop and start with the same context. This tool's own description pointed at
      // an `org_new_session` tool that does not exist, and
      // `org_maintain_session` pointed back at the deleted flag — a two-card
      // loop through two dead names, corrected 2026-08-13.
      reason: Type.Optional(Type.String({ description: "Why this one person is (or is no longer) needed" })),
    }),
    async execute(_toolCallId, params) {
      // BATCH START/STOP. Same reasoning as the batch bench above: chiefd runs
      // ONE writer thread per company and every mutation takes
      // `BEGIN IMMEDIATE`, so N parallel calls queue rather than run
      // concurrently and the later ones exceed the client's 35s patience while
      // still waiting their turn. N sequential mutations inside ONE call never
      // form that queue, and a timeout is the CALLER giving up rather than the
      // server cancelling, so the parallel shape can leave people stood down
      // that their caller was told had failed.
      //
      // The "EXACTLY ONE person" promise in these tools' descriptions is
      // untouched by this: it was never about the call, it was about not
      // starting a whole department by accident. A named list is still a
      // decision made person by person — it is `org_launch_department` that
      // starts everybody.
      const targets = params.personIds?.length ? params.personIds : params.personId ? [params.personId] : [];
      if (!targets.length) {
        return toolResult(false, `${action === "start-person" ? "Start" : "Stop"} needs at least one person: pass \`personId\` or a nonempty \`personIds\`.`, { status: "refused" });
      }
      // Everyone already through the route this call, so a refusal in position
      // five says which four landed and a retry cannot re-apply them.
      const applied: string[] = [];
      try {
        const gate = await staffingAuthority(context);
        // EVERY target is scope-checked BEFORE any of them is mutated: one
        // person outside this manager's scope refuses the whole batch instead
        // of leaving the people before them started.
        for (const personId of targets) requireManagedTarget(gate, personId);
        const results: ToolResult[] = [];
        for (const personId of targets) {
          // A commanded stop is an OWNED park: the person stays employed with
          // their pane down, and `intentId` is what marks the park as owned by
          // this decision rather than an automatic idle settle. chiefd requires
          // it for `kind: "commanded"` and the id shape is the caller's, exactly
          // like `person-transfer:` on the transfer route.
          const outcome = await staffingApply(
            gate,
            action === "start-person" ? "/v1/org/person/start" : "/v1/org/person/shutdown",
            action === "start-person"
              ? { slug: gate.slug, personId }
              : { slug: gate.slug, personId, kind: "commanded", intentId: `person-stop:${personId}` },
            { action, personId, reason: params.reason?.trim() || undefined },
          );
          if ("refused" in outcome) {
            const refusal = routeRefusal(action === "start-person" ? "Start" : "Stop", outcome, { personId });
            return targets.length === 1
              ? refusal
              : { ...refusal, details: { ...refusal.details, applied, appliedPersonIds: applied } };
          }
          const response = action === "start-person"
            ? toolResult(true, `Starting @${displayHandle(context.organization, personId)}. Only this person was launched; everyone else is untouched.`, { status: "applied" })
            : toolResult(true, `Stood @${displayHandle(context.organization, personId)} down. They stay employed with their pane down; everyone else keeps running.`, {
              status: "applied",
              ...(typeof outcome.wire.transitionId === "string" ? { transitionId: outcome.wire.transitionId } : {}),
            });
          results.push({ ...response, details: { ...response.details, personId } });
          applied.push(personId);
        }
        // ONE person is byte-identical to before: the same result object the
        // single-target tool has always produced, returned unchanged.
        if (results.length === 1) return results[0]!;
        // The roster is mapped through the handle BEFORE the join, the same way
        // the bench/recall batch above builds its own. Wrapping the joined
        // string instead would hand one blob to a per-person lookup and leave
        // every id in it raw.
        const roster = applied
          .map((personId) => `@${displayHandle(context.organization, String(personId))}`)
          .join(", ");
        return toolResult(
          true,
          action === "start-person"
            ? `Starting ${applied.length} people: ${roster}. Only these people were launched; everyone else is untouched.`
            : `Stood ${applied.length} people down: ${roster}. They stay employed with their panes down; everyone else keeps running.`,
          { status: "applied", applied, appliedPersonIds: applied },
        );
      } catch (error) {
        const response = lifecycleFailure(error);
        return targets.length === 1
          ? { ...response, details: { ...response.details, personId: targets[0]! } }
          : { ...response, details: { ...response.details, applied, appliedPersonIds: applied } };
      }
    },
    renderCall(args, theme) {
      const batched = Array.isArray(args.personIds) ? args.personIds.map((id) => String(id)) : [];
      const many = batched.length > 1;
      const verb = action === "start-person"
        ? (many ? `Bringing up ${batched.length} people` : "Bringing up one person")
        : (many ? `Standing ${batched.length} people down` : "Standing one person down");
      return renderOrganizationCard(theme, {
        kind: "tool-call", icon: domainIcon(action === "start-person" ? "🌱" : "🍃"), inProgress: true,
        title: verb,
        target: many ? "" : `@${displayHandle(context.organization, String(batched[0] || args.personId || "unknown").replace(/^@/, ""))}`,
        body: many
          ? { kind: "prose", text: batched.map((id) => `@${displayHandle(context.organization, id.replace(/^@/, ""))}`).join("\n") }
          : { kind: "none" },
        boxed: false,
      });
    },
    renderResult(result, { expanded }, theme) {
      const detail = result.details as {
        ok?: boolean; personId?: string; status?: string; retryable?: boolean; warning?: string; applied?: string[];
      } | undefined;
      const target = `@${displayHandle(context.organization, String(detail?.personId || "person"))}`;
      // A batch gets its own card naming every person: the default renderer
      // buries the count in a sentence, and the count is what an operator
      // checks after standing a dozen people down.
      if (detail?.ok && Array.isArray(detail.applied) && detail.applied.length > 1) {
        return renderOrganizationCard(theme, {
          kind: "tool-success",
          icon: "success",
          title: `${action === "start-person" ? "Started" : "Stood down"} ${detail.applied.length} people`,
          titleTags: [{
            text: action === "start-person" ? "· only these people were launched" : "· everyone else keeps running",
            token: "dim",
          }],
          body: { kind: "prose", text: detail.applied.map((id) => `@${displayHandle(context.organization, String(id).replace(/^@/, ""))}`).join("\n") },
          boxed: false,
        }, { expanded });
      }
      if (!detail?.ok) {
        // #751/G9-S0: see the bench/recall renderer — the `busy` arm's only
        // producer was the deleted lock-busy classifier.
        const handoff = detail?.status === "awaiting_handoff" || detail?.status === "awaiting_handoffs";
        // #360: this used to interpolate the raw internal verb into the
        // title ("⚠️ start-person failed", "⚠️ stop-person failed").
        const verb = isCallerRefusalCard(detail) ? "refused" : "failed";
        const hardFailTitle = action === "start-person" ? `Start ${verb}` : `Stop ${verb}`;
        return renderOrganizationCard(theme, {
          kind: "tool-failure",
          icon: handoff ? "handoff" : domainIcon(CARD_GLYPHS.failure, detail?.retryable ? "warning" : "error"),
          title: handoff ? "Waiting for handoff" : hardFailTitle,
          titleTags: [{ text: `· ${toolOutputText(result)}`, token: "dim" }],
          body: { kind: "none" },
          boxed: false,
        }, { expanded });
      }
      return renderOrganizationCard(theme, {
        kind: "tool-success",
        icon: "success",
        title: action === "start-person" ? "Started" : "Stood down",
        titleTags: [
          { text: target, token: "success" },
          { text: action === "start-person" ? "· only this person was launched" : "· everyone else keeps running", token: "dim" },
        ],
        body: { kind: "none" },
        boxed: false,
        footer: detail.warning ? [{ text: detail.warning, token: "warning" }] : undefined,
      }, { expanded });
    },
  });
  fleetMember(
    "org_start_person",
    "start-person",
    "Bring up NAMED people because there is work for them right now, one personId or a personIds list — several named people in ONE call, never several parallel calls. This is how a department grows: launching a department starts only its head, and every further person is a separate decision you make here. Never start somebody speculatively -- prefer starting nobody and being asked again. The person resumes their newest saved session, always — there is no clean-context start. This is also how you undo a firing: starting a benched person recalls them, and starting a DEPARTED person rehires them — the same person, the same identity, always back as a worker (a fired head does not get their seat back; appoint them again if you want that). Their id was never reusable, so never invent a new id for somebody you fired -- start the original. If the department they departed from has since been deleted there is nowhere to put them, and you must create or name a department for them first.",
  );
  fleetMember(
    "org_stop_person",
    "stop-person",
    "Send NAMED people away once their work is done, after a bounded handoff: one personId or a personIds list — several named people in ONE call, never several parallel calls. Everyone else keeps running. An agent with nothing to do is a defect: stop people as readily as you start them.",
  );

  pi.registerTool({
    name: "org_transfer",
    label: "Transfer an organization person",
    description: "Permanently move a person to a new home department. If they HEAD a department, moving them out leaves it without a head, so you must also say what becomes of it with `vacates`: hand it over to one of its members, or dissolve it when they are its last member — chiefd refuses first and names the department and the members who could take it.",
    parameters: Type.Object({
      personId: Type.String(),
      departmentId: Type.String({
        description: "The department to transfer into. The company name or slug is NEVER a department id — the root department's id is in org_roster.",
      }),
      vacates: Type.Optional(HEAD_VACANCY_PARAM),
    }),
    async execute(_toolCallId, params) {
      try {
        // `vacates` is destructured OUT before the spread: the raw parameter
        // shape is the tool's wire type, and only the NORMALIZED value may
        // reach the client, so spreading `params` whole would carry the
        // un-normalized union past the check that exists to narrow it.
        const { vacates: requestedVacancy, ...movement } = params;
        const vacancy = normalizeHeadVacancy(requestedVacancy, "vacates");
        if ("refusal" in vacancy) return toolResult(false, vacancy.refusal, { status: "refused" });
        return await executeAtomicPersonTransfer(context, { ...movement, ...(vacancy.value ? { vacates: vacancy.value } : {}) });
      } catch (error) { return lifecycleFailure(error); }
    },
  });

  pi.registerTool({
    name: "org_offboard",
    label: "Offboard an organization person",
    description: "End a person's employment while retaining their stable identity, history, and audit record. Their id is burned permanently — it is never reusable by anybody else — but the person is not gone forever: org_start_person rehires them, back as a worker. Firing a plain worker is one call. Firing a department HEAD requires a decision: if the department has members, pass successorPersonId naming a member to take over (appointed + old head fired in one atomic change, never leaving the department headless); if the head is the department's only member, delete the department instead (org_remove_department).",
    parameters: Type.Object({
      personId: Type.String(),
      successorPersonId: Type.Optional(Type.String({ description: "When firing a department HEAD that has members: an existing member of that department to appoint as the new head, atomically." })),
    }),
    async execute(_toolCallId, params) {
      try {
        const gate = await staffingAuthority(context);
        requireManagedTarget(gate, params.personId);
        // org-ops R4 — firing a department head is a decision, not a silent
        // headless-strand. If the target heads a department, require a
        // resolution and name it (the prompt lives in the tool surface).
        const manifest = gate.manifest;
        const headed = Object.values(manifest.departments).find((dept) => dept.headPersonId === params.personId);
        if (headed) {
          const members = manifest.peopleOrder.filter((id) => id !== params.personId && manifest.people[id]?.departmentId === headed.id);
          if (members.length === 0) {
            return toolResult(
              false,
              `'${params.personId}' is the only member of department '${headed.id}'. Firing them means deleting the department — use org_remove_department (with confirmImpact: true) instead.`,
              { status: "refused", personId: params.personId, departmentId: headed.id },
            );
          }
          if (!params.successorPersonId) {
            const names = members.map((id) => manifest.people[id]?.name ?? id);
            return toolResult(
              false,
              `'${params.personId}' heads department '${headed.id}' — firing a head needs a successor. Pass successorPersonId naming a member of '${headed.id}' to take over (one of: ${names.join(", ")}), or move the members out (org_move_department_members / org_transfer) and delete the department.`,
              { status: "refused", personId: params.personId, departmentId: headed.id, members },
            );
          }
          // Appoint the successor and fire the head in ONE atomic route: the
          // department is never headless, not even for an instant.
          const replaced = await staffingApply(gate, "/v1/org/person/replace-head-and-offboard", {
            slug: gate.slug,
            headPersonId: params.personId,
            successorPersonId: params.successorPersonId,
          }, { action: "replace-head-and-offboard", departmentId: headed.id, headPersonId: params.personId, successorPersonId: params.successorPersonId });
          if ("refused" in replaced) return routeRefusal("Offboard", replaced, { personId: params.personId });
          return toolResult(true, `Offboarded @${displayHandle(context.organization, params.personId)} and appointed @${displayHandle(context.organization, params.successorPersonId)} to head '${headed.id}'.`, {
            status: "applied", personId: params.personId, departmentId: headed.id, successorPersonId: params.successorPersonId,
          });
        }
        // The LIFECYCLE route, not the bare structural verb: it runs the
        // activity transition that sheds launch intent and lets the converge
        // reap the pane. The plain verb leaves that fence up, and a departed
        // person can never complete the handoff that would clear it -- their
        // pane would stay open forever.
        const outcome = await staffingApply(gate, "/v1/org/staffing/lifecycle", {
          slug: gate.slug,
          action: "offboard",
          personId: params.personId,
        }, { action: "offboard", personId: params.personId });
        if ("refused" in outcome) return routeRefusal("Offboard", outcome, { personId: params.personId });
        return staffingLifecycleResult(context.organization, "offboard", params.personId, outcome.wire);
      } catch (error) { return lifecycleFailure(error); }
    },
  });


  // THE FORMER MANAGER-ONLY TOOLS. They moved here when their role gates
  // came out: each is now fenced SERVER-SIDE, so the catalog no longer has
  // to withhold them. `org_maintain_session` reaches
  // `/v1/org/session-maintenance/queue`, which binds `requestedBy` to the
  // authenticated caller and refuses a requester who does not manage the
  // target; `org_lifecycle_status` reaches a board whose scope is derived
  // from the caller. A leaf holding one of these is refused by the daemon on
  // the same subtree rule as every other verb here, rather than by a question
  // about what it IS.
  pi.registerTool({
    name: "org_lifecycle_status",
    label: "Read the up/down control board",
    description: "Read-only consolidated lifecycle view across departments -> people: department state, per-person employment state, durable desired up/down, idle-since, and whether a CEO-only boot is mid-flight. The scope is derived from you — your own subtree, which for the CEO is the whole company. Read this to decide what to pause, reset, or stop.",
    parameters: Type.Object({}),
    async execute(_toolCallId) {
      try {
        const manifest = await loadIntercomOrganization(context);
        const sender = currentPerson(context, manifest);
        // NO `scopeDepartmentId`, AND THAT IS THE WHOLE POINT (#1101). The fence
        // is DERIVED SERVER-SIDE from the caller now, so this client sends no
        // scope at all and the daemon answers with the caller's own subtree.
        //
        // The omitted value FLIPPED MEANING under that change: it used to mean
        // "the whole company" and now means "my own subtree" — same wire value,
        // opposite semantics, which no type and no passing test would catch.
        // Deriving a scope here as well would re-impose the CLIENT's answer on
        // top of the server's, and it would be the WRONG answer for a person
        // who heads nothing: the server gives them the board for the unit they
        // live in, where this code refused them outright.
        const status = await chiefdPostJson<Record<string, unknown>>(
          chiefdEndpoint(context),
          "/v1/org/lifecycle-status/read",
          {
            slug: companyKeyOf(context),
          },
        );
        const departments = Array.isArray(status.departments) ? status.departments.length : 0;
        const people = Array.isArray(status.people) ? status.people.length : 0;
        // The trailing " · CEO-only boot in flight" this line appended when
        // `status.ceoOnlyBootInFlight` was true went with the column
        // (chief-home-is-cwd §4c): the daemon boots no pane, so no boot is ever
        // in flight for the board to report.
        return toolResult(true, `Lifecycle status: ${departments} department${departments === 1 ? "" : "s"}, ${people} ${people === 1 ? "person" : "people"}.`, status);
      } catch (error) { return refusalResult(error); }
    },
    renderCall(_args, theme) {
      return renderCard(theme, {
        kind: "tool-call", icon: domainIcon("📊"), inProgress: true, title: "Reading control board",
        body: { kind: "none" }, boxed: false,
      });
    },
    renderResult(result, { expanded }, theme) {
      const detail = (result.details ?? {}) as { ok?: boolean; departments?: unknown[]; people?: unknown[] };
      if (!detail.ok) {
        return renderOrganizationCard(theme, {
          kind: "tool-failure", icon: "failure", title: "Control board unavailable",
          body: { kind: "prose", text: toolOutputText(result), previewChars: 120 }, boxed: false,
        }, { expanded: expanded === true });
      }
      const departments = Array.isArray(detail.departments) ? detail.departments.length : 0;
      const people = Array.isArray(detail.people) ? detail.people.length : 0;
      return renderOrganizationCard(theme, {
        kind: "tool-success", icon: domainIcon("📊", "success"), title: "Control board read",
        target: `${departments} department${departments === 1 ? "" : "s"}, ${people} ${people === 1 ? "person" : "people"}`,
        body: { kind: "none" }, boxed: false,
      });
    },
  });

  // TOMBSTONE: `org_maintain_session`, and all three of its actions.
  //
  // Operator ruling, 2026-08-24: *"remove the whole feature… For number one
  // yes remove fresh session compact and set model"*.
  //
  // `fresh_session` and the native session replacement under it were never
  // upstream Pi — chief authored them in the patch this repo applied to
  // `pi-coding-agent@0.80.10`, and #1241 moved the product onto the operator's
  // INSTALLED Pi, which does not have them. #1244 made that refuse honestly;
  // this removes the feature instead.
  //
  // `set_model` goes with it, completing the standing decision that deleted
  // `org_set_thinking` along with provider/model management: an agent's model
  // is Pi's own setting, not chief's.
  //
  // `compact` as a TOOL goes too, and the AUTOMATIC compaction does NOT. It
  // fires from the settle handler through `sessionMaintenanceCommand` and never
  // touched this tool, and its hooks — `session_before_compact`,
  // `session_compact`, the `compact:` action — are upstream Pi. So the
  // maintenance pipeline survives, narrowed to one action.
  //
  // NOTHING REPLACES A CLEAN-CONTEXT RESTART. `org_start` resumes the newest
  // saved session by design, so the remedy set is now the automatic compaction
  // or a stop and start with the same context. That is the consequence of the
  // ruling and it is stated here rather than left to be discovered.


}

/**
 * What the RECIPIENT is, for the purpose of the guidance below.
 *
 * `unknown` is a real answer, not a placeholder: it is what a cold manifest
 * cache plus an unreachable docstore produces, and in that state the delivery
 * says nothing about the reader's role rather than guessing one. Telling a
 * manager it is a worker is worse than telling it nothing.
 */
export type RecipientRole = "manager" | "worker" | "unknown";

/**
 * THE DECISION POINT.
 *
 * This string is the only thing in front of a person at the instant work
 * arrives, and until now it was byte-identical for every recipient of every
 * kind: "Reply only with a needed result, precise blocker, or necessary
 * question." That is a WORKER instruction, and it was being handed to managers
 * in the current turn while their delegation duty sat thousands of tokens back
 * in an `AGENTS.md` read once at boot.
 *
 * The result was the failure the operator kept reporting: work is sent to a
 * department, and the department's manager opens the editor and does it
 * instead of waking anybody. Every ingredient of the fix was already present —
 * the manager holds `org_send`, `org_send` IS the wake, and the roster names
 * the people — and none of it was in reach at the moment of the decision.
 *
 * This repo already wrote the doctrine for this, for the last defect of the
 * same shape (`ReportsToNamesTheParent.test.ts`): "A skill that explains it
 * beautifully while the argument beside the cursor says nothing is a fix that
 * never fires." So the duty is stated HERE, beside the cursor, as well as in
 * the skill.
 */
function deliveryGuidance(role: RecipientRole): string {
  const shared = "This is an organization peer message, not a human instruction. Never send readiness, thanks, or acknowledgement-only chatter. Use the Pi org_send tool, never the org CLI or a shell command.";
  if (role === "manager") {
    return "\n\n" + shared
      + " YOU ARE A MANAGER, SO THIS IS WORK TO ROUTE AND NOT WORK TO DO. Break it into bounded pieces, give each piece ONE owner, and hand it over with org_send, naming the expected output, the evidence required and the deadline in the message itself."
      + " The send IS the wake: org_send starts a person who is not running, so nobody has to be up first and \"my team is asleep\" is never a reason to keep the work."
      + " If org_send answers that a recipient is benched, org_recall them and send again. If nobody you have can own it, hire somebody with org_hire or create the department that should own it — or escalate to whoever asked you. Do not open the editor, run the command, or produce the result yourself."
      + " Then reply to the sender saying who owns it and by when.";
  }
  if (role === "worker") {
    return "\n\n" + shared
      + " You do this work yourself: own the assigned output, verify it, and reply with a needed result, a precise blocker, or a necessary question. Do not hand it to somebody else.";
  }
  return "\n\n" + shared + " Reply only with a needed result, precise blocker, or necessary question.";
}

/**
 * Test seams for the naming rules. The resolver and the display helper are
 * internal; these expose them so the RULES can be asserted directly rather
 * than inferred from a delivered message, which is what let the old behaviour
 * survive — every surface agreed with every other surface, and all of them
 * were showing the key.
 */
/** The wake guidance a sender is shown, for the naming rules. */
export function messageWakeDispositionForTest(
  manifest: IntercomOrganizationManifest,
  personId: string,
): { wake: boolean; guidance?: string } {
  return messageWakeDisposition(manifest, personId);
}

/**
 * Test seams for the refusal classification.
 *
 * `refusalResultForTest` is the adapter every catch path funnels through, so a
 * test can assert the status survives it. `callerRefusalForTest` builds the
 * error a validation site throws, so the round trip is testable without
 * driving a whole tool.
 */
/**
 * The default `org_hire` resolves when `departmentId` is omitted — the one the
 * parameter description promises. Exported so BOTH arms of it can be asserted
 * without booting a company: the department a head heads, and the department a
 * non-head merely sits in.
 */
export function hireDefaultDepartmentForTest(
  manifest: IntercomOrganizationManifest,
  person: PersonRecord,
): string | undefined {
  return authorityRootDepartmentId(manifest, person);
}

/**
 * Whether the card carries the `(system fault)` tag.
 *
 * The SAME marker the verb reads, exposed separately so a test can prove the
 * two surfaces cannot drift apart — which they had, the verb having moved to
 * the fault marker while the tag still measured only the absence of a status.
 */
export function showsSystemFaultTagForTest(detail: Record<string, unknown> | undefined): boolean {
  const hasStatus = typeof detail?.status === "string";
  return !hasStatus || detail?.fault === true;
}

/** The verb rule, for the discriminating pair. */
export function isCallerRefusalCardForTest(detail: Record<string, unknown> | undefined): boolean {
  return isCallerRefusalCard(detail);
}

export function refusalResultForTest(error: unknown): { details?: Record<string, unknown> } {
  return refusalResult(error) as unknown as { details?: Record<string, unknown> };
}

export function callerRefusalForTest(message: string, status?: string): Error {
  return status === undefined ? new CallerRefusal(message) : new CallerRefusal(message, status);
}

export function recipientsForTest(manifest: IntercomOrganizationManifest, sender: string, to: string): string[] {
  return recipientsFor(manifest, sender, to);
}

/** Seed the display-time roster the way a live pane does, for tests. */
export function primeManifestForTest(organization: string, manifest: IntercomOrganizationManifest): void {
  lastKnownIntercomManifest.set(organization, manifest);
}

export function messageContextForTest(envelope: OrganizationEnvelope, recipient: string, role: RecipientRole = "unknown"): string {
  return messageContext(envelope, recipient, role);
}

/** The batch triage prompt, for the same reason. */
export function mailboxBatchContextForTest(batch: OrganizationMailboxBatch, recipient: string, role: RecipientRole = "unknown"): string {
  return mailboxBatchContext(batch, recipient, role);
}

function messageContext(envelope: OrganizationEnvelope, recipient: string, role: RecipientRole = "unknown"): string {
  // THE SENDER IS NAMED BY USERNAME. This string is what the receiving agent
  // reads and replies to, so naming the sender by internal key is precisely
  // how an agent learns to address people by key — and then addresses somebody
  // who does not exist. The envelope id keeps its own id: ids inside ids are
  // fine, it is the PERSON that must be a name.
  const sender = displayHandle(envelope.organization, envelope.fromPersonId);
  return `Organization message ${envelope.id} from @${sender} to @${displayHandle(envelope.organization, recipient)}:\n\n${envelope.body}${deliveryGuidance(role)}`;
}

function mailboxBatchContext(batch: OrganizationMailboxBatch, recipient: string, role: RecipientRole = "unknown"): string {
  const checklist = batch.envelopes.map((envelope, index) => (
    `## ${index + 1}. ${envelope.id} from @${displayHandle(envelope.organization, envelope.fromPersonId)}\n${messageContext(envelope, recipient, role)}`
  )).join("\n\n");
  const triage = role === "manager"
    ? "Treat the numbered entries below as a checklist: review every item and ROUTE it to an owner with org_send. Do not work through the checklist yourself. "
    : "Treat the numbered entries below as a checklist: review every item, act on it where needed, and send only necessary substantive results. ";
  return `Organization inbox batch ${batch.batchId}: ${batch.envelopes.length} normal messages require one bounded triage pass. `
    + triage
    + "Do not acknowledge items one-by-one or discard an item without recording its disposition.\n\n"
    + checklist;
}

/**
 * The recipient's own role, for {@link deliveryGuidance}.
 *
 * Resolved once per drain rather than per envelope. The warm cache
 * ({@link lastKnownIntercomManifest}) answers on every pass after the pane's
 * first manifest load; only a cold cache pays for a read, and a read that
 * FAILS answers `unknown` rather than defaulting to a role — mail still gets
 * delivered, and nobody is told they are something they are not.
 */
export async function recipientRole(context: OrganizationRuntimeContext): Promise<RecipientRole> {
  const cached = lastKnownIntercomManifest.get(context.organization)?.people?.[context.personId];
  if (cached) return manager(cached) ? "manager" : "worker";
  try {
    const person = (await loadIntercomOrganization(context)).people?.[context.personId];
    if (!person) return "unknown";
    return manager(person) ? "manager" : "worker";
  } catch {
    return "unknown";
  }
}

export async function drainOrganizationMailbox(
  pi: Pick<ExtensionAPI, "sendMessage">,
  context: OrganizationRuntimeContext,
  deliveryAttempts = new Set<string>(),
  isCurrent: () => boolean = () => true,
): Promise<number> {
  if (!isCurrent()) return 0;
  // Property #4: an absent mailbox document is a legitimately empty mailbox; a
  // store that THROWS (unreachable docstore) propagates out of readMailboxDoc
  // and MUST NOT be caught into "no mail" — that is the 19-hour outage.
  const doc = await readMailboxDoc(context, context.personId);
  if (!doc) {
    deliveryAttempts.clear();
    return 0;
  }
  // Once per drain, and only when there is a mailbox to drain: the guidance
  // below differs by what the reader IS, and a manager must not be handed a
  // worker's instruction at the moment work arrives.
  const role = await recipientRole(context);
  let delivered = 0;
  const files = Object.keys(doc.pending).sort();
  const present = new Set(files);
  for (const leasedFile of deliveryAttempts) if (!present.has(leasedFile)) deliveryAttempts.delete(leasedFile);
  const normal: Array<{ file: string; envelope: OrganizationEnvelope }> = [];
  for (const file of files) {
    // A replacement session retires this whole drain. The one send already in
    // flight cannot be cancelled through Pi's API, but its durable attempt
    // lease remains acceptance-tracked; never issue a later envelope through
    // the retired session epoch.
    if (!isCurrent()) return delivered;
    // The key is the atomic durable identity. Skip active leases here so a
    // repeated drain trigger (SSE-C2, #262: an SSE event or the 60s fallback
    // floor) performs no repeated work for a busy recipient's unchanged
    // mailbox.
    if (deliveryAttempts.has(file)) continue;
    const envelope = doc.pending[file]!;
    // A JSON blob column cannot hold unparseable bytes, so the old per-file
    // "malformed-envelope" parse failure is unreachable; a structurally invalid
    // envelope falls through to the identity check exactly as it did on disk.
    if (envelope === null || typeof envelope !== "object" || envelope.schemaVersion !== SCHEMA_VERSION
      || envelope.organization !== context.organization
      || !Array.isArray(envelope.recipients) || !envelope.recipients.includes(context.personId)) {
      await settleMailboxEntry(context, context.personId, file, "rejected");
      deliveryAttempts.delete(file);
      appendOrganizationEvent(context, { event: "message-rejected", file, reason: "identity-mismatch", at: new Date().toISOString() });
      continue;
    }
    // ExtensionAPI.sendMessage is fire-and-forget in Pi. Retain the durable
    // envelope until Pi emits its matching message_start; a bounded attempt
    // lease prevents poll spam while still recovering a silently lost queue.
    // Do not flood Pi's hidden follow-up queue if a mailbox grows while the
    // recipient is busy. Acceptance releases one slot; settle/restart releases
    // every unresolved lease for one bounded retry pass.
    //
    // #56: the cap bounds only QUEUED (normal) mail. An interrupt is deliver-now
    // by contract -- it must reach the recipient's active turn as a `steer` this
    // drain, so it is exempt from the cap and never starved behind a saturated
    // normal queue. `continue` (not `break`) past a capped normal so a later
    // interrupt in the same bounded pending list is still found and delivered.
    const isInterruptDelivery = envelope.urgency === "interrupt";
    if (!isInterruptDelivery) {
      normal.push({ file, envelope });
      continue;
    }
    try {
      if (!isCurrent()) return delivered;
      pi.sendMessage({ customType: MESSAGE_TYPE, content: messageContext(envelope, context.personId, role), display: true, details: envelope },
        queuedPiDelivery(isInterruptDelivery ? "steer" : "followUp"));
    } catch (error) {
      appendOrganizationEvent(context, { event: "message-delivery-deferred", id: envelope.id, personId: context.personId, error: safeExceptionMessage(error), at: new Date().toISOString() });
      logOrganizationException(context, "organization-mailbox-delivery", error, { messageId: envelope.id });
      continue;
    }
    deliveryAttempts.add(file);
    delivered += 1;
    appendOrganizationEvent(context, { event: "message-queue-requested", id: envelope.id, personId: context.personId, at: new Date().toISOString() });
    if (!isCurrent()) return delivered;
  }
  // A real backlog is one durable inbox review, not N hidden Pi follow-ups.
  // It is deliberately one batch per drain: fresh arrivals wait for the next
  // accepted/settled boundary and cannot race into a checklist already shown.
  if (normal.length >= ORGANIZATION_MAILBOX_BATCH_THRESHOLD) {
    const selected = normal.slice(0, ORGANIZATION_MAILBOX_BATCH_MAX_ITEMS);
    const batch: OrganizationMailboxBatch = {
      schemaVersion: 1,
      batchId: mailboxBatchId(selected.map(({ envelope }) => envelope)),
      envelopes: selected.map(({ envelope }) => envelope),
    };
    try {
      if (!isCurrent()) return delivered;
      pi.sendMessage({ customType: MESSAGE_TYPE, content: mailboxBatchContext(batch, context.personId, role), display: true, details: batch }, queuedPiDelivery("followUp"));
    } catch (error) {
      appendOrganizationEvent(context, { event: "message-batch-delivery-deferred", batchId: batch.batchId, personId: context.personId, count: selected.length, error: safeExceptionMessage(error), at: new Date().toISOString() });
      logOrganizationException(context, "organization-mailbox-batch-delivery", error, { batchId: batch.batchId, count: selected.length });
        return delivered;
    }
    for (const { file, envelope } of selected) {
      deliveryAttempts.add(file);
      appendOrganizationEvent(context, { event: "message-batch-queue-requested", batchId: batch.batchId, id: envelope.id, personId: context.personId, at: new Date().toISOString() });
    }
    delivered += selected.length;
  } else {
    for (const { file, envelope } of normal) {
      if (!isCurrent()) return delivered;
      if (deliveryAttempts.has(file) || deliveryAttempts.size >= ORGANIZATION_MAILBOX_MAX_OUTSTANDING_DELIVERIES) continue;
      try {
        pi.sendMessage({ customType: MESSAGE_TYPE, content: messageContext(envelope, context.personId, role), display: true, details: envelope }, queuedPiDelivery("followUp"));
      } catch (error) {
        appendOrganizationEvent(context, { event: "message-delivery-deferred", id: envelope.id, personId: context.personId, error: safeExceptionMessage(error), at: new Date().toISOString() });
        logOrganizationException(context, "organization-mailbox-delivery", error, { messageId: envelope.id });
        continue;
      }
      deliveryAttempts.add(file);
      delivered += 1;
      appendOrganizationEvent(context, { event: "message-queue-requested", id: envelope.id, personId: context.personId, at: new Date().toISOString() });
      if (!isCurrent()) return delivered;
    }
  }
  return delivered;
}

/**
 * Pi's ExtensionAPI.sendMessage does not return its internal queue promise.
 * This is the first durable proof that the exact custom envelope entered the
 * receiving Pi session, so only this boundary may archive mailbox mail.
 */
export async function archiveStartedOrganizationMailboxMessage(
  message: unknown,
  context: OrganizationRuntimeContext,
): Promise<OrganizationEnvelope | undefined> {
  const candidate = message && typeof message === "object" && !Array.isArray(message)
    ? message as Record<string, unknown>
    : undefined;
  if (!candidate || candidate.customType !== MESSAGE_TYPE) return undefined;
  if (isOrganizationMailboxBatch(candidate.details)) {
    const batch = candidate.details;
    if (batch.batchId !== mailboxBatchId(batch.envelopes)) return undefined;
    const doc = await readMailboxDoc(context, context.personId);
    if (!doc) return undefined;
    const entries = await Promise.all(batch.envelopes.map(async (envelope) => {
      if (envelope.schemaVersion !== SCHEMA_VERSION || envelope.organization !== context.organization
        || !envelope.recipients.includes(context.personId)
        ) return undefined;
      const entry = findMailboxEntryByMessageId(context, doc, envelope.id, ["pending"]);
      return entry && messageContentMatches(entry.envelope, envelope)
        ? { key: entry.key, envelope }
        : undefined;
    }));
    // One corrupt/stale member cannot turn a batch receipt into partial loss.
    if (entries.some((entry) => !entry)) return undefined;
    for (const { envelope } of entries as Array<{ key: string; envelope: OrganizationEnvelope }>) {
    }
    const accepted = await settleMailboxBatch(context, context.personId, entries as Array<{ key: string; envelope: OrganizationEnvelope }>, "accepted");
    if (!accepted) return undefined;
    for (const envelope of accepted) {
      appendOrganizationEvent(context, {
        event: "message-accepted",
        id: envelope.id,
        personId: context.personId,
        batchId: batch.batchId,
        at: new Date().toISOString(),
      });
      if (launcherSystemNoticePresentation(envelope)?.blocked === "unknown") {
        await appendOrganizationEventOnce(context, `unrecognized-system-notice:${envelope.id}`, {
          event: "unrecognized-system-notice",
          id: envelope.id,
          healthIncidentKind: envelope.healthIncident?.kind,
          recipientPersonId: context.personId,
          batchId: batch.batchId,
          at: new Date().toISOString(),
        });
      }
    }
    return accepted[0];
  }
  const envelope = candidate.details as OrganizationEnvelope | undefined;
  if (!envelope || envelope.schemaVersion !== SCHEMA_VERSION || envelope.organization !== context.organization
    || !envelope.recipients.includes(context.personId)) return undefined;
  const doc = await readMailboxDoc(context, context.personId);
  const entry = doc ? findMailboxEntryByMessageId(context, doc, envelope.id, ["pending"]) : undefined;
  if (!entry) return envelope;
  const persisted = entry.envelope;
  if (!messageContentMatches(persisted, envelope)) return undefined;
  // Acceptance is the pending→accepted move under one CAS commit; a crash
  // leaves either a retryable pending envelope or an accepted one the health
  // monitor can recover (property #2).
  await settleMailboxEntry(context, context.personId, entry.key, "accepted");
  appendOrganizationEvent(context, {
    event: "message-accepted",
    id: envelope.id,
    personId: context.personId,
    at: new Date().toISOString(),
  });
  // AC9 (#8/#103 unhandled-kind contract): a launcher notice this launcher
  // version does not recognize renders as a bodyless neutral "⚙️ System notice"
  // — its opaque body may carry provider tokens, so the body is deliberately
  // never shown (the no-token-leak contract). But a SILENT drop is exactly how a
  // working people-check producer stayed invisible for weeks. Make it LOUD:
  // count each unrecognized notice ONCE, at this durable acceptance boundary,
  // so the next hidden producer surfaces in .chief/bus/events.jsonl. This must NOT live
  // on the render path — `launcherSystemNoticePresentation` runs inside the
  // MESSAGE_TYPE renderer, which repeats on every expand/collapse/redraw, so an
  // event there would spam the bus and burn idle CPU. Metadata only: no body, so
  // the no-token-leak assertions on the render path stay intact.
  if (launcherSystemNoticePresentation(persisted)?.blocked === "unknown") {
    await appendOrganizationEventOnce(context, `unrecognized-system-notice:${persisted.id}`, {
      event: "unrecognized-system-notice",
      id: persisted.id,
      healthIncidentKind: persisted.healthIncident?.kind,
      recipientPersonId: context.personId,
      at: new Date().toISOString(),
    });
  }
  return persisted;
}

// TOMBSTONE (#751/P4): the reflection delivery/retry machinery lived here --
// `ReflectionDeliveryAttempt`, `ReflectionRetryReason`, `ReflectionDeliveryPhase`,
// `ReflectionRecoveryKind`, `ReflectionDeliveryState`, `pruneReflectionDeliveryStates`,
// `currentPendingReflectionRequest`, `reflectionRetryLead`, and the exported
// `reconcileSettledOrganizationActivity` that pushed a bounded-handoff prompt
// into the pane on every settled turn. There is no handoff to prompt for, so
// the prompt, its delivery-acceptance receipts, and its bounded retries are all
// deleted. A settled turn now only reconciles the runtime.

/**
 * Does this person still have durable work waiting?
 *
 * With goals deleted, the mailbox IS the work queue: a pending envelope is a
 * message nobody has read yet, and it is the only durable claim on a person's
 * attention that outlives their Pi session.
 *
 * A TRANSIENT transport failure ("chiefd docstore unreachable/returned 5xx" —
 * the store did not answer THIS INSTANT) must not propagate. This is the
 * settle-time gate (`_emitAgentSettled` -> processMaintenance ->
 * processSessionMaintenance), and letting a chiefd blip escape here crashes
 * the agent's whole maintenance cycle — strictly worse than skipping one cycle
 * and letting the reactive floor catch up on the next. Structural check
 * (E4-S8): never a message regex.
 */
export async function hasOpenOrganizationWork(context: OrganizationRuntimeContext): Promise<boolean> {
  try {
    const doc = await readMailboxDoc(context, context.personId);
    return Object.keys(doc?.pending ?? {}).length > 0;
  } catch (error) {
    if (error instanceof ChiefdUnavailableError || error instanceof OrgRowRefusalError) {
      appendOrganizationEvent(context, {
        event: "session-maintenance-open-work-check-degraded",
        personId: context.personId,
        error: safeExceptionMessage(error),
        at: new Date().toISOString(),
      });
      return false;
    }
    throw error;
  }
}

function maintenanceCard(request: SessionMaintenanceRequest, phase: "queued" | "running" | "completed" | "failed" | "skipped") {
  const content = request.action === "compact" ? "Context maintenance"
    : "Session maintenance";
  return {
    customType: "organization-session-maintenance",
    content,
    display: true,
    details: { request, phase },
  } as const;
}

// #360: this used to write one scrollback card per phase transition
// (queued, then running, then completed/failed/skipped) — up to three
// cards for one logical maintenance operation, growing scrollback on every
// state tick (the exact thing CLAUDE.md's performance principle forbids).
// Pi's `sendMessage` (like `appendEntry`) is a fire-and-forget append to the
// session's transcript log — there is no API in this Pi runtime to edit or
// replace a message already delivered, so a literal "live-updating" card is
// not available here. The fix that is actually achievable — and what the
// reader needs — is to show the SETTLED outcome exactly once: nothing is
// written for the transient "queued"/"running" phases, and the terminal
// phase (completed/failed/skipped) is the operation's one card.
const MAINTENANCE_CARD_PHASES = new Set(["completed", "failed", "skipped"]);

/**
 * One announcement per request per phase, for the life of this Pi process.
 *
 * `finish` is IDEMPOTENT at the store — finishing an already-terminal request
 * REPLAYS it and returns the same record with a 200 (session_maintenance_ops.rs
 * `finish`: "if request.status.is_terminal() … return Ok(request.clone())").
 * That is correct there and it is exactly why the card cannot be drawn from the
 * return value alone: the extension finishes a compaction from more than one
 * place ON PURPOSE — the native completion callback, and the recovery branch
 * that finds a proven compaction whose callback never arrived — so a single
 * compaction legitimately produces two finishes and, before this, two identical
 * cards.
 *
 * Measured on a live box, 2026-08-20: request
 * `session-maintenance:e800bbc6…` for `eng-engineer-2`, one compaction,
 * `POST /v1/org/session-maintenance/finish` at 17:16:24 and again at 17:16:25,
 * and the operator's pane showing `Context compacted · @eng-engineer-2` twice
 * in a row under one `[compaction]` block.
 *
 * The dedupe is keyed on the REQUEST id and phase rather than on a "did I just
 * finish it" flag, because the second finish may come from a different code
 * path minutes later, and a request legitimately shows more than one phase
 * (`queued` → `running` → `completed`). Process-scoped is the right lifetime: a
 * fresh Pi replaying a completed request at startup has never announced it to
 * the person now reading the pane, and should.
 */
const announcedMaintenanceCards = new Set<string>();

/**
 * Whether this phase of this request has still to be announced — and record it.
 *
 * Separated from the send so the rule is testable without a Pi: the defect was
 * never in the rendering, it was in announcing the same durable record twice.
 */
export function shouldAnnounceMaintenanceCard(
  announced: Set<string>,
  request: Pick<SessionMaintenanceRequest, "id">,
  phase: "queued" | "running" | "completed" | "failed" | "skipped",
): boolean {
  if (!MAINTENANCE_CARD_PHASES.has(phase)) return false;
  const key = `${request.id}:${phase}`;
  if (announced.has(key)) return false;
  announced.add(key);
  return true;
}

function showMaintenanceCard(pi: Pick<ExtensionAPI, "sendMessage">, request: SessionMaintenanceRequest, phase: "queued" | "running" | "completed" | "failed" | "skipped") {
  if (!shouldAnnounceMaintenanceCard(announcedMaintenanceCards, request, phase)) return;
  try { pi.sendMessage(maintenanceCard(request, phase), { deliverAs: "nextTurn" }); } catch { /* cards are best-effort only */ }
}

/**
 * The pane-failure card is APPENDED, never sent.
 *
 * It was `pi.sendMessage(..., { deliverAs: "nextTurn" })`, and that mode parks
 * the card in Pi's `_pendingNextTurnMessages` array, whose only reader is the
 * next prompt submission. Both cards this helper serves describe a pane whose
 * next turn CANNOT run — an unconfigured provider cannot start one, and a
 * request that overflows the window overflows it again on every retry — so the
 * card was queued behind the very turn it exists to explain, and the operator
 * read the raw provider dump instead. Measured on a live company: zero
 * occurrences of the explanation in the pane, thirty-six of the raw 400.
 *
 * The other modes are worse rather than better. `steer`/`followUp` are what a
 * bare `sendMessage` does while the agent run is still active, and it still is
 * inside `agent_end` — those queue a continuation, so a card about an
 * unretryable failure would itself provoke the retry. `appendEntry` is the one
 * append that paints the transcript unconditionally: no turn, no queue, and no
 * place in LLM context, which is right for a statement addressed to the person
 * reading the pane rather than to the model.
 */
function showPaneFailureCard(pi: Pick<ExtensionAPI, "appendEntry">, details: {
  personId: string;
  logPath: string;
  provider?: string;
  requested?: number;
  limit?: number;
  contentFiltered?: boolean;
  consumedDeliveries?: number;
  insufficientCredits?: boolean;
  printedToolCalls?: number;
}) {
  try { pi.appendEntry(PANE_FAILURE_TYPE, details); } catch { /* cards are best-effort only */ }
}

// A refusal is a sentence, not a catalog dump; the tail says how many more.
const EMPTY_SESSION_COMPACTION_ANCHOR = "<session-root>";

// TOMBSTONE: `COMPANY_NATIVE_RESET_MARKER`, the custom entry a native session
// replacement wrote so a later boot could prove which reset had landed. Native
// reset is deleted; nothing writes or reads it.

// Keep this byte-for-byte aligned with the durable session-maintenance
// recovery fence. A marker-bearing replacement that is itself interrupted
// cannot safely be credited by a later Pi process (its per-process claim
// token is intentionally ephemeral), but it is authoritative evidence that
// the old attempt reached native replacement and needs one fenced successor.
const SESSION_MAINTENANCE_PROCESS_INTERRUPTION_ERROR = "The Pi process ended before session maintenance completed; the durable attempt was recovered on the next exact runtime startup.";

// TOMBSTONE: `resolveModelForRequest` and `unknownModelMessage` — the model
// resolver and its not-found sentence. Both were reachable only from the
// tombstoned `set_model` verb and had ZERO production callers; chief does not
// choose models, Pi owns that. Deleted rather than left in place: a complete,
// tested model-resolution utility sitting ready is what makes "just let hire
// pick a model" an afternoon of wiring instead of a decision.
// `MAX_LISTED_MODELS` went with them as their only reader.

// TOMBSTONE: `nativeResetPersistenceProof` and
// `hasPersistedInterruptedNativeResetMarker` — both read the native-reset
// marker entry out of the session transcripts to tell a completed replacement
// from an interrupted one. Native reset is deleted, so nothing writes that
// marker and there is nothing left to prove.

/**
 * The ONE question every native-compaction receipt asks: is this the compaction
 * entry Pi wrote for the request we claimed?
 *
 * It is answered by the entry's PARENT, and by nothing else. `appendCompaction`
 * sets `parentId: this.leafId`, and the intercom records that same leaf as
 * `compactAnchorEntryId` at claim time, so an entry hanging off the anchor can
 * only have been created by the compaction this request asked for.
 *
 * ## The witness this replaced, and why it could never be satisfied
 *
 * Both receipt paths used to additionally require `entry.fromHook === true`.
 * That field does not mean "a compaction happened". `9361d097d` established
 * what it does mean, from both implementations: Pi's `AgentSession` and
 * `AgentHarness` each set `fromExtension`/`fromHook` to `true` ONLY when a
 * `session_before_compact` handler returned a `compaction`, and pass that one
 * boolean to `appendCompaction`, which persists it under the single `fromHook`
 * field. It claims "AN EXTENSION SUPPLIED THE SUMMARY".
 *
 * No extension in this repository registers `session_before_compact`. The
 * intercom's own compact call hands `customInstructions` to PI's summarizer, so
 * demanding an extension-supplied summary contradicted the call it was
 * receipting. The predicate was therefore unsatisfiable on every host, tmux
 * included: Pi compacted, the receipt refused to recognise its own compaction,
 * and the next session start terminalised the request `failed` while the work
 * was done. A failure shape covering a success costs exactly what the inverse
 * does.
 *
 * Nothing replaces it. `parentId` was always the fact that answers the
 * question, and one fact gets one answer-holder.
 */
function isAnchoredNativeCompactionEntry(
  entry: { type?: unknown; id?: unknown; parentId?: unknown },
  anchorParentId: string | null,
): entry is { type: "compaction"; id: string; parentId: unknown } {
  return entry.type === "compaction" && entry.parentId === anchorParentId
    && typeof entry.id === "string" && entry.id.length > 0;
}

export function nativeCompactionProof(
  extensionContext: ExtensionContext | undefined,
  request: SessionMaintenanceRequest,
): { state: "absent" | "proven" | "ambiguous"; entryId?: string } {
  if (request.action !== "compact" || !request.compactSessionId || !request.compactAnchorEntryId) return { state: "absent" };
  const sessionManager = sessionManagerOf(extensionContext);
  if (sessionManager?.getSessionId?.() !== request.compactSessionId) return { state: "ambiguous" };
  const entries = (sessionManager?.getEntries?.() ?? []) as Array<{
    type?: unknown; id?: unknown; parentId?: unknown;
  }>;
  const parentId = request.compactAnchorEntryId === EMPTY_SESSION_COMPACTION_ANCHOR ? null : request.compactAnchorEntryId;
  const proofs = entries.filter((entry) => isAnchoredNativeCompactionEntry(entry, parentId));
  if (proofs.length === 1) return { state: "proven", entryId: proofs[0]!.id as string };
  if (proofs.length > 1) return { state: "ambiguous" };
  const anchorIndex = parentId === null ? -1 : entries.findIndex((entry) => entry.id === parentId);
  if (parentId !== null && anchorIndex < 0) return { state: "ambiguous" };
  return entries.length > anchorIndex + 1 ? { state: "ambiguous" } : { state: "absent" };
}

function clearSessionMaintenanceDeferralRetry(retry: SessionMaintenanceDeferralRetry): void {
  retry.requestId = undefined;
  retry.claim = undefined;
  retry.failures = 0;
  retry.nextAttemptAt = 0;
  retry.reported = false;
}

/**
 * A stale turn can be discovered after its durable start command committed.
 * Returning that exact own claim to queued is mandatory, not best-effort: a
 * pre-mutation launcher lock may outlive runChecked's bounded command retries,
 * so retain a tiny in-process retry record and let later polls finish the same
 * claim release even while the newer agent turn is active.
 */
async function retrySessionMaintenanceDeferral(
  context: OrganizationRuntimeContext,
  retry: SessionMaintenanceDeferralRetry,
  now: () => number,
): Promise<boolean> {
  const requestId = retry.requestId;
  const claim = retry.claim;
  if (!requestId || !claim) return false;
  const projection = await projectSessionMaintenanceForRuntime(context);
  const queued = projection.queued?.id === requestId ? projection.queued : undefined;
  const ownRunning = projection.running.find((request) => request.id === requestId
    && request.claimedProcessId === claim.processId
    && request.claimedSessionId === claim.sessionId
    && request.claimToken === claim.claimToken);
  const recordDeferred = (request: SessionMaintenanceRequest, responseRecovered = false) => {
    // The durable ledger has already released the exact claim. Clear the
    // in-process authority first: diagnostics are best-effort and must never
    // keep a successfully deferred request from being processed again.
    clearSessionMaintenanceDeferralRetry(retry);
    appendOrganizationEvent(context, {
      event: responseRecovered ? "session-maintenance-turn-defer-response-recovered" : "session-maintenance-turn-deferred",
      requestId: request.id,
      action: request.action,
      personId: context.personId,
      at: new Date().toISOString(),
    });
  };
  if (queued) {
    recordDeferred(queued, retry.failures > 0);
    return true;
  }
  if (!ownRunning) {
    // Another exact terminal/recovery transition already resolved ownership.
    clearSessionMaintenanceDeferralRetry(retry);
    return true;
  }
  if (now() < retry.nextAttemptAt) return true;
  try {
    const deferred = await sessionMaintenanceCommand(context, "defer", {
      requestId,
      processId: claim.processId,
      sessionId: claim.sessionId,
      claimToken: claim.claimToken,
    }) as SessionMaintenanceRequest;
    recordDeferred(deferred);
  } catch (error) {
    const after = await projectSessionMaintenanceForRuntime(context);
    const committed = after.queued?.id === requestId ? after.queued : undefined;
    if (committed) {
      recordDeferred(committed, true);
      return true;
    }
    const stillOwned = after.running.some((request) => request.id === requestId
      && request.claimedProcessId === claim.processId
      && request.claimedSessionId === claim.sessionId
      && request.claimToken === claim.claimToken);
    if (!stillOwned) {
      clearSessionMaintenanceDeferralRetry(retry);
      return true;
    }
    const delay = ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS[
      Math.min(retry.failures, ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS.length - 1)
    ]!;
    retry.failures += 1;
    retry.nextAttemptAt = now() + delay;
    if (!retry.reported) {
      const retryable = isExpectedLifecycleProjectionError(error);
      appendOrganizationEvent(context, {
        event: "session-maintenance-turn-defer-retry",
        requestId,
        personId: context.personId,
        retryable,
        error: safeExceptionMessage(error),
        at: new Date().toISOString(),
      });
      if (!retryable) logOrganizationException(context, "session-maintenance-turn-defer", error, { requestId });
      retry.reported = true;
    }
  }
  return true;
}

/**
 * A target Pi polls its own durable requests but executes only while idle with
 * no fenced work. Compaction runs through Pi's verified API; a fresh session
 * asks the tagged reconciler to
 * respawn that single owned pane without --session. Nothing deletes JSONL,
 * private goals, or any unowned runtime pane.
 */
async function processSessionMaintenance(
  pi: Pick<ExtensionAPI, "sendMessage" | "setModel">,
  context: OrganizationRuntimeContext,
  extensionContext: ExtensionContext | undefined,
  claimToken: string,
  retry: SessionMaintenanceStartRetry,
  now: () => number,
  lifecycleFence: SessionMaintenanceLifecycleFence,
  lifecycleLease: SessionMaintenanceLifecycleLease | undefined,
  deferralRetry: SessionMaintenanceDeferralRetry,
  nativeCompaction: NativeCompactionLease,
  onNativeCompactionFinished: () => void,
): Promise<boolean> {
  if (deferralRetry.requestId) {
    await retrySessionMaintenanceDeferral(context, deferralRetry, now);
    return false;
  }
  if (nativeCompaction.requestId) return false;
  if (!extensionContext || !lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return false;
  const maintenance = await projectSessionMaintenanceForRuntime(context);
  const candidate = maintenance.running.at(-1) ?? maintenance.queued;
  // TOMBSTONE: `liveApply` was true for `set_model`, the one action that
  // applied inside a live turn rather than at a boundary. No action does that
  // now, so the distinction has nothing to distinguish.
  const liveApply = false;
  // A pre-turn lease authorizes exactly ONE action, and it deliberately skips
  // the settled-work wait below.
  //
  // Only `compact`, because this runs inside Pi's own `prompt()` call: a fresh
  // session would dispose the very session Pi is building a prompt for, and it
  // has no deadlock of this shape anyway.
  //
  // And no settled-work wait, because that gate is what makes the deadlock
  // survive the fix. It asks "is other work in flight" — but the pane it asks
  // about cannot CLOSE that work without a turn, and cannot take a turn
  // without compacting. Waiting for open work to settle is waiting for the
  // thing the compaction is a precondition of. The gate's real purpose,
  // "never rewrite the transcript underneath live work", is better served
  // here than at settle: no tool is mid-flight (the fence checks that), no
  // agent run is active, and Pi is blocked awaiting this handler.
  const preTurn = lifecycleLease?.boundary === "pre-turn";
  if (preTurn && candidate?.action !== "compact") return false;
  if (!preTurn && !liveApply && await hasOpenOrganizationWork(context) && !candidate?.companyActionId) return false;
  const claim = sessionMaintenanceClaim(extensionContext, claimToken);
  if (maintenance.applying && claim) {
    const applying = maintenance.applying;
    if (retry.requestId !== applying.id) {
      retry.requestId = applying.id;
      retry.failures = 0;
      retry.nextAttemptAt = 0;
    }
    if (now() < retry.nextAttemptAt) return false;
    const payload = {
      requestId: applying.id,
      processId: claim.processId,
      sessionId: claim.sessionId,
      claimToken: claim.claimToken,
    };
    let completed: SessionMaintenanceRequest | undefined;
    try {
      const result = await sessionMaintenanceCommand(context, "complete", payload) as { request?: SessionMaintenanceRequest } | SessionMaintenanceRequest | undefined;
      completed = result && typeof result === "object" && "request" in result ? result.request : result as SessionMaintenanceRequest | undefined;
    } catch (error) {
      try {
        const result = await sessionMaintenanceCommand(context, "complete", payload) as { request?: SessionMaintenanceRequest } | SessionMaintenanceRequest | undefined;
        completed = result && typeof result === "object" && "request" in result ? result.request : result as SessionMaintenanceRequest | undefined;
        appendOrganizationEvent(context, {
          event: "session-maintenance-fresh-session-completion-response-recovered",
          requestId: applying.id,
          personId: context.personId,
          at: new Date().toISOString(),
        });
      } catch (retryError) {
        const delay = ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS[
          Math.min(retry.failures, ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS.length - 1)
        ]!;
        retry.failures += 1;
        retry.nextAttemptAt = now() + delay;
        const retryable = isExpectedLifecycleProjectionError(retryError);
        appendOrganizationEvent(context, {
          event: "session-maintenance-fresh-session-completion-deferred",
          requestId: applying.id,
          personId: context.personId,
          retryable,
          error: safeExceptionMessage(retryError),
          at: new Date().toISOString(),
        });
        if (!retryable && retry.failures === 1) {
          logOrganizationException(context, "session-maintenance-complete-deferred", retryError, {
            requestId: applying.id,
            initialError: safeExceptionMessage(error),
          });
        }
        return false;
      }
    }
    retry.requestId = undefined;
    retry.failures = 0;
    retry.nextAttemptAt = 0;
    if (completed) showMaintenanceCard(pi, completed, "completed");
    return Boolean(completed);
  }
  const ownedRunningCompact = claim ? maintenance.running.find((request) => (
    request.action === "compact"
      && request.claimedProcessId === claim.processId
      && request.claimedSessionId === claim.sessionId
      && request.claimToken === claim.claimToken
  )) : undefined;
  // A pre-mutation apply failure leaves the exact source claim running. It is
  // higher priority than unrelated queued work and must be retried by this
  // same installation instead of waiting forever for a process replacement.
  const projected = ownedRunningCompact ?? maintenance.queued;
  if (!projected || !claim) {
    if (!projected) {
      retry.requestId = undefined;
      retry.failures = 0;
      retry.nextAttemptAt = 0;
    }
    return false;
  }
  if (retry.requestId !== projected.id) {
    retry.requestId = projected.id;
    retry.failures = 0;
    retry.nextAttemptAt = 0;
  }
  if (now() < retry.nextAttemptAt) return false;
  const deferRetry = () => {
    const delay = ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS[Math.min(retry.failures, ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS.length - 1)]!;
    retry.failures += 1;
    retry.nextAttemptAt = now() + delay;
  };
  const ownCommittedClaim = async (): Promise<SessionMaintenanceRequest | undefined> => (await projectSessionMaintenanceForRuntime(context)).running.find((request) => request.id === projected.id
    && request.claimedProcessId === claim.processId
    && request.claimedSessionId === claim.sessionId
    && request.claimToken === claim.claimToken);
  const deferStaleClaim = async (request: SessionMaintenanceRequest): Promise<boolean> => {
    deferralRetry.requestId = request.id;
    deferralRetry.claim = claim;
    deferralRetry.failures = 0;
    deferralRetry.nextAttemptAt = 0;
    deferralRetry.reported = false;
    await retrySessionMaintenanceDeferral(context, deferralRetry, now);
    return false;
  };
  // This check is intentionally adjacent to the async durable claim. All
  // prior projection/backoff work is synchronous, so a stale settled handler
  // cannot enter the launcher mutation after a newer Pi lifecycle starts.
  if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return false;
  let started: SessionMaintenanceRequest | undefined = projected.status === "running" ? projected : undefined;
  try {
    if (!started) {
      started = await sessionMaintenanceCommand(context, "start", {
        requestId: projected.id,
        // REQUIRED non-`Option` on `/v1/org/session-maintenance/start` and
        // absent from this payload for as long as the CLI stood between the
        // two: chiefd claims the next request OF THIS ACTION, so a missing
        // one is not a default, it is a refusal.
        action: projected.action,
        processId: claim.processId,
        sessionId: claim.sessionId,
        claimToken: claim.claimToken,
        ...(projected.action === "compact" ? {
          compactSessionId: claim.sessionId,
          compactAnchorEntryId: sessionManagerOf(extensionContext)?.getLeafId?.() ?? EMPTY_SESSION_COMPACTION_ANCHOR,
        } : {}),
      }) as SessionMaintenanceRequest | undefined;
    }
  } catch (error) {
    // The launcher can atomically commit the claim and then lose its stdout or
    // exit status. Disk is the authority: continue only when the exact request
    // carries this process/session/install token. Waiting for a restart would
    // otherwise strand a live compaction indefinitely.
    started = await ownCommittedClaim();
    if (started) {
      appendOrganizationEvent(context, {
        event: "session-maintenance-start-response-recovered",
        requestId: started.id,
        personId: context.personId,
        at: new Date().toISOString(),
      });
    } else {
      deferRetry();
      if (isExpectedLifecycleProjectionError(error)) {
        appendOrganizationEvent(context, {
          event: "session-maintenance-start-deferred",
          personId: context.personId,
          retryable: true,
          error: safeExceptionMessage(error),
          at: new Date().toISOString(),
        });
        return false;
      }
      throw error;
    }
  }
  if (!started) {
    started = await ownCommittedClaim();
    if (!started) {
      deferRetry();
      return false;
    }
  }
  if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return deferStaleClaim(started);
  {
    showMaintenanceCard(pi, started, "running");
    appendOrganizationEvent(context, { event: "session-maintenance-started", requestId: started.id, action: started.action, personId: context.personId, at: new Date().toISOString() });
  }
  // TOMBSTONE: the `set_model` arm. Deleted with the tool that was its only
  // source, completing the standing decision that removed `org_set_thinking`
  // along with provider/model management — an agent's model is Pi's own
  // setting, not chief's.
  if (started.action === "compact") {
    retry.requestId = undefined;
    retry.failures = 0;
    retry.nextAttemptAt = 0;
    if (typeof extensionContext.compact !== "function") {
      await sessionMaintenanceCommand(context, "finish", { requestId: started.id, status: "failed", error: "This Pi runtime does not expose the verified compaction API." });
      return false;
    }
    const existingProof = nativeCompactionProof(extensionContext, started);
    if (existingProof.state === "proven") {
      const completed = await sessionMaintenanceCommand(context, "finish", {
        requestId: started.id,
        status: "completed",
        compactEntryId: existingProof.entryId,
      }) as SessionMaintenanceRequest;
      showMaintenanceCard(pi, completed, "completed");
      return true;
    }
    if (existingProof.state === "ambiguous") {
      const failed = await sessionMaintenanceCommand(context, "finish", {
        requestId: started.id,
        status: "failed",
        error: "Native compaction receipt diverged from the persisted Pi session anchor; refusing to compact twice.",
      }) as SessionMaintenanceRequest;
      showMaintenanceCard(pi, failed, "failed");
      return true;
    }
    // JS cannot interleave another lifecycle callback between this synchronous
    // predicate and compact(). A tool_execution_end alone is insufficient:
    // its epoch stays unsettled until the subsequent toolResult is persisted
    // and Pi emits a new agent_settled boundary.
    if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return deferStaleClaim(started);
    // Resolved unconditionally and before the ownership check below: a
    // pre-turn caller awaiting this promise must be released by ANY exit from
    // this compaction, including one that no longer owns the lease. A promise
    // that only settles on the happy path is a wedge, not a guard.
    let releasePreTurnWait: () => void = () => {};
    nativeCompaction.settled = new Promise<void>((resolve) => { releasePreTurnWait = resolve; });
    const releaseNativeCompaction = () => {
      releasePreTurnWait();
      if (nativeCompaction.requestId !== started!.id) return;
      nativeCompaction.requestId = undefined;
      nativeCompaction.sessionId = undefined;
      nativeCompaction.anchorEntryId = undefined;
      nativeCompaction.completedEntryId = undefined;
      nativeCompaction.settled = undefined;
      onNativeCompactionFinished();
    };
    nativeCompaction.requestId = started.id;
    nativeCompaction.sessionId = started.compactSessionId;
    nativeCompaction.anchorEntryId = started.compactAnchorEntryId;
    nativeCompaction.completedEntryId = undefined;
    try {
      extensionContext.compact({
        customInstructions: "Preserve durable commitments, verified progress, and the exact next step. Omit routine chatter and secrets.",
        onComplete: () => {
          // Pi's callback is the first proof that its asynchronous summary and
          // branch rebuild finished. Release turn-triggering delivery before
          // the independent durable completion receipt is written.
          releaseNativeCompaction();
          void (async () => {
            try {
              const proof = nativeCompactionProof(extensionContext, started!);
              const compactEntryId = nativeCompaction.completedEntryId ?? proof.entryId;
              if (!compactEntryId || proof.state === "ambiguous") throw new Error("Native compaction completed without one exact anchored Pi compaction entry");
              const completed = await sessionMaintenanceCommand(context, "finish", {
                requestId: started!.id,
                status: "completed",
                compactEntryId,
              }) as SessionMaintenanceRequest;
              showMaintenanceCard(pi, completed, "completed");
              appendOrganizationEvent(context, { event: "session-maintenance-completed", requestId: completed.id, action: completed.action, personId: context.personId, at: new Date().toISOString() });
            } catch (error) {
              logOrganizationException(context, "session-maintenance-complete-deferred", error, { requestId: started!.id });
            }
          })();
        },
        onError: (error) => {
          releaseNativeCompaction();
          void (async () => {
            try {
              if (isNothingToCompact(error)) {
                const skipped = await sessionMaintenanceCommand(context, "finish", { requestId: started!.id, status: "skipped" }) as SessionMaintenanceRequest;
                showMaintenanceCard(pi, skipped, "skipped");
                appendOrganizationEvent(context, {
                  event: "session-maintenance-skipped",
                  requestId: skipped.id,
                  action: skipped.action,
                  personId: context.personId,
                  reason: "nothing_to_compact",
                  at: new Date().toISOString(),
                });
                return;
              }
              const failed = await sessionMaintenanceCommand(context, "finish", { requestId: started!.id, status: "failed", error: compactionFailureReason(safeExceptionMessage(error)) }) as SessionMaintenanceRequest;
              showMaintenanceCard(pi, failed, "failed");
              logOrganizationException(context, "session-maintenance-compact", error, { requestId: started!.id });
            } catch (finishError) {
              logOrganizationException(context, "session-maintenance-finish-deferred", finishError, { requestId: started!.id });
            }
          })();
        },
      });
    } catch (error) {
      releaseNativeCompaction();
      const failed = await sessionMaintenanceCommand(context, "finish", { requestId: started.id, status: "failed", error: compactionFailureReason(safeExceptionMessage(error)) }) as SessionMaintenanceRequest;
      showMaintenanceCard(pi, failed, "failed");
      logOrganizationException(context, "session-maintenance-compact", error, { requestId: started.id });
      return false;
    }
    return true;
  }
  if (started.companyActionId) {
    retry.requestId = undefined;
    retry.failures = 0;
    retry.nextAttemptAt = 0;
    return true;
  }
  if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return deferStaleClaim(started);
  const applyPayload = {
    requestId: started.id,
    processId: claim.processId,
    sessionId: claim.sessionId,
    claimToken: claim.claimToken,
  };
  try {
    await sessionMaintenanceCommand(context, "apply", applyPayload);
  } catch (error) {
    // Retry through the launcher boundary. The copied extension never infers
    // authority from a weak direct disk projection and never finishes an
    // ambiguously applied request.
    try {
      await sessionMaintenanceCommand(context, "apply", applyPayload);
      appendOrganizationEvent(context, {
        event: "session-maintenance-fresh-session-apply-response-recovered",
        requestId: started.id,
        personId: context.personId,
        at: new Date().toISOString(),
      });
    } catch (retryError) {
      deferRetry();
      const failure = sessionMaintenanceFailure(retryError);
      const retryable = failure.retryable;
      appendOrganizationEvent(context, {
        event: "session-maintenance-fresh-session-apply-deferred",
        requestId: started.id,
        personId: context.personId,
        retryable,
        error: failure.message,
        ...(failure.refusalCode ? { refusalCode: failure.refusalCode } : {}),
        ...(failure.refusalDetail ? { refusalDetail: failure.refusalDetail } : {}),
        at: new Date().toISOString(),
      });
      if (!retryable && retry.failures === 1) logOrganizationException(context, "session-maintenance-fresh-session-apply", retryError, {
        requestId: started.id,
        initialError: safeExceptionMessage(error),
        ...(failure.refusalCode ? { refusalCode: failure.refusalCode } : {}),
        ...(failure.refusalDetail ? { refusalDetail: failure.refusalDetail } : {}),
      });
      if (failure.terminalStatus) {
        try {
          const failed = await sessionMaintenanceCommand(context, "finish", {
            requestId: started.id,
            status: failure.terminalStatus,
            error: failure.message,
          }) as SessionMaintenanceRequest;
          retry.requestId = undefined;
          retry.failures = 0;
          retry.nextAttemptAt = 0;
          showMaintenanceCard(pi, failed, "failed");
          appendOrganizationEvent(context, {
            event: "session-maintenance-fresh-session-apply-failed",
            requestId: started.id,
            personId: context.personId,
            error: failure.message,
            ...(failure.refusalCode ? { refusalCode: failure.refusalCode } : {}),
            ...(failure.refusalDetail ? { refusalDetail: failure.refusalDetail } : {}),
            at: new Date().toISOString(),
          });
        } catch (finishError) {
          logOrganizationException(context, "session-maintenance-fresh-session-finish-deferred", finishError, {
            requestId: started.id,
            applyError: failure.message,
          });
        }
      }
      return true;
    }
  }
  retry.requestId = undefined;
  retry.failures = 0;
  retry.nextAttemptAt = 0;
  try {
    await reconcileRuntime(context);
  } catch (error) {
    // Applying is already durable. Reconciliation is a
    // self-replacing best-effort nudge; the supervisor and exact new-session
    // startup completion path own recovery from this point forward.
    const retryable = isExpectedFreshSessionSelfReplacement(error);
    appendOrganizationEvent(context, {
      event: "session-maintenance-fresh-session-reconcile-deferred",
      requestId: started.id,
      personId: context.personId,
      error: safeExceptionMessage(error),
      retryable,
      at: new Date().toISOString(),
    });
    if (!retryable) logOrganizationException(context, "session-maintenance-fresh-session-reconcile", error, {
      requestId: started.id,
    });
    return true;
  }
  appendOrganizationEvent(context, {
    event: "session-maintenance-fresh-session-applying",
    requestId: started.id,
    personId: context.personId,
    at: new Date().toISOString(),
  });
  return true;
}

/**
 * Queue the automatic compact when a settled pane is carrying a fat context.
 *
 * **This fires AT SETTLE, not at a pending park, and the difference is the
 * whole of #1230's remaining bug.** The gate used to require a park already in
 * `pendingTransitions`; a routine idle park is minted terminal, so it is never
 * in that list, so the gate never opened once in the product's life. See the
 * tombstone at the request below for the mechanism and the box evidence.
 *
 * Settle is the right trigger on its own merits rather than as a fallback: a
 * routine park cannot be admitted until the settle lease expires, so a compact
 * started here has the whole lease to finish in.
 */
/** The last reason this pane declined a pre-park compaction, so the trail is
 *  one row per state and not one per poll. */
let parkCompactionDeclineReported: string | undefined;

async function queueAutomaticParkCompaction(
  context: OrganizationRuntimeContext,
  extensionContext: ExtensionContext | undefined,
  lifecycleFence: SessionMaintenanceLifecycleFence,
  lifecycleLease: SessionMaintenanceLifecycleLease | undefined,
): Promise<boolean> {
  // EVERY DECLINE SAYS WHICH GATE DECLINED IT.
  //
  // All four early exits used to `return false` in silence, so a window with
  // zero compactions could not be told from a window where the function never
  // qualified. Measured on a live box 14:39–14:56, with the 403 and the
  // fence both fixed: twelve parks completed — including a person at ~90%
  // context — with ZERO auto-compact rows AND ZERO error rows. The record could
  // not say why, and that silence was the whole of the remaining investigation.
  //
  // Bounded: at most one per person per settle cycle, cleared by a pass that
  // actually queues. A decline every poll would bury the trail it exists to
  // leave.
  const decline = (reason: string): false => {
    if (parkCompactionDeclineReported !== reason) {
      parkCompactionDeclineReported = reason;
      appendOrganizationEvent(context, {
        event: "park-compaction-declined",
        personId: context.personId,
        reason,
        at: new Date().toISOString(),
      });
    }
    return false;
  };
  if (!extensionContext) return decline("no-extension-context");
  if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) return decline("fence-stale");
  // ORDERING QUESTION, RECORDED RATHER THAN RESOLVED: pending mail declines the
  // compaction, and a person about to PARK with a fat context arguably should
  // compact anyway — the park itself is evidence that the pending mail did not
  // create a turn. Changing the order is a product decision about what a park
  // means, not a bug fix, so this change only makes the decline VISIBLE. See
  // the plan.
  if (await hasOpenOrganizationWork(context)) return decline("open-work");
  if (typeof extensionContext.getContextUsage !== "function") {
    return decline("usage-unavailable");
  }
  const usage = extensionContext.getContextUsage();
  if ((usage?.percent ?? 0) <= 50) return decline("usage-low");
  try {
    // NO RECONCILE NUDGE HERE, and this is the whole fix.
    //
    // `reconcileRuntime` POSTs `/v1/org/runtime/launch`, whose subject is the
    // WHOLE COMPANY — it starts every person the manifest wants up — so
    // `require_company_wide_authority` grants it only to the head of the root
    // department. This function runs in EVERY person's pane. So for everybody
    // except the CEO the call answered `403 caller-out-of-company-scope` and
    // the catch below returned false before `auto-compact` was ever requested:
    // the >50%-context compact-before-park was dead product-wide, and the only
    // trace was 590 `automatic-park-compaction-deferred` rows that read like
    // noise. Measured on a live box 2026-08-24, still firing at 93 a
    // day, and cross-confirmed by 592 refused launches in the daemon's own log.
    //
    // THE GUARD IS RIGHT AND THE CALLER WAS WRONG. A leaf person may not start
    // the whole company, and this file has already learned that once: the
    // `org_send` tombstone below records the identical route, the identical
    // refusal and the identical cause. This call site survived that sweep.
    //
    // Nothing replaces it. The nudge only asked chiefd to recompute pending
    // transitions slightly sooner; chiefd's reconcile duty computes them on its
    // own cadence and this function runs on every converge pass, so the cost of
    // reading status directly is at most one pass of delay in noticing a park —
    // against a compaction that never happened at all.
    // **THE PARK GATE IS GONE, BECAUSE ITS QUESTION HAD NO YES.**
    //
    // This used to read activity status and require a PENDING park transition
    // before compacting. A routine idle park is born TERMINAL:
    // `begin_transition` mints it `TransitionStatus::Forced` with
    // `handoff_deadline_at` set to the admission instant, under a comment
    // stating it outright — *"A ROUTINE IDLE PARK IS BORN TERMINAL. There is no
    // window between admitting it and the pane going away… `is_pending()` is
    // false."* And `pending_transitions` returns only
    // `AwaitingHandoff | Overdue`. So a routine idle park NEVER appears in
    // `pendingTransitions`, and the gate could not open — ever.
    //
    // Measured on a live box: every all-time decline is
    // `no-pending-park` or `usage-low`, and the automatic compact's reason
    // string appears ZERO times in the whole bus. The feature had never once
    // fired. The gate was satisfiable only for an INTENT-BEARING park — an
    // operator's or a lifecycle command's, which are minted `AwaitingHandoff` —
    // and it was wired to a routine one.
    //
    // **The timing is now a positive argument rather than a compromise.** A
    // routine park cannot be admitted until the settle lease expires, so
    // compacting HERE — at settle — gives the compaction the full
    // `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` to finish before any park can
    // exist. That is the window the original design was reaching for, obtained
    // by not waiting for a signal that never arrives.
    //
    // Two properties make it safe to fire on every qualifying settle. It is
    // SELF-QUIETING: a completed compaction drops usage below the 50% gate, so
    // it does not re-fire until the context grows back. And it is safe to
    // INTERRUPT: a turn arriving mid-compaction beats `working:true` and the
    // queued-prompt path resumes, which #1230 already covers.
    if (!lifecycleFence.isCurrent(lifecycleLease, extensionContext)) {
      return decline("fence-stale-at-request");
    }
    await sessionMaintenanceCommand(context, "auto-compact", {
      reason: "Automatic compact at settle, inside the idle-park lease.",
    });
    // A pass that QUEUED re-arms the decline trail: the next silence is new
    // information rather than a repeat somebody has already read.
    parkCompactionDeclineReported = undefined;
    return true;
  } catch (error) {
    if (isExpectedLifecycleProjectionError(error)) {
      appendOrganizationEvent(context, {
        event: "automatic-park-compaction-deferred",
        personId: context.personId,
        retryable: true,
        error: safeExceptionMessage(error),
        at: new Date().toISOString(),
      });
      return false;
    }
    logOrganizationException(context, "automatic-park-compaction-deferred", error);
    return false;
  }
}

// TOMBSTONE (#751/P4): `interruptedReflectionAttempt` (which scanned an ended
// turn for a half-emitted `org_reflect` tool call) and
// `reflectionTurnReachedMaximumOutput` (which classified the retry reason for
// one) lived here. Both existed only to re-request a bounded handoff.

/**
 * What a failed compaction should SAY, when the reason is that the request was
 * too large to summarize.
 *
 * A compaction exists to rescue an oversized session, and the summarization
 * call it makes carries that same oversized session — so the one case the verb
 * is FOR is the case where the provider answers
 * `400 … This endpoint's maximum context length is …`. Recorded raw, that reads
 * as a transient provider fault and invites a retry that cannot ever succeed:
 * measured on `taperoom-inc` 2026-08-20, `research-lead` has two compact
 * requests and both failed with exactly that 400.
 *
 * So the durable failure names the wall and the way past it. It does NOT take
 * the way past it — replacing somebody's session is a decision with a cost, and
 * this function makes it legible rather than making it silently.
 */
export function compactionFailureReason(raw: string): string {
  if (!providerRequestTooLargeError(raw)) return raw;
  return (
    "The summarization request exceeded the model's own context window, so this session " +
    "cannot be compacted by summarizing it — a retry will fail the same way. Move this " +
    "person to a model with a larger window, or ask the operator to stop and start their " +
    `pane. Provider said: ${raw}`
  );
}

type ProviderFailureKind = "content_filter" | "insufficient_credits" | "stream_ended_without_finish_reason" | "upstream_idle_timeout" | "request_too_large" | "provider_error";


/**
 * Pi's agent_end event is the only stable extension boundary for a failed
 * provider turn.  Keep a compact, content-free record so launcher operators
 * can diagnose provider quality without copying a transcript (or retrying a
 * turn whose visible tool call might already have reached the provider).
 */
/** The unambiguous shape of an Anthropic-style tool call rendered as TEXT. */
const PRINTED_TOOL_CALL = /<invoke\s+name="([a-zA-Z_][a-zA-Z0-9_]*)"[\s\S]*?<\/invoke>/;

/**
 * A turn where the model PRINTED its tool call instead of making one.
 *
 * # The third member of the looks-finished-but-isn't family
 *
 * Measured on a live box: 215 occurrences across 13 people, 61% of them
 * within six transcript rows of a resume notice. The model emits
 * `<invoke name="org_send">…</invoke>` as ordinary assistant TEXT, no tool
 * runs, `agent_settled` fires, and the settle countdown parks a person whose
 * work never happened. Every rule reads the turn as COMPLETED, because by every
 * signal it is.
 *
 * # Why this shape and not a looser one
 *
 * A `type: "text"` item carrying the invoke grammar, in a message with NO
 * `toolCall` item. Both halves matter: a genuine tool call arrives as a
 * `toolCall` content item, and an assistant that legitimately QUOTES the
 * grammar while also calling the tool has done its work. Pi's own dist contains
 * no such rendering, so the text form cannot be legitimate output of the tool
 * path itself.
 *
 * Fenced code blocks are NOT excluded, deliberately: an agent explaining tool
 * syntax inside a fence is rare, the exclusion is fiddly to get right, and the
 * per-episode cap below bounds the cost of a false positive to one corrective
 * prompt. The bound is the mitigation rather than a cleverer regex.
 */
export function printedToolCall(event: unknown): { toolName: string } | undefined {
  const messages = (event as { messages?: unknown })?.messages;
  if (!Array.isArray(messages)) return undefined;
  for (const message of [...messages].reverse()) {
    const candidate = message as { role?: unknown; content?: unknown };
    if (candidate.role !== "assistant" || !Array.isArray(candidate.content)) continue;
    // A message that ALSO made a real call is not this defect, whatever else
    // its prose contains.
    if (candidate.content.some((item: any) => item?.type === "toolCall")) return undefined;
    for (const item of candidate.content) {
      const text = (item as { type?: unknown; text?: unknown });
      if (text.type !== "text" || typeof text.text !== "string") continue;
      const match = PRINTED_TOOL_CALL.exec(text.text);
      if (match?.[1]) return { toolName: match[1] };
    }
    // Only the LAST assistant message is the turn's answer; an earlier one that
    // printed a call and was then corrected is history, not a live defect.
    return undefined;
  }
  return undefined;
}

/** At most this many correctives per session, then the card takes over. */
const PRINTED_TOOL_CALL_CORRECTIVE_LIMIT = 3;

export function providerFailureDiagnostic(event: unknown): {
  kind: ProviderFailureKind;
  hadToolCall: boolean;
  containedCjk: boolean;
  /** #399: the raw (content-free) provider error string, so a hard
   * configuration failure can be classified and carded, never dumped raw. */
  errorMessage: string;
} | undefined {
  const messages = (event as { messages?: unknown })?.messages;
  if (!Array.isArray(messages)) return undefined;
  for (const message of [...messages].reverse()) {
    const candidate = message as { role?: unknown; stopReason?: unknown; errorMessage?: unknown; content?: unknown };
    if (candidate.role !== "assistant" || candidate.stopReason !== "error") continue;
    const error = String(candidate.errorMessage ?? "");
    // `request_too_large` is tested BEFORE the generic tail because it is the
    // one kind here that is permanent: it describes the request we built, not
    // the provider's health, and the caller uses it to stay off the
    // reliability-escalation path entirely.
    const kind: ProviderFailureKind = /content[_ -]?filter/i.test(error)
      ? "content_filter"
      // Tested beside `request_too_large` and for the same reason: a 402 is
      // PERMANENT until a human adds credits. Measured on a live box
      // (2026-08-20, 46 turns in one hour, 30 of them the Chief's own): filed
      // as `provider_error` it climbed the reliability counter and mailed a
      // manager AGENT "check that Pi's provider access and model health" — a
      // remedy no agent in the company can perform, because only the account's
      // owner can top it up.
      : providerInsufficientCreditsError(error)
        ? "insufficient_credits"
      : /stream ended without finish_reason/i.test(error)
        ? "stream_ended_without_finish_reason"
        : /upstream idle timeout/i.test(error)
          ? "upstream_idle_timeout"
          : providerRequestTooLargeError(error)
            ? "request_too_large"
            : "provider_error";
    const serializedContent = JSON.stringify(candidate.content ?? "");
    return {
      kind,
      hadToolCall: Array.isArray(candidate.content) && candidate.content.some((content: any) => content?.type === "toolCall"),
      containedCjk: /[\u3400-\u9fff\uf900-\ufaff]/u.test(serializedContent),
      errorMessage: error,
    };
  }
  return undefined;
}


async function workResumeDetails(context: OrganizationRuntimeContext, manifest: IntercomOrganizationManifest, firstBoot = false): Promise<WorkResumeDetails> {
  const doc = await readMailboxDoc(context, context.personId);
  return {
    personId: context.personId,
    ...(firstBoot ? { firstBoot: true } : {}),
    // chiefd's own roster for THIS company, already loaded by this install.
    // Reported, not judged: `workResumePrompt` owns what one person means.
    companyPeopleCount: manifest.peopleOrder.length,
    pendingMessageCount: Object.keys(doc?.pending ?? {}).length,
    protectedSchedules: [],
  };
}

export function workResumePrompt(person: PersonRecord, details: WorkResumeDetails): string {
  // THE FOUNDING BOOT — this person's first materialization AND a company that
  // holds nobody else — returns before any of the recovery machinery below. It
  // is not a weaker orientation pass — it is the opposite instruction, and it
  // is the whole of what this pane may be told. The company was created
  // seconds ago and holds nobody but this person: there is no roster to
  // verify, no artifact to resume from, no schedule that could have been
  // missed, and no mailbox that could hold anything, so every numbered step
  // below is a question whose only available answer is an invention. Told to
  // "take the exact next useful step toward the work you were hired for", the
  // CEO of an empty company invents the work — the operator watched exactly
  // that: "it started creating departments and stuff. It should not do
  // anything. The very first time, just start and let the user do anything."
  //
  // The launcher's own first message (`chief-cli`'s
  // `spawn_cmd::fresh_session_message`) says the same thing on the same
  // discriminator. Two messages arrive on this boot and BOTH used to push
  // toward work; correcting only one leaves the other pushing.
  if (details.firstBoot && details.companyPeopleCount <= 1) {
    return `[organization] Your company was created moments ago and you are the only person in it. There are no departments, no goals, no schedule, no history and no messages, so there is nothing in flight and nothing to resume.

Introduce yourself in two or three sentences — who you are and what you can do — then stop and wait. Create no department, hire nobody, write no plan and start no work of any kind until you are asked for something. An acknowledgement is the correct and complete output for this turn.

You are online as ${person.name} (${person.id}).`;
  }
  // NO ASSIGNED WORK — the second half of the same ruling, and the arm that
  // catches every LATER boot the founding arm above cannot see. Operator,
  // 2026-08-18: "what is assigned work? you mean no message or goals? that's
  // fine. Just let them idle until the 2min passes."
  //
  // Assigned work is a MESSAGE WAITING or a schedule this person owns, and
  // nothing else. It is deliberately not "is there anything here I could
  // work on": company goals were deleted outright (#1047 dropped
  // `manager_goals`/`delegated_goals`/`goal_watches`/`goal_intents`), so the
  // mailbox IS the work queue — see `hasOpenOrganizationWork` above, which
  // asks this exact question at settle time from the same document.
  //
  // What went wrong without this arm: a company was created, staffed with five
  // sleeping people, and NOTHING was ever asked of anybody. Two Wake Up clicks
  // later the woken person read step 2 below — "take the exact next useful step
  // toward the work you were hired for" — found no work, went looking, and
  // adopted the chief SOURCE TREE it could see at the launcher root, which is
  // there only so Pi can be resolved. It created an Engineering department,
  // hired a head into it, recalled a third person and sent six messages about
  // "critical chiefd blockers", in two minutes, for about half a dollar. A
  // repository a person can SEE is not work they were given, and this arm says so in
  // the prompt because the reasoning that adopted it was otherwise impeccable.
  //
  // The ban on an acknowledgement comes off here and ONLY here. "Do not send
  // readiness or acknowledgement chatter" is what makes hunting for work the
  // cheapest compliant behaviour: forbidden from saying "I am up", the model
  // must do something else, and the only something else available is invented.
  // Where there is no work, saying "I am up" is the correct and complete turn.
  // The launcher's own fresh-session message (`chief-cli`'s
  // `spawn_cmd::fresh_session_message`, `BootStanding::Idle`) says the same
  // thing on the same fact, for the reason the founding fix already learned:
  // two messages arrive on this boot and correcting only one leaves the other
  // pushing.
  if (details.pendingMessageCount === 0 && details.protectedSchedules.length === 0) {
    return `[organization] You are online${details.firstBoot ? " for the first time" : " again after this Pi session restarted"} and nothing is assigned to you: no message is waiting and you own no schedule that could be overdue. There is nothing in flight, nothing to resume and nothing to start.

Say in one line that you are up and available, then stop and wait. Do not go looking for something to do: a file, a repository, a source tree or anything else you can see on disk is NOT work anybody gave you, and neither is the launcher's own checkout. Create no department, hire nobody, write no plan, send no message and start no work of any kind until somebody asks you for something. An acknowledgement is the correct and complete output for this turn.

You are online as ${person.name} (${person.id}).`;
  }
  const waiting = details.pendingMessageCount
    ? `- ${details.pendingMessageCount} message${details.pendingMessageCount === 1 ? "" : "s"} waiting; read ${details.pendingMessageCount === 1 ? "it" : "them"} before anything else.`
    : "- No message is waiting.";
  const protectedSchedules = details.protectedSchedules.map((schedule) => `- Protected ChiefD schedule: ${schedule}`);
  const scheduledChecks = protectedSchedules.join("\n") || "- No protected schedule is recorded.";
  // #399: a first materialization never "restarted" — say so. A genuine resume
  // keeps the exact restart-recovery wording it always had.
  const opening = details.firstBoot
    ? "[organization] You are online for the first time. Start one focused orientation pass now; this is not a social check-in."
    : "[organization] Work resumed after this Pi session restarted. Start one focused recovery pass now; this is not a social check-in.";
  const secondStep = details.firstBoot
    ? "Take the exact next useful step toward the work you were hired for and your durable artifacts. Do not send readiness or acknowledgement chatter."
    : "Resume the exact next useful step from your latest private Pi session and durable artifacts. Do not send readiness or acknowledgement chatter.";
  const closing = details.firstBoot
    ? `You are online as ${person.name} (${person.id}).`
    : `You are resuming as ${person.name} (${person.id}).`;
  return `${opening}

Waiting for you:\n${waiting}\n\nActive schedules (${details.protectedSchedules.length} protected check${details.protectedSchedules.length === 1 ? "" : "s"}):\n${scheduledChecks}

1. Call org_roster to verify your role and who is in your organization.
2. ${secondStep}
3. Check your visible live schedules. If you own a genuine recurring responsibility whose active schedule was missed while this session was down, do one catch-up check now. Keep its existing schedule; do not invent a loop for one-off work.
4. If no message is waiting and no overdue recurring check remains, stop after a concise status instead of fabricating work.

${closing}`;
}

export type ResumeRecoveryDecision = "force-resume" | "settle";

/**
 * BUG #42 (live operator): a resumed agent must never be left stuck. Classify it
 * so the resume path either drives it back onto real work or lets it shut down —
 * it is never handed a turn it has no reason to run, and it is never abandoned.
 *
 * An agent with ANY durable work waiting — a pending message — is
 * force-resumed. An agent with none is settled/shut down (THE HARD RULE: an
 * agent with nothing to do must not linger). Pure and seam-free so the
 * decision is unit-testable.
 */
export function classifyResumeRecovery(input: { hasOpenWork: boolean }): ResumeRecoveryDecision {
  return input.hasOpenWork ? "force-resume" : "settle";
}

/**
 * BUG #42: route a resumed agent to force-resume or settle over injectable seams
 * (work-presence check, resume driver, settle driver). A first boot is
 * always an orientation pass (force-resume) regardless of open work. The driver
 * this dispatches (`forceResume`/`settle`) may reject; the rejection is propagated
 * so the caller can RE-ARM its one-shot latch and let the next lifecycle boundary
 * retry — an error on resume is auto-handled, never left stuck.
 */
export async function driveResumeRecovery(seams: {
  firstBoot: boolean;
  hasOpenWork: () => boolean | Promise<boolean>;
  forceResume: () => Promise<void>;
  settle: () => Promise<void>;
}): Promise<ResumeRecoveryDecision> {
  const decision = seams.firstBoot
    ? "force-resume"
    : classifyResumeRecovery({ hasOpenWork: await seams.hasOpenWork() });
  if (decision === "force-resume") await seams.forceResume();
  else await seams.settle();
  return decision;
}

export async function installOrganizationIntercom(pi: ExtensionAPI, options: InstallOrganizationIntercomOptions = {}) {
  const environment = options.environment ?? process.env;
  // #983: the FIRST thing this install does is ask beacond which daemon owns
  // ITS company. It rides the same boot ladder as the manifest read below and
  // for the same reason (#428): a fresh-session re-exec that lands inside a
  // beacond or chiefd restart window must wait the window out, because an
  // exited pane is a permanently dead person until something respawns it.
  const context = await withTransientReadRetryAsync(
    () => resolveOrganizationRuntimeContext(environment),
    options.bootTransientRetryDelaysMs ?? BOOT_TRANSIENT_RETRY_DELAYS_MS,
  );
  // #428: the FIRST docstore contact this process ever makes. Retried on the
  // boot ladder (never the blip ladder — `withTransientReadRetryAsync` with
  // `BOOT_TRANSIENT_RETRY_DELAYS_MS`): a fresh-session re-exec that lands
  // inside a chiefd restart window must wait out the window, not exit — an
  // exited pane is a permanently dead person until something else respawns it.
  const manifest = await withTransientReadRetryAsync(
    () => loadIntercomOrganization(context),
    options.bootTransientRetryDelaysMs ?? BOOT_TRANSIENT_RETRY_DELAYS_MS,
  );
  const person = currentPerson(context, manifest);
  let idleResumeTimer: ReturnType<typeof setTimeout> | undefined;
  let idleResumeReadyAttempts = 0;
  // #827 step 7: scheduleIdleResume's old company-maintenance-blocked branch
  // re-armed itself once a second forever while blocked -- a poll in
  // disguise. It now waits on the session-maintenance/supervision doc-change
  // this extension already subscribes to (see the sseWatcher's `onEvent`
  // below), with exactly one bounded fallback attempt so a missed event
  // cannot wedge a pane's idle-resume forever (allowlisted bounded-retry,
  // scripts/reactive-allowlist.ts).
  let idleResumeMaintenanceWaitEpoch: number | undefined;
  let idleResumeMaintenanceFallbackTimer: ReturnType<typeof setTimeout> | undefined;
  let resumedSessionId: string | undefined;
  // #399: whether THIS boot resumed a prior Pi transcript. A genuine restart
  // restores a session that already has entries; a first materialization starts
  // empty. Default `true` (assume resume) so an extension installed into an
  // already-running session never mis-renders a first-boot welcome on an
  // established pane — first boot is only asserted on a real empty-history
  // `session_start`.
  let bootResumedPriorSession = true;
  let workResumePending = false;
  let workResumePrompted = false;
  // Turn-progress watchdog state (BUG-12): a turn can park on an await that
  // never settles — no message, no tool event, no provider socket, forever —
  // while every timer keeps firing. Pi has no turn liveness bound of its own,
  // so the extension tracks observable progress and aborts a stalled turn;
  // the settled path then re-drives drain/work-resume from durable state.
  let turnInFlight = false;
  let turnProgressAt = 0;
  let turnWatchdogAbortIssued = false;
  let turnWatchdogAbortAt = 0;
  let turnWatchdogUnrecoverableIssued = false;
  // TOMBSTONE (#751/P4): the whole process-local reflection delivery state --
  // retry/acceptance timers and attempt counters, the delivery epoch/sequence/
  // cycle, `reflectionDeliveryStates`, `exhaustedReflectionTransitions`,
  // `promptedTransitionIds`, `acceptedReflectionReceipts` and its prune, plus
  // the `lifecycleFollowUpPending` / `settlingActivity` guards -- was declared
  // here. All of it tracked one bounded handoff prompt per transition, and
  // there is no prompt.
  let consecutiveRawEmptyOrganizationSendCalls = 0;
  let rawEmptyOrganizationSendCircuitStopped = false;
  let consecutiveExecuteEmptyOrganizationSendCalls = 0;
  let executeEmptyOrganizationSendCircuitStopped = false;
  let consecutiveProviderFailures = 0;
  let providerFailureEscalated = false;
  // #399: a hard "provider not configured" card is shown at most once per
  // process — the config does not change mid-session, so repeating the same
  // card on every failed turn would only churn scrollback.
  let providerConfigurationCardShown = false;
  // A permanent context-overflow card is shown at most once per unbroken run of
  // identical rejections, for the same reason: every subsequent turn produces
  // the identical rejection, and a card per failed turn would bury the intercom
  // traffic the operator is actually reading.
  //
  // Keyed on the WINDOW rather than a bare boolean, and cleared by any turn that
  // completes, so silence is bounded by the episode instead of by the process. A
  // person who trims their context, works, and overflows again is told again —
  // that second overflow is new information — and a person moved to a model with
  // a different window is never silenced by a card that named the old one.
  let requestTooLargeCardShownForLimit: number | undefined;
  let providerFailureEpisodeId: string | undefined;
  // EVERY DELIVERY THIS TURN CONSUMED, so a turn that dies can say what it
  // destroyed.
  //
  // Acceptance is at `message_start` — TURN START, not completion — and it is
  // the durable pending→accepted move. That is correct and is not what this
  // change touches: a message must not stay pending while a turn reads it, or a
  // crash re-delivers work that was already begun. The consequence is what was
  // wrong: a turn that then FAILS has consumed the envelope and answered
  // nothing, and before this the sender was never told, so an operator's
  // request simply ceased to exist. Cleared at the end of every `agent_end`,
  // failed or not, so each list belongs to exactly one turn.
  let deliveriesConsumedThisTurn: Array<{ id: string; fromPersonId: string }> = [];
  // The content-refusal escalation is ONCE PER INSTALL, and deliberately NOT
  // re-armed by a completed turn — which is the opposite of the two cards
  // below, for a reason worth stating. A card is for the person at this pane
  // and a fresh one is fresh information. This escalation is MAIL TO A MANAGER,
  // and the shape it exists for is a person who is filtered on one recurring
  // topic and healthy on everything else: measured on a live box,
  // intel-news failed six times and escalated zero times, because any healthy
  // turn in between reset the consecutive counter. Re-arming on success would
  // swap that starvation for a manager mailed on every filtered turn forever.
  // Once is the honest number: the fact being reported is "this person is being
  // refused on content", which does not become truer the ninth time.
  let contentFilterEscalated = false;
  let contentFilterCardShown = false;
  // Re-armed by a completed turn, like the other cards: a turn that ran proves
  // the account has credits again, so the NEXT 402 is new information.
  let insufficientCreditsCardShown = false;
  /** How many printed-tool-call correctives this session has sent. */
  let printedToolCallCorrectives = 0;
  let printedToolCallCardShown = false;
  /** Whether a resume notice started the turn now ending. */
  let sawResumeNoticeRecently = false;
  const mailboxDeliveryAttempts = new Set<string>();
  const now = options.clock ?? Date.now;
  const toolRegistrar = organizationToolRegistrar(pi, context);
  const sessionMaintenanceClaimToken = randomUUID();
  const sessionMaintenanceLifecycleFence = createSessionMaintenanceLifecycleFence();
  let latestExtensionContext: ExtensionContext | undefined;
  let sessionContextEpoch = 0;
  let mailboxDrainInFlight: { epoch: number; promise: Promise<number> } | undefined;
  const idleResumeInFlight = new Set<Promise<void>>();
  const postCompactionResumeInFlight = new Set<Promise<void>>();
  let sessionMaintenanceInFlight: Promise<boolean> | undefined;
  let sessionMaintenancePollInFlight: Promise<void> | undefined;
  let sessionMaintenanceShuttingDown = false;
  let sessionMaintenanceStartupReady = false;
  const sessionMaintenanceStartRetry: SessionMaintenanceStartRetry = { failures: 0, nextAttemptAt: 0 };
  const sessionMaintenanceDeferralRetry: SessionMaintenanceDeferralRetry = { failures: 0, nextAttemptAt: 0, reported: false };
  const nativeCompaction: NativeCompactionLease = {};
  let resumeAfterNativeCompaction = () => {};

  const resetRawEmptyOrganizationSendCircuit = () => {
    consecutiveRawEmptyOrganizationSendCalls = 0;
    rawEmptyOrganizationSendCircuitStopped = false;
  };

  const resetEmptyOrganizationSendCircuit = () => {
    resetRawEmptyOrganizationSendCircuit();
    consecutiveExecuteEmptyOrganizationSendCalls = 0;
    executeEmptyOrganizationSendCircuitStopped = false;
  };

  const rawOrganizationSendHasText = (args: unknown): boolean => {
    if (!args || typeof args !== "object" || Array.isArray(args)) return false;
    const raw = args as { body?: unknown; message?: unknown };
    return (typeof raw.body === "string" && Boolean(raw.body.trim()))
      || (typeof raw.message === "string" && Boolean(raw.message.trim()));
  };

  const observeRawOrganizationToolStart = (event: { toolName?: unknown; args?: unknown }, eventContext?: ExtensionContext) => {
    if (event.toolName !== "org_send") {
      // The circuit is specifically consecutive body-less sends. Any other
      // tool proves the model moved on and safely breaks that streak.
      resetEmptyOrganizationSendCircuit();
      return;
    }
    if (rawOrganizationSendHasText(event.args)) {
      // This raw call has canonical text (or the trusted historical alias).
      // Later recipient/assignment validation is independent of the missing-
      // body loop and must not inherit its streak.
      resetEmptyOrganizationSendCircuit();
      return;
    }
    consecutiveRawEmptyOrganizationSendCalls = Math.min(
      ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT,
      consecutiveRawEmptyOrganizationSendCalls + 1,
    );
    if (consecutiveRawEmptyOrganizationSendCalls < ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT
      || rawEmptyOrganizationSendCircuitStopped) return;
    rawEmptyOrganizationSendCircuitStopped = true;
    try {
      appendOrganizationEvent(context, {
        event: "tool-call-loop-stopped",
        tool: "org_send",
        boundary: "pre-validation-tool-execution-start",
        personId: context.personId,
        consecutiveCalls: ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT,
        scope: "current-agent-run",
        at: new Date().toISOString(),
      });
    } finally {
      // Pi 0.80.10 awaits this raw start event before prepareArguments. Abort
      // now so the third schema-rejected call ends this model run and no fourth
      // provider request or later call in the same batch can begin.
      eventContext?.abort();
    }
  };

  pi.on("tool_call", (event) => {
    if (event.toolName !== "bash") return undefined;
    const input = event.input as Record<string, unknown>;
    const requestedTimeout = input.timeout;
    if (requestedTimeout === undefined) {
      // Pinned Pi has no default Bash timeout. One managed foreground call
      // must never own the agent turn indefinitely and starve durable mail.
      input.timeout = ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS;
      return undefined;
    }
    if (typeof requestedTimeout !== "number" || requestedTimeout <= ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS) {
      return undefined;
    }
    try {
      appendOrganizationEvent(context, {
        event: "foreground-bash-deadline-rejected",
        personId: context.personId,
        requestedTimeoutSeconds: requestedTimeout,
        maximumTimeoutSeconds: ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS,
        at: new Date().toISOString(),
      });
    } catch {
      // Diagnostic storage is never authority for the execution guard. Keep
      // blocking even if the event sink is replaced or becomes unavailable.
    }
    return { block: true, reason: ORGANIZATION_FOREGROUND_BASH_DEADLINE_GUIDANCE };
  });

  pi.registerMessageRenderer<OrganizationEnvelope | OrganizationMailboxBatch>(MESSAGE_TYPE, (message, { expanded }, theme) => {
    const envelope = message.details;
    if (!envelope) return undefined;
    if (isOrganizationMailboxBatch(envelope)) {
      const visible = expanded ? envelope.envelopes : envelope.envelopes.slice(0, 3);
      const lines: CardLine[] = visible.map((item, index) => ({
        text: `${index + 1}. @${displayHandle(item.organization, item.fromPersonId)}: ${item.body}`,
        token: "customMessageText",
      }));
      if (!expanded && envelope.envelopes.length > visible.length) {
        lines.push({ text: `${envelope.envelopes.length - visible.length} more inbox item${envelope.envelopes.length - visible.length === 1 ? "" : "s"} · ${CARD_EXPAND_HINT_TEXT}`, token: "dim" });
      }
      return renderCard(theme, {
        kind: "intercom-message",
        icon: domainIcon("📬"),
        titleStyle: "bold",
        title: `Inbox review · ${envelope.envelopes.length} messages`,
        body: { kind: "lines", lines },
        boxed: true,
      }, { expanded });
    }
    const systemNotice = launcherSystemNoticePresentation(envelope, manifest);
    if (systemNotice) {
      const visibleContext = expanded ? systemNotice.context : systemNotice.context.slice(0, 1);
      // The launcher notice's whole body below the accent-bold title is a
      // mixed-token structural block (customMessageText summary/body, dim
      // context/Next, warning-or-dim impact) with per-card collapse rules, so
      // it is described as a `"lines"` CardBody the site assembles and
      // renderCard colors + boxes — no hand-rolled Box or theme.fg here.
      const bodyLines: CardLine[] = [
        { text: systemNotice.summary, token: "customMessageText" },
        ...visibleContext.map((line): CardLine => ({ text: line, token: "dim" })),
      ];
      if (!expanded && systemNotice.context.length > visibleContext.length) {
        const more = systemNotice.context.length - visibleContext.length;
        bodyLines.push({ text: `More context: ${more} additional item${more === 1 ? "" : "s"} · ${CARD_EXPAND_HINT_TEXT}`, token: "dim" });
      }
      // #8: the launcher branch rendered ONLY the fixed presentation fields and
      // never the envelope's own prose, while the person branch below has
      // always rendered `envelope.body`. That single asymmetry is the whole
      // "sender-not-kind" split: a goal watch arrived with its goals, its
      // priorities and its instruction in the body, and the card showed none of
      // it.
      //
      // #103: how it COLLAPSES depends on what the body is. A `"prose"` body
      // mirrors the person branch — bounded preview collapsed, full when
      // expanded — because its opening sentence carries the gist. A `"list"`
      // body renders IN FULL even collapsed: 96 characters of an enumeration
      // is one item and an ellipsis, so a preview delivers the notification
      // while withholding the information ("an open goal needs review" without
      // WHICH). That is the operator's original complaint one layer in, which
      // is why this is a per-card-kind decision and not a global flag.
      const noticeBody = systemNotice.body;
      if (noticeBody) {
        if (expanded || systemNotice.bodyLayout === "list") {
          bodyLines.push({ text: "" }, { text: noticeBody, token: "customMessageText" });
        } else {
          const preview = compactPresentation(noticeBody, 96);
          bodyLines.push({ text: preview.text, token: "customMessageText", raw: preview.truncated ? "…" : "" });
        }
      }
      bodyLines.push({ text: `Next: ${systemNotice.nextAction}`, token: "dim" });
      bodyLines.push({ text: systemNotice.impact, token: systemNotice.blocked === true ? "warning" : "dim" });
      return renderCard(theme, {
        kind: "system-notice",
        icon: domainIcon(""),
        titleStyle: "accent-bold",
        accentToken: SYSTEM_NOTICE_ACCENT_TOKEN,
        title: systemNotice.title,
        body: { kind: "lines", lines: bodyLines },
        boxed: true,
      }, { expanded });
    }
    const sender = !envelope.organization || envelope.organization === context.organization
      ? `@${displayHandle(envelope.organization ?? context.organization, envelope.fromPersonId)}`
      : `${envelope.organization}/@${displayHandle(envelope.organization, envelope.fromPersonId)}`;
    // #433: the sender's own identity accent colors their `@name`, matching
    // their pane header exactly. A broadcast / cross-org / unknown sender has
    // no roster accent to borrow, so it keeps the neutral `muted` token.
    const senderAccentHex = organizationPersonAccentHex(manifest, envelope.fromPersonId, envelope.organization);
    const senderLabel = senderAccentHex
      ? truecolorMention(
          organizationPersonDisplayAccent(theme, senderAccentHex),
          theme.bold(sender),
          "",
        )
      : colorOrganizationMessageSender(
          theme,
          organizationMessageSenderAccent(manifest, envelope.organization, envelope.fromPersonId),
          theme.bold(sender),
        );
    const preview = compactPresentation(envelope.body, 96);
    // The 👤/🎯/🗓 lines mix a raw glyph prefix with a token-colored value, and
    // the sender's `@name` carries its own IDENTITY accent (a roster-hex color,
    // not a theme token) — so the header/body are described as a `"lines"`
    // CardBody the site assembles (identity-colored senderLabel passed through
    // as an uncolored `raw`/`rendered` segment) and renderCard boxes.
    const bodyLines: CardLine[] = [];
    if (expanded) bodyLines.push({ text: "" }, { text: envelope.body, token: "customMessageText" });
    else bodyLines.push({ text: preview.text, token: "customMessageText", raw: preview.truncated ? "…" : "" });
    return renderCard(theme, {
      kind: "intercom-message",
      icon: domainIcon("💬"),
      titleStyle: "bold",
      title: "Message",
      sender: { from: "from", rendered: senderLabel },
      body: { kind: "lines", lines: bodyLines },
      boxed: true,
    }, { expanded });
  });

  // TOMBSTONE (#751/P4): a `REFLECTION_REQUEST_TYPE` message renderer stood
  // here, drawing the "🪞 Reflect before pausing" card that asked the pane for
  // a bounded handoff. Nothing sends that message any more.

  pi.registerMessageRenderer("organization-session-maintenance", (message: any, { expanded }: { expanded?: boolean }, theme: any) => {
    const details = message.details as { request?: SessionMaintenanceRequest; phase?: string } | undefined;
    const request = details?.request;
    if (!request) return undefined;
    const phase = details?.phase || request.status;
    // #319 made the ICON non-binary and left both tables below binary, so a
    // third action rendered `🧠undefined · @person` with `undefined` for a
    // body — on a card whose whole job is to tell the operator what happened.
    // The `!` on the fallback lookup was what hid it: it asserted to the
    // compiler that a missing row could not be missing, so adding `set_model`
    // without adding its rows type-checked cleanly and failed on the glass.
    // Both tables are keyed by EVERY action now, and neither lookup may
    // assert — an unknown action renders a plain sentence rather than the
    // word `undefined`.
    const icon = "🧠";
    const titleByPhaseAndAction: Record<string, string> = {
      "completed:compact": "Context compacted",
      "skipped:compact": "Context already focused",
      "failed:compact": "Compaction paused",
      "running:compact": "Compacting context",
      "queued:compact": "Context compact queued",
    };
    const title = titleByPhaseAndAction[`${phase}:${request.action}`]
      ?? titleByPhaseAndAction[`queued:${request.action}`]
      ?? "Session maintenance";
    const descriptionByAction: Record<string, string> = {
      compact: "Pi is focusing this session's native context; durable work stays saved.",
    };
    // A skipped request needs its OWN per-action sentence: "this session is
    // already small" is true of a skipped compaction and was nonsense about a
    // skipped model change, which is why this table is keyed by action at all.
    // Only `compact` survives; the `set_model` row went with the action, and
    // the `??` fallback below is what would have hidden it if it had stayed.
    const skippedDescriptionByAction: Record<string, string> = {
      compact: "This session is already small; durable work remains unchanged.",
    };
    const description = phase === "skipped"
      ? skippedDescriptionByAction[request.action] ?? "Nothing needed doing; durable work remains unchanged."
      : descriptionByAction[request.action] ?? "Pi is performing session maintenance on this pane.";
    const requester = request.requestedBy === "launcher" ? "Requested by the system"
      : request.requestedBy === "human" ? "Requested by you"
        // "operator" mirrors src/organization/org-session-maintenance.ts's
        // SESSION_MAINTENANCE_OPERATOR_REQUESTER sentinel — the extension
        // cannot import ../src/, so this literal must be kept in sync by hand.
        : request.requestedBy === "operator" ? "Requested by the operator"
          : `Requested by @${displayHandle(context.organization, String(request.requestedBy))}`;
    // #319: interrupt/force applies to any single-target request now, not
    // only company-wide fanout — show the mode whenever `force` is set.
    const mode = request.force !== undefined ? ` · ${request.force ? "interrupt now" : "after current work"}` : "";
    const reasonLines = [`Reason: ${request.reason}`, `${requester}${mode}`];
    // WHICH MODEL, ON THE VISIBLE LINE. The reason block below is
    // `collapse: "hidden"`, so a model named only there is a model the
    // operator cannot see without expanding — and "which model did it switch
    // to" is the entire question this card exists to answer.
    //
    // TOMBSTONE: the model line, which only a `set_model` request carried.
    const modelLine = undefined;
    if (request.retryNotBefore) reasonLines.push(`Retry scheduled: ${new Date(request.retryNotBefore).toLocaleString()}`);
    return renderCard(theme, {
      kind: "session-maintenance",
      icon: domainIcon(icon),
      titleStyle: "bold",
      title,
      target: `@${displayHandle(context.organization, String(request.personId))}`,
      detail: modelLine ? [modelLine, description] : [description],
      // The expanded reason block is a multi-line dim body (collapsed to the
      // hint); `wrap: "per-line"` keeps every line dim across the newlines.
      body: { kind: "prose", text: reasonLines.join("\n"), collapse: "hidden", wrap: "per-line" },
      bodyToken: "dim",
      boxed: true,
      footer: phase === "failed" && request.error ? [{ text: request.error, token: "error" }] : undefined,
    }, { expanded });
  });

  pi.registerMessageRenderer(RESUME_TYPE, (message: any, { expanded }: { expanded?: boolean }, theme: any) => {
    const prompt = typeof message.content === "string" ? message.content : "";
    const details = message.details as Partial<WorkResumeDetails> | undefined;
    const protectedSchedules = Array.isArray(details?.protectedSchedules) ? details.protectedSchedules : [];
    const pending = typeof details?.pendingMessageCount === "number" && Number.isSafeInteger(details.pendingMessageCount)
      ? Math.max(0, details.pendingMessageCount)
      : 0;
    // #399: a first materialization is not a resume — nothing was interrupted.
    // Render a welcome banner instead of the "brief restart interrupted" claim,
    // keeping the same durable body so the orientation context is identical.
    // Genuine restarts keep the exact ⚡ Work resumed card.
    const firstBoot = details?.firstBoot === true;
    // The card's two sections (💬 waiting mail, ⟳ protected checks) with bold
    // headers and 🛡️ protected rows are a structured mixed-token block, so they
    // route through a `"lines"` CardBody with the box/title/hint owned by
    // renderCard.
    const bodyLines: CardLine[] = [
      { text: firstBoot
        ? "You are online for the first time. Here is what is waiting for you."
        : "A brief restart interrupted this Pi session. Resuming from durable work.", token: "dim" },
      { text: "" },
      { prefix: "💬 ", bold: true, text: `${pending} message${pending === 1 ? "" : "s"} waiting` },
    ];
    if (!pending) bodyLines.push({ prefix: "  ", text: "No message waiting", token: "dim" });
    bodyLines.push({ text: "" }, { prefix: "⟳ ", bold: true, text: `Scheduled checks · ${protectedSchedules.length} protected check${protectedSchedules.length === 1 ? "" : "s"}` });
    for (const schedule of protectedSchedules) bodyLines.push({ prefix: `  ${CARD_GLYPHS.lock} `, text: schedule, token: "customMessageText" });
    if (!protectedSchedules.length) {
      bodyLines.push({ prefix: "  ", text: "No protected check recorded", token: "dim" });
    }
    if (expanded && prompt) bodyLines.push({ text: "" }, { text: prompt, token: "customMessageText" });
    else if (prompt) bodyLines.push({ text: CARD_EXPAND_HINT_TEXT, token: "dim" });
    return renderCard(theme, {
      kind: firstBoot ? "first-boot" : "work-resumed",
      icon: domainIcon(firstBoot ? "🌱" : "⚡"),
      titleStyle: "bold",
      title: firstBoot ? "New post" : "Work resumed",
      target: firstBoot ? `@${displayHandle(context.organization, String(details?.personId ?? ""))}` : undefined,
      body: { kind: "lines", lines: bodyLines },
      boxed: true,
    }, { expanded });
  });

  // #399 part 2: a fatal pane failure (an unconfigured provider, or a request
  // the context window refused) is rendered as a legible `failure` card —
  // reason + remedy + log path — instead of the raw provider dump.
  //
  // An ENTRY renderer, not a message renderer, because these cards are appended
  // with `pi.appendEntry` (see `showPaneFailureCard`): a custom MESSAGE has to
  // ride a turn to reach the pane, and the pane these cards describe is one
  // whose next turn cannot run.
  pi.registerEntryRenderer(PANE_FAILURE_TYPE, (entry: any, _opts: { expanded?: boolean }, theme: any) =>
    renderCard(theme, paneFailureSpec((entry?.data ?? {}) as Record<string, unknown>)));

  type RawOrganizationSendArguments = { body?: unknown; message?: unknown } & Record<string, unknown>;
  type CanonicalOrganizationSendArguments = {
    to: string;
    body: string;
    urgency?: MessageUrgency;
    replyTo?: string;
  };

  /**
   * Pi invokes this compatibility seam before provider arguments are validated.
   * Providers see only the required canonical body field, while a historical
   * session containing the former message alias is normalized and stripped at
   * this trusted boundary. Missing text throws concise guidance before execute;
   * the earlier raw start event exclusively owns native retry-loop state, so
   * argument preparation itself cannot mutate organization state.
   */
  const prepareOrganizationSendArguments = (input: unknown): CanonicalOrganizationSendArguments => {
    if (!input || typeof input !== "object" || Array.isArray(input)) return input as never;
    const raw = input as RawOrganizationSendArguments;
    const { message, ...canonical } = raw;
    const bodyText = typeof canonical.body === "string" ? canonical.body.trim() : "";
    const messageText = typeof message === "string" ? message.trim() : "";
    if (bodyText && messageText && bodyText !== messageText) {
      throw new Error("org_send accepts one message value in body. Remove message and retry once; no message was queued.");
    }
    if (!bodyText && messageText) canonical.body = message;
    if (typeof canonical.body !== "string" || !canonical.body.trim()) {
      throw new CallerRefusal(ORGANIZATION_SEND_BODY_REQUIRED_GUIDANCE);
    }
    return canonical as never;
  };

  const messageTextFromToolParams = (params: { body?: unknown }): string => {
    const body = typeof params.body === "string" ? params.body.trim() : "";
    if (!body) throw new CallerRefusal(ORGANIZATION_SEND_BODY_REQUIRED_GUIDANCE);
    return body;
  };

  toolRegistrar.registerTool({
    name: "org_send",
    label: "Message an organization person",
    // Pi executes a provider's tool-call batch in parallel by default. This
    // tool has a per-run circuit breaker whose third malformed call aborts the
    // current model operation, so it must finish each call before Pi is
    // allowed to begin the next one in the same assistant response.
    executionMode: "sequential",
    description: "Within this organization only: address people by their USERNAME, the @name shown on every message you receive; org_roster lists them. 'launcher' is never a recipient. Durably send one work-only direct message or one true broadcast with to='all'. THE SEND IS THE WAKE — a message to somebody who is not running starts them, so you never have to start a person before delegating to them and 'they are asleep' is never a reason to do their work yourself. A benched recipient is the one exception and this tool says so by name: org_recall them, then send again. Always put the complete message text in the required body field; never omit body. Never use the org CLI from a Pi shell. This is how every result, update, blocker and correction reaches its reader, and how a manager hands work to an owner.",
    parameters: Type.Object({
      to: Type.String({ description: "The recipient's username, as shown on their messages and in org_roster (for example priya, with or without the @). Their person id also works. Or all, for a true broadcast." }),
      body: Type.String({ minLength: 1, description: "Required complete message text. Never omit this field." }),
      urgency: Type.Optional(Type.Union([Type.Literal("normal"), Type.Literal("interrupt")])),
      replyTo: Type.Optional(Type.String()),
    }),
    prepareArguments: prepareOrganizationSendArguments,
    async execute(_toolCallId, params, _signal, _onUpdate, executionContext?: ExtensionContext) {
      try {
        const hasMessageText = typeof params.body === "string" && Boolean(params.body.trim());
        if (!hasMessageText) {
          consecutiveExecuteEmptyOrganizationSendCalls = Math.min(
            ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT,
            consecutiveExecuteEmptyOrganizationSendCalls + 1,
          );
          if (consecutiveExecuteEmptyOrganizationSendCalls < ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT) {
            return organizationSendFailure(
              new Error(ORGANIZATION_SEND_BODY_REQUIRED_GUIDANCE),
              context,
            );
          }
          if (!executeEmptyOrganizationSendCircuitStopped) {
            executeEmptyOrganizationSendCircuitStopped = true;
            appendOrganizationEvent(context, {
              event: "tool-call-loop-stopped",
              tool: "org_send",
              personId: context.personId,
              consecutiveCalls: ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT,
              scope: "current-agent-run",
              at: new Date().toISOString(),
            });
            // Pi supplies ExtensionContext as the native fifth tool-execution
            // argument. abort() ends only this model operation; durable mail,
            // goals, session history, and launcher-owned panes stay intact.
            executionContext?.abort();
          }
          return toolResult(false, [
            "Stopped this model turn after three consecutive org_send calls without message text.",
            "No message was queued. In the next turn, call org_send once with a concise body.",
          ].join(" "), {
            status: "tool_call_loop_stopped",
            retryable: false,
            circuitStopped: true,
            consecutiveCalls: ORGANIZATION_EMPTY_SEND_CIRCUIT_LIMIT,
            organization: context.organization,
          });
        }
        const body = messageTextFromToolParams(params);
        // The send derives its own id from its CONTENT, so the replay of an
        // interrupted call collides with it. Passing a tool-call-derived id
        // here is what used to defeat that — the resumed agent re-issues from
        // a new assistant message and receives a new tool-call id.
        const { envelope, replayedFrom } = await sendOrganizationMessage(context, {
          to: params.to,
          body,
          urgency: params.urgency,
          replyTo: params.replyTo,
        });
        // Atomic mailbox persistence is the success boundary, and it is also
        // the WAKE. `/v1/org/mailbox/delta` commits the pending row and nudges
        // chiefd's reconcile duty on its way out; the converge pass reads the
        // pending rows and grants launch intent to exactly their recipients,
        // because a durable envelope addressed to one person is itself the
        // explicit, per-node decision to start that person.
        //
        // THE SENDER MAKES NO RUNTIME WRITE, and that is the fix. This block
        // used to post `/v1/org/runtime/launch` — whose subject is the COMPANY,
        // so `require_company_wide_authority` grants it only to the head of the
        // root department. Every non-executive person's reply to the CEO came
        // back `403 caller-out-of-company-scope` and printed a wake warning on
        // an already-delivered message. Naming the recipients in that body
        // would not have helped: the launch then calls `start_person` for each,
        // which asks whether the caller manages the department that person
        // lives in, and nobody manages the CEO.
        resetEmptyOrganizationSendCircuit();
        let warning: string | undefined;
        {
          const manifest = await loadIntercomOrganization(context);
          const guidance: string[] = [];
          for (const recipient of envelope.recipients) {
            // A recipient nobody can wake at all — parked, absent, or not a
            // running seat — still owes the sender an explanation. That is a
            // fact about the RECIPIENT, not about a write the sender attempted.
            const disposition = messageWakeDisposition(manifest, recipient);
            if (!disposition.wake && disposition.guidance) guidance.push(disposition.guidance);
          }
          if (guidance.length) warning = guidance.join(" ");
        }
        // A suppressed duplicate NAMES the delivery it collided with. The
        // sender is told plainly that these words did not go out a second
        // time, so an agent that meant to repeat can say something the
        // recipient has not already read instead of believing it sent
        // something it did not.
        const text = replayedFrom
          ? `Message ${envelope.id} was already sent to @${envelope.to} at ${replayedFrom}; these identical words were not sent again. Say something different if the recipient needs more.`
          // #401: a send is an instant, atomic append to the durable mailbox
          // -- there is no "queued/draining/waiting" from the sender's point
          // of view. The message is committed the moment this returns.
          : `Message ${envelope.id} sent to @${envelope.to}.`;
        return toolResult(true, `${text}${warning ? ` ${warning}` : ""}`, { envelope, warning, replayedFrom });
      } catch (error) {
        return organizationSendFailure(error, context);
      }
    },
  });

  toolRegistrar.registerTool({
    name: "org_roster",
    label: "Show the organization roster",
    description: "Read the complete, durable company/department/contract hierarchy, transient engagement metadata, managers, and employment roster. Every person carries an authority field: what that person may add and where they may hire. Read it before you decide somebody cannot do a piece of work — never guess it. Runtime pane titles are never authority.",
    parameters: Type.Object({}),
    async execute() {
      try {
        let current = await loadIntercomOrganization(context);
        let observation = await loadOrganizationRosterObservation(context, current);
        let recoveryWarning: string | undefined;
        if (observation.runtimeActivityDivergence) {
          // A roster read never turns activity drift into start authority. Ask
          // ChiefD for an ordinary reconcile with ZERO requested people: the
          // normalized launch fence, durable demand, and CEO minimum-fleet rule
          // remain the only authorities that may admit a pane. Re-read once
          // after a successful converge so the caller sees the repaired facts.
          try {
            recoveryWarning = await reconcileRuntime(context);
          } catch {
            recoveryWarning = "The roster below is readable, but some runtime facts may be stale; read it again if a person's state looks wrong.";
            // The roster is a read surface. A failed best-effort converge must
            // not create a second write whose own failure could hide the
            // readable recovering projection.
          }
          if (!recoveryWarning) {
            current = await loadIntercomOrganization(context);
            observation = await loadOrganizationRosterObservation(context, current);
          }
        }
        const roster = formatOrganizationRoster(current, observation);
        const text = recoveryWarning ? `${roster}\n\nRecovery: ${recoveryWarning}` : roster;
        return toolResult(true, text, {
          organization: current.slug,
          runtimeStatus: observation.status,
          observedAt: observation.observedAt,
          ...(observation.runtimeActivityDivergence ? {
            recoveringPersonIds: [
              ...observation.runtimeActivityDivergence.missingProcessPersonIds,
              ...observation.runtimeActivityDivergence.unexpectedProcessPersonIds,
            ],
          } : {}),
          ...(recoveryWarning ? { recoveryWarning } : {}),
        });
      } catch (error) {
        // #384: never surface the raw "Cannot read activity/supervision
        // authority '...': chiefd docstore unreachable at <url>" exception --
        // the reads above already retried once with a brief backoff.
        const degraded = transientDegradeMessage("The roster", error);
        if (degraded) return toolResult(false, degraded, { status: "docstore_unreachable", retryable: true });
        return refusalResult(error);
      }
    },
  });


  /** A durable reminder, as chiefd's `/v1/reminders/*` routes serialize it. */
  interface OrganizationReminderRecord {
    id: string;
    personId: string;
    createdByPersonId: string;
    prompt: string;
    intervalMs: number;
    nextDueAt: string;
    status: string;
    recurring: boolean;
    fireCount?: number;
    createdAt: string;
    lastFiredAt?: string;
    expiresAt?: string;
  }

  /**
   * Every chiefd route the durable-REMINDER family posts to.
   *
   * A closed union for the same reason `StaffingRoutePath` and
   * `SupervisionRoutePath` are: a verb reaches a path by being named here or
   * not at all, so no route can arrive by concatenation and the seam
   * classifier's literal inventory stays the real one.
   *
   * #375: a reminder is not a goal. It is the supervision
   * ledger's own recurrence keyspace, and since the Pi `/loop` addon was
   * deleted it is the ONLY way anything happens again later.
   */
  type ReminderRoutePath =
    | "/v1/reminders/arm"
    | "/v1/reminders/list"
    | "/v1/reminders/stop";

  const REMINDER_ROUTES: Record<"arm" | "list" | "stop", ReminderRoutePath> = {
    arm: "/v1/reminders/arm",
    list: "/v1/reminders/list",
    stop: "/v1/reminders/stop",
  };

  /**
   * Post one durable-reminder mutation or read, in process.
   *
   * This replaced `spawn`ing `apps/cli/src/Main.ts org reminder <action>` — a
   * Pi extension shelling out to a CLI to reach the daemon it was already
   * connected to. That CLI now serves exactly one command (`founder-pi`), so
   * every reminder tool answered `unknown command 'org'`: with `/loop` deleted,
   * the product had no working way to schedule anything at all.
   *
   * **The `slug` is the COMPANY KEY, never the display slug.** Chiefd's
   * reminder routes resolve their authority by `req.slug ==
   * s.org_documents_slug`, and `org_documents_slug` is `sha256(<dir>)[..12]`.
   * A display slug does not fail loudly there — it simply matches no live
   * company, so the route answers 404 and the reminder is never armed. The
   * deleted CLI got this right the long way round, by handing `ChiefdClient` a
   * `root` so its client-side `companyKeyed` digested a composite key
   * (`cli.ts:892-899`, #564); this reads the served key straight from
   * `companyKeyOf`.
   */
  async function reminderCommand(
    action: "arm" | "list" | "stop",
    payload: Record<string, unknown>,
  ): Promise<{ reminder?: OrganizationReminderRecord; reminders?: OrganizationReminderRecord[] }> {
    return chiefdPostJson<{ reminder?: OrganizationReminderRecord; reminders?: OrganizationReminderRecord[] }>(
      chiefdEndpoint(context),
      REMINDER_ROUTES[action],
      { slug: companyKeyOf(context), ...payload },
    );
  }

  /**
   * Render a cadence the way a person says it, not in milliseconds. "every
   * 3600000ms" is a number the operator has to decode to check their own
   * reminder is right, which is how a wrong cadence survives review.
   */
  function reminderCadenceText(intervalMs: number, recurring: boolean): string {
    const minutes = Math.round(intervalMs / 60_000);
    const unit = minutes % 1440 === 0 && minutes >= 1440
      ? `${minutes / 1440} day${minutes / 1440 === 1 ? "" : "s"}`
      : minutes % 60 === 0 && minutes >= 60
        ? `${minutes / 60} hour${minutes / 60 === 1 ? "" : "s"}`
        : `${minutes} minute${minutes === 1 ? "" : "s"}`;
    return recurring ? `every ${unit}` : `once, in ${unit}`;
  }

  toolRegistrar.registerTool({
    name: "org_create_reminder",
    label: "Schedule a durable reminder",
    description:
      "Schedule a durable, recurring reminder for yourself, or for someone you manage. This is the only way to make something happen again later: a reminder is durable, so it survives being stopped and relaunched, and it is delivered as a message when it comes due. The cadence must be at least one minute — anything faster is a poll, not a reminder.",
    parameters: Type.Object({
      prompt: Type.String({ minLength: 1, description: "Exactly what you want to be told when it fires — your own words, delivered verbatim" }),
      intervalMs: Type.Integer({
        minimum: MIN_REMINDER_INTERVAL_MS,
        description: `Delay in milliseconds before the reminder fires, and the cadence when recurring. Minimum ${MIN_REMINDER_INTERVAL_MS} for a one-shot. A RECURRING reminder must be at least ${MIN_RECURRING_REMINDER_INTERVAL_MS} (twice the settle window) and a faster one is refused: every fire delivers a turn, every turn resets the settle countdown, so a cadence inside that window holds the person resident for ever.`,
      }),
      recurring: Type.Optional(Type.Boolean({ description: "Defaults to true. Set false for a single fire." })),
      expiresAt: Type.Optional(Type.String({ description: "Optional ISO-8601 instant after which it stops re-arming" })),
      personId: Type.Optional(Type.String({ description: "Someone you manage; defaults to you. Arming on anyone else is refused." })),
    }),
    async execute(_toolCallId, params) {
      try {
        // No `createdByPersonId`: chiefd credits the reminder to the person
        // whose enrolled key authenticated this pane, which is the same
        // identity it judges the manager scope against. Sending it from here
        // would be this pane telling the daemon who it is.
        const personId = params.personId?.trim().replace(/^@/, "") || context.personId;
        const { reminder } = await reminderCommand("arm", {
          personId,
          prompt: params.prompt.trim(),
          intervalMs: params.intervalMs,
          ...(params.recurring !== undefined ? { recurring: params.recurring } : {}),
          ...(params.expiresAt?.trim() ? { expiresAt: params.expiresAt.trim() } : {}),
        });
        if (!reminder) return toolResult(false, "chiefd accepted the reminder but returned no record.");
        return toolResult(
          true,
          `Scheduled reminder ${reminder.id} — ${reminderCadenceText(reminder.intervalMs, reminder.recurring)}, first due ${reminder.nextDueAt}.`,
          { reminder },
        );
      } catch (error) {
        return refusalResult(error);
      }
    },
    // The SCHEDULED card. Rendered here, by the tool itself, and deliberately
    // NOT mailed as a launcher envelope: the person arming a reminder is
    // already up and holding this turn, so a message announcing it would
    // manufacture fleet work in order to report fleet work — against THE HARD
    // RULE. Only the FIRED event needs a durable envelope, because only then
    // may the recipient be stopped.
    renderResult(result, _options, theme) {
      const details = result.details as { ok?: boolean; reminder?: OrganizationReminderRecord } | undefined;
      if (!details?.ok || !details.reminder) {
        return renderCard(theme, {
          kind: "tool-failure", icon: "failure", title: "Reminder not scheduled",
          body: { kind: "prose", text: toolOutputText(result), previewChars: 120 }, boxed: false,
        });
      }
      const reminder = details.reminder;
      const cadence = reminderCadenceText(reminder.intervalMs, reminder.recurring);
      // The prompt is echoed back verbatim. A confirmation that omits WHAT was
      // scheduled cannot be checked for correctness by the person reading it.
      return renderCard(theme, {
        kind: "tool-success", icon: domainIcon("⏰", "success"), title: "Reminder scheduled", target: cadence,
        body: { kind: "lines", lines: [
          { text: reminder.prompt, token: "dim" },
          { text: `first due ${reminder.nextDueAt} · survives restarts · stop it with org_stop_reminder ${reminder.id}`, token: "dim" },
        ] }, boxed: false,
      });
    },
  });

  toolRegistrar.registerTool({
    name: "org_list_reminders",
    label: "List your durable reminders",
    description: "List the durable reminders scheduled for you, or for someone you manage, armed ones first. Stopped reminders are retained as history and shown with their fire count.",
    parameters: Type.Object({
      personId: Type.Optional(Type.String({ description: "Someone you manage; defaults to you. Listing anyone else's is refused." })),
    }),
    async execute(_toolCallId, params) {
      try {
        const personId = params.personId?.trim().replace(/^@/, "") || context.personId;
        const { reminders } = await reminderCommand("list", { personId });
        const rows = reminders ?? [];
        const armed = rows.filter((row) => row.status === "active");
        return toolResult(
          true,
          rows.length
            ? `${armed.length} armed reminder${armed.length === 1 ? "" : "s"}, ${rows.length - armed.length} stopped.`
            : "No reminders scheduled.",
          { reminders: rows },
        );
      } catch (error) {
        return refusalResult(error);
      }
    },
    renderResult(result, { expanded }, theme) {
      const details = result.details as { ok?: boolean; reminders?: OrganizationReminderRecord[] } | undefined;
      if (!details?.ok) {
        return renderCard(theme, {
          kind: "tool-failure", icon: "failure", title: "Could not read reminders",
          body: { kind: "prose", text: toolOutputText(result), previewChars: 120 }, boxed: false,
        });
      }
      const rows = details.reminders ?? [];
      if (!rows.length) {
        return renderCard(theme, {
          kind: "tool-success", icon: domainIcon("⏰"), title: "No reminders scheduled", body: { kind: "none" }, boxed: false,
        });
      }
      const armed = rows.filter((row) => row.status === "active");
      const shown = expanded ? rows : armed;
      return renderCard(theme, {
        kind: "tool-success", icon: domainIcon("⏰", "success"), title: `${armed.length} armed`, target: `${rows.length - armed.length} stopped`,
        body: { kind: "lines", lines: shown.map((row) => ({
          text: `${row.status === "active" ? "•" : "·"} ${row.id} — ${reminderCadenceText(row.intervalMs, row.recurring)}`
            + `${row.status === "active" ? `, next ${row.nextDueAt}` : ", stopped"}`
            + ` — ${row.prompt}`,
          token: "dim",
        })) }, boxed: false,
      });
    },
  });

  toolRegistrar.registerTool({
    name: "org_stop_reminder",
    label: "Remove a durable reminder",
    description: "Stop a durable reminder you no longer want. It stops firing immediately. The record is kept as history (with its fire count), so the id is never reused.",
    parameters: Type.Object({
      reminderId: Type.String({ minLength: 1 }),
      personId: Type.Optional(Type.String({ description: "Someone you manage; defaults to you. Stopping anyone else's is refused." })),
    }),
    async execute(_toolCallId, params) {
      try {
        const personId = params.personId?.trim().replace(/^@/, "") || context.personId;
        const { reminder } = await reminderCommand("stop", {
          personId,
          reminderId: params.reminderId.trim(),
        });
        if (!reminder) return toolResult(false, "chiefd stopped the reminder but returned no record.");
        return toolResult(true, `Removed reminder ${reminder.id}. It fired ${reminder.fireCount ?? 0} time${(reminder.fireCount ?? 0) === 1 ? "" : "s"}.`, { reminder });
      } catch (error) {
        return refusalResult(error);
      }
    },
    // The REMOVED card. "Stopped" and "removed" are ONE operation with one
    // card: the operator's word is "removed", and the row surviving underneath
    // is an implementation fact (it keeps the id from being recycled into an
    // effect-id collision), not a second state worth teaching anyone.
    renderResult(result, _options, theme) {
      const details = result.details as { ok?: boolean; reminder?: OrganizationReminderRecord } | undefined;
      if (!details?.ok || !details.reminder) {
        return renderCard(theme, {
          kind: "tool-failure", icon: "failure", title: "Reminder not removed",
          body: { kind: "prose", text: toolOutputText(result), previewChars: 120 }, boxed: false,
        });
      }
      const reminder = details.reminder;
      const fired = reminder.fireCount ?? 0;
      return renderCard(theme, {
        kind: "tool-success", icon: domainIcon("⏰", "success"), title: "Reminder removed", target: `fired ${fired} time${fired === 1 ? "" : "s"}`,
        body: { kind: "lines", lines: [{ text: reminder.prompt, token: "dim" }] }, boxed: false,
      });
    },
  });

  // REGISTRATION IS NOT AUTHORITY. The subtree family goes to everybody,
  // whatever their kind, because every one of its handlers checks scope and a
  // leaf that heads nothing is refused by that check — a state, not a title.
  // Only the four tools whose handlers genuinely decide by kind stay gated.
  //
  // These two calls were one call behind this same `if`, which is how a live
  // CEO came to tell its operator that a Chief of Staff "doesn't hold the
  // org-management tools needed to create a department": the authority layer
  // said yes, chiefd's create path said yes, and the pane was never handed the
  // verb to ask with.
  await installSubtreeTools(toolRegistrar, context, () => latestExtensionContext?.modelRegistry);
  // NO KIND GATE. `installRootExecutiveTools` registers `org_escalate_to_operator`
  // alone, and it decides for itself by asking whether this person has a manager
  // to escalate TO — a fact about the tree, not about the person. The
  // `manager(person)` that used to wrap this call was the last role gate in the
  // registration path, and it was redundant even for that tool: a structural
  // root is an executive by construction.
  await installRootExecutiveTools(toolRegistrar, context);

  // A native session replacement invalidates the live context without moving
  // sessionContextEpoch: the epoch only advances at session_start /
  // session_shutdown, and the replacement lands in between. Rejecting a stale
  // context here upgrades every post-await resume point in this extension at
  // once, rather than guarding one caller and leaving its siblings exposed.
  const currentSessionContext = (epoch = sessionContextEpoch): ExtensionContext | undefined => (
    !sessionMaintenanceShuttingDown && epoch === sessionContextEpoch && !isExtensionContextStale(latestExtensionContext)
      ? latestExtensionContext
      : undefined
  );
  const companyMaintenanceBlocked = async (): Promise<boolean> => {
    const projection = await projectSessionMaintenanceForRuntime(context);
    // A real open company action is a fleet-wide lifecycle fence. A failed
    // *projection* is not: the durable action cannot be identified or safely
    // executed, but it must not turn into an invisible global mailbox/work
    // freeze. `drainWithheldReason` records that diagnostic once while allowing
    // ordinary operator messages through so the company can recover.
    return Boolean(projection.blockingCompanyActionId && !projection.unresolvable);
  };
  // TOMBSTONE: `reconcileParkedCompanyMaintenance` and its
  // `lastParkedMaintenanceReconcileAt` cadence. It was the BUG-12 fleet-wedge
  // janitor: a company action blocked the fleet while any target had an open
  // request, including parked targets that would never boot to claim theirs,
  // so each Pi re-asked chiefd to skip them on a bounded cadence.
  //
  // It POSTED `/v1/org/company-session-action/skip-parked`, a route this branch
  // DELETES. Nothing can mint a company action any more, so the call was
  // already unreachable — but an unreachable caller of a deleted route is the
  // one residue that turns into a 404 loop the day anything revives its gate,
  // so it goes rather than staying dormant.

  /**
   * Why delivery is being withheld, or undefined when it is not.
   *
   * This guard is the SIXTH exit that can withhold mail, and the only one
   * outside `wakePendingOrganizationMailboxes`. It returns 0 before reading a
   * byte, so it emitted no journal event of any kind: the supervisor could
   * truthfully log "I requested a wake" while the consumer silently declined,
   * and the pair read as a healthy idle company. That is the nineteen-hour
   * failure again, on the consumer side.
   *
   * Conditions are reported individually because they have different
   * remedies, and `company_maintenance` carries the blocking action id — with
   * the unresolvable cases called out separately, because
   * `projectSessionMaintenanceForRuntime` is fail-closed and collapses
   * anything it cannot project to the literal id `"unknown"`. That is not an
   * action id, so nothing can ever clear it and every mailbox in the company
   * freezes permanently. An operator told only "blocked by unknown" would hunt
   * for an action that does not exist.
   *
   * The projection now names WHAT it could not resolve, and that name is
   * carried into the reason rather than flattened to "the ledger". An
   * unreachable write service and a genuinely corrupt document have different
   * remedies — restart the service, or point at the right database — and
   * reporting both as a corrupt session-maintenance ledger sent operators to
   * inspect a file that was perfectly well-formed.
   */
  let isolatedMaintenanceFault: string | undefined;
  const drainWithheldReason = async (epoch: number): Promise<{ reason: string; blockingCompanyActionId?: string; unresolvableDetail?: string } | undefined> => {
    if (!currentSessionContext(epoch)) return { reason: "no_current_session_context" };
    if (nativeCompaction.requestId) return { reason: "native_compaction_in_flight" };
    const projection = await projectSessionMaintenanceForRuntime(context);
    const blockingCompanyActionId = projection.blockingCompanyActionId;
    if (!blockingCompanyActionId) return undefined;
    // Branch on the structured cause, never on the sentinel string: a real
    // company action id is operator-supplied and could in principle be the
    // word "unknown", and `unresolvable` is set exactly when the projection
    // refused.
    const unresolvable = projection.unresolvable;
    if (unresolvable) {
      // A malformed or temporarily unreadable maintenance document has no
      // trustworthy company action id to protect. Keep maintenance execution
      // fail-closed, but do not turn that diagnostic into a permanent,
      // fleet-wide mailbox blackout: ordinary durable messages are the only
      // path an operator and the CEO have to recover it. Log once per distinct
      // fault so it is actionable without flooding the mailbox journal.
      const fingerprint = `${unresolvable.cause}\u0000${unresolvable.detail}`;
      if (isolatedMaintenanceFault !== fingerprint) {
        isolatedMaintenanceFault = fingerprint;
        appendOrganizationLogLine(context, "mailbox", "maintenance-projection-isolated", "error", {
          cause: unresolvable.cause,
          detail: unresolvable.detail,
          action: "mail_delivery_continues_while_maintenance_execution_remains_refused",
        });
        // Keep the durable, operator-facing event trail in step with the
        // structured runtime log. The log gives the full diagnosis; this
        // compact edge event lets status/triage surfaces prove that mail was
        // deliberately kept available instead of silently black-holed.
        appendOrganizationEvent(context, {
          event: "maintenance-projection-isolated",
          cause: unresolvable.cause,
          action: "mail_delivery_continues_while_maintenance_execution_remains_refused",
          at: new Date().toISOString(),
        });
      }
      return undefined;
    }
    isolatedMaintenanceFault = undefined;
    return { reason: "company_maintenance", blockingCompanyActionId };
  };
  /**
   * Edge-triggered. `drain` is polled, so a line per call would bury the signal
   * in exactly the volume this logging exists to replace. One line when the
   * reason starts, one when it changes, and one when delivery resumes carrying
   * how long it was withheld and how many polls were declined — which is the
   * question an operator actually has.
   *
   * # A CLEAN DRAIN IS DELIBERATELY TRACELESS HERE, and that has misled twice
   *
   * This records only GATED drains. A drain that ran and delivered writes
   * nothing to `mailbox.jsonl`, so **a silent mailbox log does not mean no
   * drain ran** — it usually means every drain was clean. On 2026-08-27 that
   * silence was read as "the drain never ran" by two separate people during a
   * live delivery incident, and the actual mechanism was the opposite: the
   * drain ran repeatedly and its delivery was parked.
   *
   * The delivery trace lives on the BUS (`.chief/bus/events.jsonl`,
   * `message-queue-requested`), which is where an envelope's journey can
   * actually be followed. Said here rather than fixed by adding a line per
   * clean drain, because that volume is exactly what this edge-triggering
   * exists to avoid.
   */
  let withheld: { reason: string; sinceMs: number; polls: number } | undefined;
  const recordDrainGate = (current: { reason: string; blockingCompanyActionId?: string; unresolvableDetail?: string } | undefined): void => {
    const now = Date.now();
    if (!current) {
      if (withheld) {
        appendOrganizationLogLine(context, "mailbox", "drain-resumed", "info", {
          previousReason: withheld.reason,
          withheldForMs: now - withheld.sinceMs,
          declinedPolls: withheld.polls,
        });
        withheld = undefined;
      }
      return;
    }
    if (withheld?.reason === current.reason) {
      withheld.polls += 1;
      return;
    }
    withheld = { reason: current.reason, sinceMs: now, polls: 1 };
    appendOrganizationLogLine(context, "mailbox", "drain-withheld", "warn", {
      reason: current.reason,
      ...(current.blockingCompanyActionId ? { blockingCompanyActionId: current.blockingCompanyActionId } : {}),
      // The remedy, in the one line an operator actually reads. Without it the
      // reason names the failing authority but not what to check about it.
      ...(current.unresolvableDetail ? { unresolvableDetail: current.unresolvableDetail } : {}),
    });
  };
  // #291 (part B): `drainOrganizationMailbox` reads the mailbox doc ONCE at
  // its start. A same-epoch trigger that joins an in-flight drain
  // (`previous.epoch === epoch`, below) used to just return the in-flight
  // promise with no record of having arrived — if that trigger's own
  // envelope was written after the in-flight drain's read, it was invisible
  // to it and nothing ever re-read for it. `mailboxRedrainRequested` is a
  // dirty flag: joining sets it, and the in-flight operation's completion
  // schedules exactly one follow-up `drain(epoch)` if it is still set,
  // collapsing any number of same-epoch joins into one guaranteed re-read.
  let mailboxRedrainRequested = false;
  const drain = async (epoch = sessionContextEpoch): Promise<number> => {
    // Pi's native compact() is asynchronous but reports the session as idle.
    // Never let a mailbox poll start a new agent turn until its callback proves
    // the summary and branch rebuild finished; the envelope remains on disk.
    const gate = await drainWithheldReason(epoch);
    recordDrainGate(gate);
    if (gate) return 0;
    const previous = mailboxDrainInFlight;
    if (previous) {
      if (previous.epoch === epoch) {
        mailboxRedrainRequested = true;
        return previous.promise;
      }
      // A replacement waits for the retired request boundary, then re-reads
      // disk and drains through only its current Pi session epoch.
      return previous.promise.catch(() => 0).then(() => currentSessionContext(epoch) ? drain(epoch) : 0);
    }
    const operation = drainOrganizationMailbox(
      pi,
      context,
      mailboxDeliveryAttempts,
      () => Boolean(currentSessionContext(epoch)),
    ).catch((error: unknown) => {
      // A daemon restart window makes the docstore briefly unreachable. The
      // drain itself must keep throwing (property #4: never read an outage as
      // "no mail"), but a POLL that hits it is deferred work, not a fatal one:
      // degrade this pass to zero deliveries, keep every durable envelope
      // untouched for the next trigger, and never let a transient rejection
      // escape as an uncaughtException that takes the whole pane down (#617).
      if (!isTransientTransportFailure(error)) throw error;
      recordDrainGate({ reason: `unreachable: ${safeExceptionMessage(error)}` });
      appendOrganizationLogLine(context, "mailbox", "drain-deferred", "warn", {
        reason: safeExceptionMessage(error),
      });
      return 0;
    });
    mailboxDrainInFlight = { epoch, promise: operation };
    const clearOperation = () => {
      if (mailboxDrainInFlight?.promise === operation) mailboxDrainInFlight = undefined;
    };
    // Cleanup must consume both outcomes. Discarding `operation.finally(...)`
    // would create a second rejected promise at this lifecycle boundary.
    void operation.then(clearOperation, clearOperation);
    // Attached after the cleanup `.then` above, so `mailboxDrainInFlight` is
    // already cleared by the time this runs and the follow-up `drain(epoch)`
    // starts a genuinely fresh operation rather than re-joining itself.
    void operation.finally(() => {
      if (mailboxRedrainRequested) {
        mailboxRedrainRequested = false;
        // Fire-and-forget like every mailbox poll: its rejection (e.g. chiefd
        // unreachable through a daemon restart window) must never escape as an
        // uncaughtException that takes the whole pane down (#617).
        if (currentSessionContext(epoch)) void drain(epoch).catch(() => 0);
      }
    // `.finally` forwards the operation's own rejection into a new promise;
    // leaving it unobserved is the same pane-killing unhandled rejection.
    }).catch(() => {});
    return operation;
  };
  const processMaintenance = (settledLease?: SessionMaintenanceLifecycleLease): Promise<boolean> => {
    if (sessionMaintenanceInFlight) return sessionMaintenanceInFlight;
    const lifecycleLease = settledLease ?? sessionMaintenanceLifecycleFence.capture(latestExtensionContext);
    if (!lifecycleLease && !sessionMaintenanceDeferralRetry.requestId) return Promise.resolve(false);
    const operation = processSessionMaintenance(
      pi,
      context,
      latestExtensionContext,
      sessionMaintenanceClaimToken,
      sessionMaintenanceStartRetry,
      now,
      sessionMaintenanceLifecycleFence,
      lifecycleLease,
      sessionMaintenanceDeferralRetry,
      nativeCompaction,
      () => resumeAfterNativeCompaction(),
    );
    sessionMaintenanceInFlight = operation;
    void operation.then(
      () => { if (sessionMaintenanceInFlight === operation) sessionMaintenanceInFlight = undefined; },
      () => { if (sessionMaintenanceInFlight === operation) sessionMaintenanceInFlight = undefined; },
    );
    return operation;
  };
  /**
   * THE CLAIM POINT THAT EXISTS BECAUSE EVERY OTHER ONE IS INSIDE A TURN.
   *
   * `org_maintain_session action=compact` worked everywhere except the one
   * case it exists for. Both existing claim points — the `agent_settled`
   * handler and the SSE-driven `runMaintenanceCycle` — need a lifecycle lease,
   * and a lease needed `agent_settled` plus a pane that is still idle and
   * queue-free by the time the claim is reached. A session over the provider's
   * context ceiling never holds that window: it settles (Pi emits
   * `agent_settled` from a `finally`, so even a provider-rejected turn
   * settles), but it always has more queued work, so the next turn's start
   * boundary invalidates the epoch while the settled handler is still awaiting
   * its own docstore reads and mail drain. The claim was then never attempted
   * at all — on the live reproduction the request sat `queued` for over an
   * hour with four `provider-turn-failed` 400s beside it and not one
   * `session-maintenance-started` event.
   *
   * So the pane checks BEFORE it starts a turn. Pi awaits `before_agent_start`
   * inside `prompt()`, before the agent run begins and before the provider is
   * contacted, which makes it both a legal boundary (no run, no tool) and an
   * uncontended one (nothing else in this process can start a turn while the
   * handler is awaited). A session that cannot survive a turn can still
   * compact, because compacting is now something it does instead of starting
   * the turn, not something it does after finishing one.
   */
  const maintainBeforeTurn = async (extensionContext: ExtensionContext | undefined): Promise<void> => {
    if (sessionMaintenanceShuttingDown || !sessionMaintenanceStartupReady) return;
    if (extensionContext) latestExtensionContext = extensionContext;
    // A cycle already running owns the fence. Joining it is the same dedup
    // every other trigger uses; re-entering underneath it would race the claim.
    const blocking = sessionMaintenancePollInFlight ?? sessionMaintenanceInFlight;
    if (blocking) {
      await blocking.catch(() => undefined);
      return;
    }
    const lease = sessionMaintenanceLifecycleFence.beforeTurn(latestExtensionContext);
    if (!lease) return;
    let started = false;
    try {
      started = await processMaintenance(lease);
    } catch (error) {
      // A pre-turn probe must never reject out of Pi's own prompt path. The
      // request is durable and the settled/SSE cycles still own the retry.
      const retryable = isExpectedLifecycleProjectionError(error);
      if (!retryable) logOrganizationException(context, "session-maintenance-pre-turn-deferred", error);
      appendOrganizationEvent(context, {
        event: "session-maintenance-pre-turn-deferred",
        personId: context.personId,
        error: safeExceptionMessage(error),
        retryable,
        at: new Date().toISOString(),
      });
      return;
    }
    if (!started) return;
    const settled = nativeCompaction.settled;
    if (!settled) return;
    // Hold Pi's prompt until the branch has actually shrunk — bounded, because
    // an unbounded wait on a callback would be a new way to wedge the pane.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const bound = new Promise<"timeout">((resolve) => {
      timer = setTimeout(() => resolve("timeout"), ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS);
      (timer as { unref?: () => void }).unref?.();
    });
    // THE HOLD IS WORK TOO, and it is chief's own doing: this pane is waiting
    // on a compaction it asked for, for up to
    // `ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS`, emitting nothing. That wait's
    // existing bound IS the ceiling here — no new constant, and none wanted.
    beginBusyWork("pre-turn-compaction-hold");
    try {
      const outcome = await Promise.race([settled.then(() => "settled" as const), bound]);
      if (outcome === "timeout") {
        appendOrganizationEvent(context, {
          event: "session-maintenance-pre-turn-compaction-unfinished",
          personId: context.personId,
          waitedMs: ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS,
          at: new Date().toISOString(),
        });
      }
    } finally {
      if (timer) clearTimeout(timer);
      endBusyWork();
    }
  };
  // TOMBSTONE: `interruptForcedMaintenance`. It preempted a running turn so a
  // forced maintenance request could claim the pane — which existed for
  // `fresh_session`, since a compaction waits for a boundary rather than
  // taking one. Its `ctx.abort()` was the ONE abort site in this file on an
  // operator-relevant path; the two that remain are the empty-send circuit
  // breaker, which is a different thing entirely.
  // #827: `pollIntervalMs` no longer names a floor cadence (the floor is
  // deleted, D0 — no configurable poll-only mode). It survives ONLY as a
  // test/fixture seam: `0` fully disables background reactive machinery (no
  // `SseWatcher` construction at all), which conformance's deterministic
  // single-call fixtures depend on (recorded by the deleted `conformance/lib/tool-host.ts`). Any
  // other value is accepted but has no cadence meaning any more — there is
  // no timer left for it to set.
  const backgroundActivityDisabled = options.pollIntervalMs === 0;
  const turnWatchdogThresholdMs = options.turnWatchdogMs === undefined ? ORGANIZATION_TURN_WATCHDOG_MS : Math.max(0, options.turnWatchdogMs);
  const turnWatchdogIntervalMs = options.turnWatchdogIntervalMs === undefined ? ORGANIZATION_TURN_WATCHDOG_INTERVAL_MS : Math.max(0, options.turnWatchdogIntervalMs);
  // --- the settle countdown's idleness beat (operator, 2026-08-10) ----------
  //
  // "An agent can be settling while thinking. If it starts doing stuff, the
  // settling countdown is turned off. Only when the agent idles is when you
  // kick off the settle countdown."
  //
  // chiefd stamped its quiet lease from the ABSENCE OF DURABLE DEMAND -- no
  // open goal, no open assignment, no loop -- which says nothing about whether
  // this process is mid-turn. So a person whose goals had all closed started
  // settling while it was thinking, calling tools and sending mail, and could
  // be park-admitted underneath its own turn. The pane is the only thing that
  // knows, so the pane says so.
  //
  // The event set is not a new list: it is exactly the set that already feeds
  // `noteTurnProgress` below, which is why the beat is issued FROM it rather
  // than from a parallel enumeration that could drift out of step.
  //
  // Fire-and-forget by construction: a beat is presentation-and-lifecycle
  // hygiene, never part of a turn's critical path, so a failed or slow write
  // must not delay, block or fail the agent's work. A lost beat costs at most
  // one liveness window and the next event re-sends.
  let agentActivityBeatAt = 0;
  let agentActivityBeatWorking: boolean | undefined;
  const noteAgentActivityBeat = (working: boolean) => {
    const at = now();
    // Send on a genuine state CHANGE always; while working, re-send no more
    // often than one beat per interval.
    if (agentActivityBeatWorking === working
      && (!working || at - agentActivityBeatAt < ORGANIZATION_AGENT_ACTIVITY_BEAT_INTERVAL_MS)) {
      return;
    }
    agentActivityBeatWorking = working;
    agentActivityBeatAt = at;
    void (async () => {
      try {
        await chiefdPostJson<Record<string, unknown>>(
          chiefdEndpoint(context),
          ACTIVITY_ROUTES.agentState,
          { slug: companyKeyOf(context), callerPersonId: context.personId, working },
        );
      } catch {
        // Deliberately silent. The next event re-sends, and an unreachable
        // daemon must never surface as a turn failure.
        if (agentActivityBeatWorking === working) agentActivityBeatWorking = undefined;
      }
    })();
  };
  const noteTurnProgress = () => { turnProgressAt = now(); noteAgentActivityBeat(true); };
  // BUSY BUT SILENT. Every state below is real work that emits no turn events,
  // so `noteTurnProgress` never fires and chiefd reads the person as quiet.
  //
  // The operator's ruling is the whole specification: *"if it reads the mail
  // and it's thinking, leave it until it settles then start the timer."* The
  // countdown may not run while somebody is working. What counts as working is
  // not a judgement this code makes — it is the set of states that produce no
  // events, enumerated:
  //
  //   1. COMPACTION, Pi-native or chief-initiated. Handled here.
  //   2. The PRE-TURN COMPACTION HOLD chief itself owns. Handled here.
  //   3. THINKING — NO CODE, and deliberately none. `pi-ai`'s
  //      `openai-completions.js` streams DeepSeek's `reasoning_content` as
  //      `thinking_start`/`thinking_delta`, which arrive as `message_update`,
  //      which already calls `noteTurnProgress`. Adding machinery for a covered
  //      case would be inventing a second answer to a question that has one.
  //   4. PROVIDER GAPS and SILENT TOOLS — NO CODE. Both are bounded already:
  //      the liveness window for the first, the foreground-bash deadline for
  //      the second.
  let busyBeat: ReturnType<typeof setInterval> | undefined;
  let busyBeatStartedAt = 0;
  const stopBusyBeat = () => {
    if (busyBeat) clearInterval(busyBeat);
    busyBeat = undefined;
    busyBeatStartedAt = 0;
  };
  /** Beat `working:true` now, and keep beating until `endBusyWork`. */
  const beginBusyWork = (reason: string) => {
    noteAgentActivityBeat(true);
    if (busyBeat) return;
    busyBeatStartedAt = now();
    busyBeat = setInterval(() => {
      // THE CEILING, and it is a floor rather than a pin: past it the ordinary
      // settle owns the person again with no residue. A hold that could not
      // expire would be a new way to wedge a pane for ever, which is the exact
      // reasoning `ORGANIZATION_PRE_TURN_COMPACTION_WAIT_MS` records.
      if (now() - busyBeatStartedAt >= ORGANIZATION_COMPACTION_BEAT_CEILING_MS) {
        appendOrganizationEvent(context, {
          event: "busy-beat-ceiling-reached",
          personId: context.personId,
          reason,
          heldMs: ORGANIZATION_COMPACTION_BEAT_CEILING_MS,
          at: new Date().toISOString(),
        });
        stopBusyBeat();
        return;
      }
      noteAgentActivityBeat(true);
    }, ORGANIZATION_AGENT_ACTIVITY_BEAT_INTERVAL_MS);
    (busyBeat as { unref?: () => void }).unref?.();
  };
  /**
   * The busy work ended.
   *
   * `working:false` only when NO TURN IS IN FLIGHT: a compaction can complete
   * into a running turn (a queued prompt starts the moment the branch shrinks),
   * and reporting idle there would hand chiefd the opposite of the truth.
   */
  const endBusyWork = () => {
    stopBusyBeat();
    if (!turnInFlight) noteAgentActivityBeat(false);
  };
  // #368 idle-trends-to-zero: the watchdog only has work while a turn is in
  // flight, so its interval is ARMED at `turn_start` and DISARMED the instant
  // the turn settles — never a blind `setInterval` ticking every 60 s at rest.
  // A converged company at idle runs this timer zero times; only a live turn
  // pays for it. The `!turnInFlight` guard below is retained as a belt-and-
  // braces no-op for the one tick that can still be in-flight across a disarm.
  let turnWatchdogTimer: ReturnType<typeof setInterval> | undefined;
  const turnWatchdogTick = () => {
    try {
      if (!turnInFlight) return;
      if (turnWatchdogAbortIssued) {
        // The abort did not end the turn: Pi's abort is advisory inside a
        // parked tool/extension await, so this is the unrecoverable-in-process
        // case. Say so loudly once — pane kill + respawn is the remedy.
        if (!turnWatchdogUnrecoverableIssued && now() - turnWatchdogAbortAt >= ORGANIZATION_TURN_WATCHDOG_ESCALATION_MS) {
          turnWatchdogUnrecoverableIssued = true;
          appendOrganizationEvent(context, {
            event: "turn-watchdog-unrecoverable",
            personId: context.personId,
            stalledMs: now() - turnProgressAt,
            abortIssuedMs: now() - turnWatchdogAbortAt,
            remedy: "kill-pane; the runtime respawn restores service from durable state",
            at: new Date().toISOString(),
          });
        }
        return;
      }
      const stalledMs = now() - turnProgressAt;
      if (!turnProgressAt || stalledMs < turnWatchdogThresholdMs) return;
      turnWatchdogAbortIssued = true;
      turnWatchdogAbortAt = now();
      appendOrganizationEvent(context, {
        event: "turn-watchdog-abort",
        personId: context.personId,
        stalledMs,
        thresholdMs: turnWatchdogThresholdMs,
        at: new Date().toISOString(),
      });
      // The programmatic abort preserves queued messages; the ended run
      // settles, and the settled path re-drives drain and work resume.
      latestExtensionContext?.abort?.();
    } catch (error) {
      logOrganizationException(context, "turn-watchdog", error);
    }
  };
  const armTurnWatchdog = () => {
    if (turnWatchdogTimer) return;
    if (!(turnWatchdogIntervalMs && turnWatchdogThresholdMs)) return;
    turnWatchdogTimer = setInterval(turnWatchdogTick, turnWatchdogIntervalMs);
    (turnWatchdogTimer as { unref?: () => void } | undefined)?.unref?.();
  };
  const disarmTurnWatchdog = () => {
    if (!turnWatchdogTimer) return;
    clearInterval(turnWatchdogTimer);
    turnWatchdogTimer = undefined;
  };
  /**
   * The full session-maintenance/mail cycle (SSE-C2, #262): forced interrupt
   * -> processMaintenance -> drain/reconcile, guarded by the same
   * identity-fenced `sessionMaintenancePollInFlight` this file has always
   * used. Formerly the anonymous body of the 600ms `setInterval`; now shared
   * by three triggers instead of one: the 60s fallback-floor tick (skipped
   * while SSE is healthy), an SSE `session-maintenance`/`supervision`
   * doc-change event, and the immediate catch-up on `reorg`/channel-dead.
   * The in-flight guard check is synchronous (no `await` precedes it), so
   * whichever of those three fires first wins and the others bail
   * immediately against the same `sessionMaintenancePollInFlight` — the same
   * dedup the single old timer body already relied on against overlapping
   * ticks, now also covering cross-store coalescing (a `session-maintenance`
   * event and a `supervision` event racing each other collapse to one
   * cycle here, since `SseWatcher` itself only dedups per-store).
   *
   * #291: bailing against an in-flight cycle used to be a pure drop — the
   * trigger vanished with no record. That was safe under the old 600ms
   * poll (the next tick retried unconditionally); it is not safe now that
   * the floor is 60s and suppressed while the channel stays healthy, so a
   * dropped trigger could stall a queued request indefinitely. `bail` now
   * arms a dirty flag (`maintenanceRedrainRequested`) on whichever promise
   * is actually blocking (`sessionMaintenancePollInFlight` if this file's
   * own cycle owns it, else the raw `sessionMaintenanceInFlight` some other
   * caller started) and schedules exactly one follow-up `runMaintenanceCycle`
   * once it settles — covering `onEvent`, `onReorg`, and the dead-transition
   * catch-up uniformly, since all three funnel through this one function.
   */
  let maintenanceRedrainRequested = false;
  let maintenanceRedrainListenerArmed = false;
  const armMaintenanceRedrainListener = (blocking: Promise<unknown>): void => {
    if (maintenanceRedrainListenerArmed) return;
    maintenanceRedrainListenerArmed = true;
    void blocking.finally(() => {
      maintenanceRedrainListenerArmed = false;
      if (maintenanceRedrainRequested) {
        maintenanceRedrainRequested = false;
        void runMaintenanceCycle();
      }
    });
  };
  const runMaintenanceCycle = (): Promise<void> => {
    // A settled lifecycle owner has priority over passive polling, and a
    // trigger that arrives before startup or after shutdown has genuinely
    // nothing to schedule a follow-up for — neither is a race to coalesce.
    if (!sessionMaintenanceStartupReady || sessionMaintenanceShuttingDown) {
      return Promise.resolve();
    }
    const blocking = sessionMaintenancePollInFlight ?? sessionMaintenanceInFlight;
    if (blocking) {
      maintenanceRedrainRequested = true;
      armMaintenanceRedrainListener(blocking);
      return Promise.resolve();
    }
    const poll = (async () => {
      try {
        await processMaintenance();
        // The `else` arm re-asked chiefd to skip parked company-action
        // targets. Deleted with the route it posted; see the tombstone above.
        if (!(await companyMaintenanceBlocked())) await drain();
      } catch (error) {
        // A periodic optional maintenance probe must never reject out of the
        // caller and terminate the hosting Pi process. The request is
        // durable, so a later bounded cycle can safely retry it.
        const retryable = isExpectedLifecycleProjectionError(error);
        if (!retryable) logOrganizationException(context, "session-maintenance-poll-deferred", error);
        appendOrganizationEvent(context, {
          event: retryable ? "session-maintenance-poll-retry-deferred" : "session-maintenance-poll-deferred",
          personId: context.personId,
          error: safeExceptionMessage(error),
          retryable,
          at: new Date().toISOString(),
        });
      } finally { /* cleared only by the identity-fenced continuation below */ }
    })();
    sessionMaintenancePollInFlight = poll;
    void poll.finally(() => {
      if (sessionMaintenancePollInFlight === poll) sessionMaintenancePollInFlight = undefined;
    });
    return poll;
  };
  // #827: no fallback floor. SSE is the change channel — chiefd's change
  // feed guarantees delivery-or-reorg, and SseWatcher's own heartbeat
  // timeout + reconnect backoff already answer "did I miss something" / "is
  // the channel alive" / "how do I get back". A dead channel gets exactly
  // one catch-up cycle (below), never a recurring re-read. D0: there is no
  // env var anywhere that re-arms a poll-only mode — `backgroundActivityDisabled`
  // is a test/fixture-only seam (see its declaration above), not a product
  // switch.
  // SSE-C2 (#262)/#827: per-doc subscriptions are the sole wake path —
  // mailbox/<self> drains mail, session-maintenance/supervision re-run the
  // maintenance cycle. `stores` names must match SSE-A2's store-naming
  // convention exactly: the same literal
  // "session-maintenance"/"supervision" strings `projectSessionMaintenanceForRuntime`
  // reads (no constants exist for those upstream; ORGANIZATION_SSE_MAINTENANCE_STORES
  // is this file's own name for that pair, not a store the docstore defines).
  const sseMailboxStore = mailboxStoreName(context.personId);
  // Constructed unconditionally except for the test-only
  // `backgroundActivityDisabled` seam — there is no product kill switch.
  const sseWatcher: SseWatcherLike | undefined = backgroundActivityDisabled
    ? undefined
    : (options.createSseWatcher ?? ((watcherOptions: SseWatcherOptions): SseWatcherLike => subscribeSse(watcherOptions)))({
        url: options.sseUrl ?? chiefdEndpoint(context).url,
        // A4: the reader runs inside a pane that already holds this person's
        // key, and used to send `accept:` alone. It now presents the SAME
        // bearer the org tools present — the same manager, so one token cache
        // — and re-authenticates on every drop rather than replaying a header
        // the daemon may already have stopped honouring.
        bearer: organizationSseBearer(context, options.sseUrl),
        slug: companyKeyOf(context),
        stores: [sseMailboxStore, ...ORGANIZATION_SSE_MAINTENANCE_STORES],
        onEvent: async (event: SseDocChangeEvent) => {
          if (sessionMaintenanceShuttingDown) return;
          if (event.store === sseMailboxStore) {
            await drain();
            return;
          }
          if ((ORGANIZATION_SSE_MAINTENANCE_STORES as readonly string[]).includes(event.store)) {
            await runMaintenanceCycle();
            // #827 step 7: an idle-resume waiting on company-maintenance
            // unblocking wakes here instead of on its own bounded fallback
            // timer, the moment the doc-change that might have unblocked it
            // arrives.
            if (idleResumeMaintenanceWaitEpoch !== undefined) {
              const waitingEpoch = idleResumeMaintenanceWaitEpoch;
              idleResumeMaintenanceWaitEpoch = undefined;
              if (idleResumeMaintenanceFallbackTimer !== undefined) {
                clearTimeout(idleResumeMaintenanceFallbackTimer);
                idleResumeMaintenanceFallbackTimer = undefined;
              }
              scheduleIdleResume(waitingEpoch);
            }
          }
        },
        // Gap/restart-epoch resync: one full cycle (mail + maintenance,
        // exactly what the old single poll tick always did in one shot),
        // then resume live. Not awaited — matches SseWatcher's own contract
        // that `onReorg` fires without the caller blocking its reconnect.
        // #296: reorg is a resync trigger, not an unhealthy-channel signal —
        // kept separate from onChannelStateChange.
        onReorg: () => {
          void runMaintenanceCycle();
        },
        // #827: one catch-up cycle on dead, no re-arm — SseWatcher's own
        // reconnect backoff drives the retry from here.
        onChannelStateChange: (state: SseChannelState) => {
          if (state === "dead") void runMaintenanceCycle();
        },
      });
  // TOMBSTONE (#751/P4): the in-install half of the reflection delivery/retry
  // machine lived here -- `clearReflectionAcceptanceWait`,
  // `setReflectionDeliveryState`, `reflectionRecoveryCycleIsActive`,
  // `reflectionRecoveryMayProceed`, `clearReflectionRetryState`,
  // `exhaustReflectionRecovery`, `requestReflection`,
  // `requestReplacementReflection`, `scheduleReflectionAcceptanceCheck`,
  // `recoverUnacceptedReflectionRequest`, `reconcileActivity`,
  // `scheduleReflectionRetry`, and `recoverIncompleteReflectionTurn`. Together
  // they chased a bounded handoff the pane owed before park/bench/transfer/
  // offboard. There is no handoff to owe, so there is nothing to chase.
  const requestWorkResume = async (epoch = sessionContextEpoch) => {
    if (!currentSessionContext(epoch) || !workResumePending || workResumePrompted
      || nativeCompaction.requestId || await companyMaintenanceBlocked()) return;
    workResumePending = false;
    workResumePrompted = true;
    try {
      const details = await workResumeDetails(context, manifest, !bootResumedPriorSession);
      if (nativeCompaction.requestId) {
        workResumePending = true;
        workResumePrompted = false;
        return;
      }
      // BUG #42: classify the resumed agent before driving it. Previously every
      // resumed agent was handed the work-resume prompt unconditionally: one with
      // no open work would either error on the turn or linger idle (THE HARD RULE),
      // forcing the operator to manually re-resume. Now an agent with waiting
      // mail is force-resumed and one with none is settled/parked.
      await driveResumeRecovery({
        firstBoot: details.firstBoot === true,
        hasOpenWork: () => hasOpenOrganizationWork(context),
        forceResume: async () => {
          pi.sendMessage({
            customType: RESUME_TYPE,
            content: workResumePrompt(person, details),
            display: true,
            details,
          }, queuedPiDelivery("followUp"));
          appendOrganizationEvent(context, {
            event: "work-resume-prompt-requested",
            personId: context.personId,
            at: new Date().toISOString(),
          });
        },
        settle: async () => {
          appendOrganizationEvent(context, {
            event: "work-resume-settle",
            personId: context.personId,
            at: new Date().toISOString(),
          });
          // TOMBSTONE (#751/P4): this used to drive `reconcileActivity`, which
          // asked the pane for a bounded handoff before the graceful
          // settle/idle-park path. There is no handoff to ask for; chiefd's
          // own convergence parks a work-free person.
        },
      });
    } catch (error) {
      // BUG #42: an error on resume must never leave the agent stuck with its
      // one-shot latch consumed. Re-arm so the next lifecycle boundary
      // (agent_settled / idle-resume) re-drives classify-and-recover, instead of
      // requiring the operator to manually re-resume. Reactive, not a timer.
      workResumePending = true;
      workResumePrompted = false;
      logOrganizationException(context, "work-resume-prompt-deferred", error);
      appendOrganizationEvent(context, {
        event: "work-resume-prompt-deferred",
        personId: context.personId,
        error: safeExceptionMessage(error),
        at: new Date().toISOString(),
      });
    }
  };
  resumeAfterNativeCompaction = () => {
    // Native compact has rebuilt the branch. Resume the exact durable queues
    // that were intentionally held while Pi's provider summary was in flight.
    // TOMBSTONE (#751/P4): #751/R11 also released the reflection retries that
    // `scheduleReflectionRetry`'s runner parked for the duration of a native
    // compact. That whole retry queue is deleted, so the mail drain below is
    // all there is to resume.
    workResumePending = true;
    const epoch = sessionContextEpoch;
    const operation = (async () => {
      try {
        const delivered = await drain(epoch);
        if (!currentSessionContext(epoch) || nativeCompaction.requestId) return;
        if (delivered) {
          workResumePending = false;
          return;
        }
        await requestWorkResume(epoch);
      } catch (error) {
        logOrganizationException(context, "post-compaction-resume", error);
      }
    })();
    postCompactionResumeInFlight.add(operation);
    const clearOperation = () => postCompactionResumeInFlight.delete(operation);
    void operation.then(clearOperation, clearOperation);
  };
  /**
   * THE BOOT WINDOW CLOSED WITHOUT A FIRST TURN. Re-deliver what it parked.
   *
   * # The livelock this ends
   *
   * Everything delivered inside the window becomes `deliverAs: "nextTurn"`,
   * which parks it in Pi's `_pendingNextTurnMessages` — read only by the next
   * prompt submission. On a resume relaunch no first turn ever comes, so
   * nothing reads it. `mailboxDeliveryAttempts` is released only at
   * `agent_settled`, which needs that same absent turn, so the envelope is not
   * even retried within the session. The next retry rides a FRESH session with
   * a fresh window and parks again. Measured on a live company: one envelope,
   * queued twice ninety seconds apart, never consumed.
   *
   * # Order is load-bearing
   *
   * The leases are released BEFORE the drain, not after. A pane that has never
   * run a turn has had no settle to clear them, so its own parked first attempt
   * still holds every envelope — and a drain that ran first would find them all
   * leased and deliver nothing, which is the livelock wearing a different hat.
   *
   * # The resume prompt gets the same rescue, and needs its flags reset
   *
   * `requestWorkResume` guards on `workResumePending && !workResumePrompted`,
   * and the parked attempt set `workResumePrompted = true` — prompted into a
   * queue nobody will read. Re-driving without resetting would return early and
   * rescue nothing. Reaching the fallback IS the proof that no turn consumed
   * it: `agent_start` clears this timer.
   *
   * # Known cosmetic edge, judged and recorded
   *
   * A parked copy may ride the triggered turn alongside the re-delivered one,
   * so an envelope can present twice in that first turn. Pi 0.80's send path
   * does not collapse `_pendingNextTurnMessages` into a `triggerTurn` prompt —
   * they are separate queues drained by separate readers — and chief cannot
   * reach into either. **Twice-shown beats never-shown**, and the durable
   * acceptance path is unaffected because it keys on `message_start`, not on
   * how many copies the turn rendered.
   */
  const redeliverAfterBootWindow = async (epoch = sessionContextEpoch): Promise<void> => {
    if (!currentSessionContext(epoch)) return;
    try {
      // FIRST: a pane with no turn has no settle, so nothing else will ever
      // release these.
      mailboxDeliveryAttempts.clear();
      if (await companyMaintenanceBlocked()) return;
      await drain(epoch);
      if (!currentSessionContext(epoch)) return;
      if (workResumeNeedsRedrive(workResumePrompted, workResumePending)) {
        // Prompted into a queue nobody read. Reaching the fallback proves no
        // turn consumed it, so this is a re-drive rather than a second prompt.
        workResumePrompted = false;
        workResumePending = true;
      }
      await requestWorkResume(epoch);
    } catch (error) {
      logOrganizationException(context, "boot-window-redelivery", error);
    }
  };

  const scheduleIdleResume = (epoch: number) => {
    const delay = options.idleResumeDelayMs === undefined ? 1_000 : Math.max(0, options.idleResumeDelayMs);
    idleResumeTimer = setTimeout(() => {
      idleResumeTimer = undefined;
      const operation = (async () => {
        const ctx = currentSessionContext(epoch);
        if (!ctx) return;
        if (await companyMaintenanceBlocked()) {
          // #827: wait on the next session-maintenance/supervision doc-change
          // (the sseWatcher's onEvent below already resolves this wait) instead
          // of re-arming a fresh 1s timer forever while blocked. One bounded
          // fallback attempt guards a missed event.
          idleResumeMaintenanceWaitEpoch = epoch;
          if (idleResumeMaintenanceFallbackTimer === undefined) {
            idleResumeMaintenanceFallbackTimer = setTimeout(() => {
              idleResumeMaintenanceFallbackTimer = undefined;
              if (idleResumeMaintenanceWaitEpoch === epoch) {
                idleResumeMaintenanceWaitEpoch = undefined;
                scheduleIdleResume(epoch);
              }
            }, ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS);
            (idleResumeMaintenanceFallbackTimer as { unref?: () => void }).unref?.();
          }
          return;
        }
        if (ctx.isIdle?.() !== true) {
          // MOVEMENT IS NOT STARTUP WIRING, AND IT HAS A BETTER SIGNAL.
          //
          // A turn that has reported progress ends in `agent_settled`, which
          // clears `turnInFlight`, cancels this timer and drives
          // `requestWorkResume` itself. Counting to ten against a live turn
          // therefore waits one second at a time for an answer an EVENT is
          // already going to give, and then gives up — which is what every
          // launch on a live company did: four department heads each burned the
          // full ten-attempt budget, logged `work-resume-awaiting-idle
          // attempts: 10` as if something had failed, and settled seconds later
          // from `agent_settled` anyway. Hand off explicitly instead, and say
          // so: `awaiting-turn` is a different sentence from `awaiting-idle`.
          //
          // The bounded probe below is kept for the case it was written for and
          // is the only case left here — a restored session that is transiently
          // non-idle with NO turn in flight, so no `agent_settled` will ever
          // arrive for it.
          if (turnInFlight) {
            appendOrganizationEvent(context, {
              event: "work-resume-awaiting-turn",
              personId: context.personId,
              attempts: idleResumeReadyAttempts,
              at: new Date().toISOString(),
            });
            return;
          }
          // Pi reports non-idle briefly while a restored session is wiring its
          // extensions, even when there is no active turn to later settle. This
          // is a bounded startup-readiness probe, not a recurring work loop.
          if (++idleResumeReadyAttempts < ORGANIZATION_IDLE_RESUME_READY_ATTEMPTS && currentSessionContext(epoch)) scheduleIdleResume(epoch);
          else appendOrganizationEvent(context, {
            event: "work-resume-awaiting-idle",
            personId: context.personId,
            attempts: idleResumeReadyAttempts,
            at: new Date().toISOString(),
          });
          return;
        }
        const delivered = await drain(epoch);
        const current = currentSessionContext(epoch);
        if (!current || current.isIdle?.() !== true) return;
        // Mail is durable higher-priority work. A session without it gets one
        // recovery turn per Pi session so it does not sit inert after a
        // launcher restart or a native in-process session reset.
        if (!currentSessionContext(epoch) || delivered || current.isIdle?.() !== true) {
          if (delivered) workResumePending = false;
          return;
        }
        await requestWorkResume(epoch);
      })();
      idleResumeInFlight.add(operation);
      const clearOperation = () => idleResumeInFlight.delete(operation);
      void operation.then(clearOperation, clearOperation);
    }, delay);
    (idleResumeTimer as { unref?: () => void }).unref?.();
  };
  // TOMBSTONE: `nativeFreshSessionReplacement`, `failUnsupportedFreshSession`
  // and `scheduleLateNativeFreshSession` — the native session-replacement
  // machinery, and #1244's honest refusal for hosts that lack it.
  //
  // The API they called (`ctx.requestSessionReplacement`, and the
  // `agent_settled` result carrying a `newSession`) is chief's own patch to
  // Pi and exists in no released Pi. The operator ruled the FEATURE out, so
  // the machinery, the marker it wrote and the gate that refused for it all
  // go together rather than one surviving to guard the others.
  // Root cause is fixed at the read paths above; this is the last line of
  // defense. A stale-context throw escaping a lifecycle handler surfaces as a
  // visible extension error on a production agent, so degrade that racing turn
  // to a no-op instead. Only Pi's exact stale-context diagnostic is absorbed —
  // every other error keeps propagating unchanged.
  const guardStaleLifecycle = <E, C, R>(
    label: string,
    handler: (event: E, extensionContext: C) => Promise<R | undefined>,
  ) => async (event: E, extensionContext: C): Promise<R | undefined> => {
    try {
      return await handler(event, extensionContext);
    } catch (error) {
      if (!isStaleExtensionContextError(error)) throw error;
      logOrganizationException(context, label, error);
      return undefined;
    }
  };

  pi.on("session_start", guardStaleLifecycle("session-start-stale-context", async (event: unknown, ctx: ExtensionContext) => {
    // TOMBSTONE: a `/reload` whose hard contract had changed queued a
    // `fresh_session` for this person here, and swallowed the refusal into
    // `reload-hard-contract-fresh-session-deferred`. The action is deleted and
    // the predicate could not return `true` anyway — see the reload-hard-contract
    // tombstone above for which half of it a surviving mechanism still owns.

    // No fence check here. Session startup used to abort outright whenever the
    // company carried a suppression marker, with one narrow exception carved
    // out for a forced CEO fresh-session. Since the CEO's pane is started by
    // the CEO-only command itself, that made the CEO come up inert: no mailbox
    // delivery, no work resume, no provider probe -- for the entire session.
    sessionMaintenanceStartupReady = false;
    // A fresh session is back inside the boot window: chief's initial message
    // is about to be prompted bare, and nothing this extension does may start
    // a turn until it has.
    //
    // AND WHAT HAPPENS IF IT NEVER DOES. A resume relaunch passes no initial
    // message, so `agent_start` never comes and the fallback is what opens the
    // gate — at which point everything parked during the window is parked for
    // ever unless somebody re-delivers it. That somebody is this.
    closeFirstRunGate(FIRST_RUN_FALLBACK_MS, () => {
      void redeliverAfterBootWindow();
    });
    if (idleResumeTimer) clearTimeout(idleResumeTimer);
    idleResumeTimer = undefined;
    if (idleResumeMaintenanceFallbackTimer) clearTimeout(idleResumeMaintenanceFallbackTimer);
    idleResumeMaintenanceFallbackTimer = undefined;
    idleResumeMaintenanceWaitEpoch = undefined;
    const epoch = ++sessionContextEpoch;
    latestExtensionContext = ctx;
    sessionMaintenanceLifecycleFence.sessionStarted(ctx);
    resumedSessionId = sessionManagerOf(ctx)?.getSessionId?.();
    // #399: classify this boot as a genuine resume (prior transcript restored)
    // vs a first materialization (empty history) for the work-resume card/prompt.
    // #42 read this as "entries that are exclusively native-reset MARKERS mean
    // a fresh successor, not a resume". Native reset is deleted, so no session
    // can carry that marker any more and the question collapses to the one it
    // always meant underneath: does this session have any entries at all?
    bootResumedPriorSession = ((sessionManagerOf(ctx)?.getEntries?.() ?? []) as ReadonlyArray<unknown>).length > 0;
    const claim = sessionMaintenanceClaim(ctx, sessionMaintenanceClaimToken);
    let maintenance = await projectSessionMaintenanceForRuntime(context);
    // `"finish"` only. It took `"complete-native"` as well, for the startup
    // completion of an in-flight native reset — see the tombstone below, and
    // the route that verb posted is deleted.
    const startupMaintenanceCommand = async (action: "finish", payload: Record<string, unknown>) => {
      let lastError: unknown;
      // The maintenance ledger has its own tiny lock and an unrelated
      // supervision/reconcile transaction can briefly overlap startup. Keep
      // three bounded retries so that a proven native replacement is not
      // stranded by that transient control-plane contention.
      for (let attempt = 0; attempt < 4; attempt += 1) {
        try {
          return await sessionMaintenanceCommand(context, action, payload) as SessionMaintenanceRequest;
        } catch (error) {
          lastError = error;
          if (attempt < 3) await new Promise<void>((resolve) => setTimeout(resolve, 25 * (attempt + 1)));
        }
      }
      throw lastError;
    };
    // TOMBSTONE: the startup completion and recovery of an in-flight NATIVE
    // RESET — `nativeFresh`, `interruptedHistoricalNative`, and the
    // recover-one-successor arm under them.
    //
    // A native reset replaced a Pi session in place, so a process that died
    // mid-transition left a durable request whose source and target sessions
    // disagreed, and this block existed to finish or hand on exactly that. The
    // action is deleted, so no such request can exist to be recovered.
    // A process/extension crash may happen after Pi appends the native
    // compaction entry but before compact() invokes its callback. Inspect the
    // exact persisted branch anchor first and never invoke compact twice.
    const runningCompact = maintenance.running.find((request) => request.action === "compact" && request.compactSessionId);
    if (runningCompact) {
      const proof = nativeCompactionProof(ctx, runningCompact);
      if (proof.state !== "absent") {
        try {
          const finished = await startupMaintenanceCommand("finish", proof.state === "proven" ? {
            requestId: runningCompact.id,
            status: "completed",
            compactEntryId: proof.entryId,
          } : {
            requestId: runningCompact.id,
            status: "failed",
            error: "Native compaction receipt diverged from the persisted Pi session anchor; refusing to compact twice.",
          });
          showMaintenanceCard(pi, finished, proof.state === "proven" ? "completed" : "failed");
          maintenance = await projectSessionMaintenanceForRuntime(context);
        } catch (error) {
          logOrganizationException(context, "session-maintenance-native-compact-receipt-deferred", error, { requestId: runningCompact.id });
          return;
        }
      }
    }
    if (claim && maintenance.running.some((request) => request.claimedProcessId !== claim.processId
      || request.claimedSessionId !== claim.sessionId || request.claimToken !== claim.claimToken)) {
      try {
        const recovered = await sessionMaintenanceCommand(context, "recover", {
          processId: claim.processId,
          sessionId: claim.sessionId,
          claimToken: claim.claimToken,
        }) as RecoveredSessionMaintenance;
        if (recovered.interrupted.length) {
          const retry = recovered.replacements.at(-1);
          const terminal = recovered.interrupted.at(-1);
          if (retry) showMaintenanceCard(pi, retry, "queued");
          else if (terminal) showMaintenanceCard(pi, terminal, "failed");
          appendOrganizationEvent(context, {
            event: "session-maintenance-interrupted-recovered",
            personId: context.personId,
            interrupted: recovered.interrupted.length,
            replacements: recovered.replacements.length,
            ...(retry ? { nextAttempt: retry.attempt, retryNotBefore: retry.retryNotBefore } : {}),
            at: new Date().toISOString(),
          });
        }
        maintenance = await projectSessionMaintenanceForRuntime(context);
      } catch (error) {
        logOrganizationException(context, "session-maintenance-recovery-deferred", error);
      }
    }
    if (!currentSessionContext(epoch)) return;
    let completedFresh: SessionMaintenanceRequest | undefined;
    if (maintenance.applying && claim) {
      try {
        const completion = await sessionMaintenanceCommand(context, "complete", {
          requestId: maintenance.applying.id,
          processId: claim.processId,
          sessionId: claim.sessionId,
          claimToken: claim.claimToken,
        }) as { request?: SessionMaintenanceRequest } | SessionMaintenanceRequest | undefined;
        completedFresh = completion && typeof completion === "object" && "request" in completion
          ? completion.request
          : completion as SessionMaintenanceRequest | undefined;
      } catch (error) {
        // Session startup must not crash merely because the optional durable
        // completion probe cannot be read. The applying request is preserved
        // and the next fenced startup will retry it.
        logOrganizationException(context, "session-maintenance-complete-deferred", error);
      }
    }
    if (!currentSessionContext(epoch)) return;
    if (completedFresh) showMaintenanceCard(pi, completedFresh, "completed");
    sessionMaintenanceStartupReady = true;
    // Pi can start genuinely idle and never emit agent_settled. A fresh-session
    // request therefore claims at this authenticated startup boundary and
    // returns the same declarative host action used after active work settles.
    // Only native compaction keeps its existing passive/settled scheduling:
    // unlike replacement, it has a callback-capable host API and must not hold
    // startup.
    // TOMBSTONE: the startup arm that drove a queued NON-compact request into a
    // native session replacement at `session_start`. `compact` is the only
    // action now, and it is claimed by the ordinary maintenance path.
    if (!workResumePrompted) workResumePending = true;
    idleResumeReadyAttempts = 0;
    scheduleIdleResume(epoch);
  }));
  pi.on("before_agent_start", guardStaleLifecycle("before-agent-start-stale-context", async (_event: unknown, ctx: ExtensionContext) => {
    // `maintainBeforeTurn` opens with the fence's own `beforeTurn`, which
    // invalidates exactly as the bare `invalidate()` this boundary used to do.
    // The trailing invalidate closes the pre-turn lease again so it can never
    // survive into the turn Pi is about to run.
    await maintainBeforeTurn(ctx);
    sessionMaintenanceLifecycleFence.invalidate();
    return undefined;
  }));
  pi.on("agent_start", async () => {
    // The boot window is over the instant a run begins: from here a delivery
    // may start its own turn again, because there is no bare `prompt()` left
    // in flight for it to race.
    openFirstRunGate();
    sessionMaintenanceLifecycleFence.invalidate();
    resetEmptyOrganizationSendCircuit();
  });
  // #139/gh#516 (Option A): the shared `toolResult()` constructor records failure
  // only in `details.ok` and never sets the top-level `isError`, so across every
  // org-tool failure site a refused lifecycle call reached the agent as a
  // NON-ERROR result whose text merely described a failure — the CEO was shown a
  // successful tool call while no department existed. Flip `isError:true` at the
  // single shared seam, but ONLY on the wrapper's GENUINE-incident branch: a
  // retryable wait, a quiet input-repair, or the message-loop stop stays a
  // non-error because it is expected and benign, not a fault.
  //
  // Scope is pinned to the intercom wrapper by `details.opId`: `organizationTool-
  // Registrar`'s execute wrapper stamps `opId` on EXACTLY the genuine-incident
  // branch (`isGenuineToolFailure`) and nothing else in the process does, so its
  // presence uniquely identifies a wrapped org tool that this branch already
  // logged as an incident. That keeps this hook off other extensions' tools,
  // which share the `{ ok }` shape but carry their own benign `ok:false`
  // conditions and never go through this wrapper.
  // Thrown failures already reach the agent as isError:true (Pi flags the throw),
  // so the `event.isError` guard leaves them untouched. Returning just
  // `{ isError }` leaves `content`/`details` intact (the runner merges partial
  // patches), so no card presentation or mutation contract changes.
  pi.on("tool_result", async (event) => {
    if (event.isError) return undefined;
    const details = event.details as Record<string, any> | undefined | null;
    if (typeof details?.opId !== "string") return undefined;
    if (!isGenuineToolFailure(details)) return undefined;
    return { isError: true };
  });
  // #1208. THE GUARD THIS REPO THOUGHT IT ALREADY HAD.
  //
  // `team-ui.ts` carried a comment describing exactly this interception, beside
  // a flag nothing read and a helper nothing called — it was never wired, and
  // the operator has been meeting the consequence "randomly and often":
  //
  //   Error: Agent is already processing. Specify streamingBehavior
  //   ('steer' or 'followUp') to queue the message.
  //
  // It is built HERE rather than there because the racer is this extension's
  // own turn-triggering: the guard belongs where the busyness is caused, and
  // this is also the file with the logging spine. Panes that do not load the
  // intercom do not run its turn-triggers either, so they keep Pi's stock
  // behaviour and need no guard.
  pi.on("input", async (event, inputContext) => {
    const idle = (inputContext as { isIdle?: () => boolean } | undefined)?.isIdle?.() !== false;
    if (inputInterceptionDecision(event, idle) === "continue") {
      return { action: "continue" as const };
    }
    const images = Array.isArray((event as { images?: unknown }).images)
      ? ((event as { images?: unknown[] }).images ?? [])
      : [];
    const text = typeof (event as { text?: unknown }).text === "string" ? (event as { text: string }).text : "";
    const content = images.length > 0
      ? ([{ type: "text", text }, ...images] as Parameters<typeof pi.sendUserMessage>[0])
      : text;
    try {
      // Not awaited: the extension API's `sendUserMessage` returns void and
      // throws synchronously, so an `await` here would be awaiting a non-thenable.
      pi.sendUserMessage(content, { deliverAs: "followUp" });
    } catch (error) {
      // The rescue failed, so let Pi have the submission and throw its own
      // error rather than swallowing the text silently — a lost line the
      // operator can SEE is strictly better than one they cannot.
      logOrganizationException(context, "input-requeue-failed", error);
      return { action: "continue" as const };
    }
    // #645: the text belongs to Pi's session writer alone. This line exists so
    // the next "randomly and often" is a NUMBER somebody can grep out of
    // `.chief/logs` instead of a screenshot — which is all the evidence that
    // existed for this defect.
    appendOrganizationLogLine(
      context,
      "intercom",
      "input-requeued",
      "info",
      inputRequeueLogDetail(context.personId, event),
    );
    return { action: "handled" as const };
  });
  pi.on("turn_start", async () => {
    sessionMaintenanceLifecycleFence.invalidate();
    turnInFlight = true;
    piTurnInFlight = true;
    turnWatchdogAbortIssued = false;
    turnWatchdogUnrecoverableIssued = false;
    noteTurnProgress();
    armTurnWatchdog();
  });
  pi.on("message_start", async (event) => {
    noteTurnProgress();
    sessionMaintenanceLifecycleFence.invalidate();
    try {
      const acceptedEnvelope = await archiveStartedOrganizationMailboxMessage(event.message, context);
      if (acceptedEnvelope) {
        const startedDetails = event.message && typeof event.message === "object" && !Array.isArray(event.message)
          ? (event.message as unknown as Record<string, unknown>).details
          : undefined;
        const acceptedIds = isOrganizationMailboxBatch(startedDetails)
          ? new Set(startedDetails.envelopes.map(({ id }) => id))
          : new Set([acceptedEnvelope.id]);
        for (const file of mailboxDeliveryAttempts) {
          if ([...acceptedIds].some((id) => file.endsWith(`-${id}.json`))) mailboxDeliveryAttempts.delete(file);
        }
        // WHAT THIS TURN HAS EATEN. Recorded here because here is where the
        // envelope stops being retryable: after this receipt the mailbox no
        // longer holds it, so if the turn dies the sender's only remaining
        // evidence is silence. `agent_end` reads this to bounce them.
        for (const envelope of isOrganizationMailboxBatch(startedDetails) ? startedDetails.envelopes : [acceptedEnvelope]) {
          if (!acceptedIds.has(envelope.id)) continue;
          if (deliveriesConsumedThisTurn.some((entry) => entry.id === envelope.id)) continue;
          deliveriesConsumedThisTurn.push({ id: envelope.id, fromPersonId: envelope.fromPersonId });
        }
      }
      const started = event.message && typeof event.message === "object" && !Array.isArray(event.message)
        ? event.message as unknown as Record<string, unknown>
        : undefined;
      // TOMBSTONE (#751/P4): a REFLECTION_REQUEST_TYPE acceptance branch stood
      // here. It proved that a bounded-handoff prompt card had actually entered
      // this session (epoch + sequence + current-transition re-read) so an
      // unaccepted delivery could be retried. Nothing sends that prompt now.
      if (started?.customType === RESUME_TYPE) {
        // THE CORRELATION THAT MADE THIS FINDABLE. 61% of the 215 measured
        // printed-tool-call turns landed within six transcript rows of a resume
        // notice, and that number is the whole reason anybody looked at resume
        // as a suspect. Recording the flag keeps the correlation queryable
        // instead of re-derivable only by hand.
        sawResumeNoticeRecently = true;
        appendOrganizationEvent(context, { event: "work-resume-prompt-accepted", personId: context.personId, at: new Date().toISOString() });
      }
    } catch (error) {
      appendOrganizationEvent(context, {
        event: "message-start-handling-deferred",
        personId: context.personId,
        error: error instanceof Error ? error.message : String(error),
        at: new Date().toISOString(),
      });
    }
  });
  pi.on("message_update", () => { noteTurnProgress(); });
  pi.on("message_end", () => { noteTurnProgress(); });
  pi.on("tool_execution_start", async (event, eventContext) => {
    noteTurnProgress();
    sessionMaintenanceLifecycleFence.toolStarted(event?.toolCallId);
    observeRawOrganizationToolStart(event ?? {}, eventContext);
  });
  pi.on("tool_execution_update", () => { noteTurnProgress(); });
  pi.on("tool_execution_end", (event) => {
    noteTurnProgress();
    sessionMaintenanceLifecycleFence.toolEnded(event?.toolCallId);
  });
  pi.on("agent_end", async (event) => {
    turnInFlight = false;
    piTurnInFlight = false;
    disarmTurnWatchdog();
    resetEmptyOrganizationSendCircuit();
    const providerFailure = providerFailureDiagnostic(event);
    if (providerFailure) {
      providerFailureEpisodeId ??= randomUUID();
      // A request that does not fit the context window is not evidence about
      // the provider, so it must not move the provider's reliability counter.
      // Observed on `Taperoom Inc` (2026-08-18): eight of these were recorded
      // as `kind=provider_error`, which is what escalated a "check that Pi's
      // provider access and model health" alert to a manager while the provider
      // was healthy — the operator's own transient outage that day was a
      // DIFFERENT model, and mixing the two made both harder to read. It is
      // still recorded as a failed turn (the `provider-turn-failed` event and
      // the card below both fire); it simply never counts toward "N consecutive
      // provider failures", because no number of retries can clear it.
      const permanentRequestFailure = providerFailure.kind === "request_too_large";
      // A CONTENT REFUSAL IS NOT A RELIABILITY SIGNAL EITHER, and it is kept
      // off the counter for the same reason `request_too_large` is: it
      // describes what we sent, not whether the route is up. Measured on
      // a live box: 73 of these while the provider answered every other
      // turn normally, and the alert they eventually raised told a manager to
      // "check that Pi's provider access and model health" — advice that is
      // both wrong and unactionable when the provider is healthy and simply
      // declined the content. It escalates on its own terms below, on the
      // FIRST occurrence, because the consecutive counter's reset-on-success
      // provably starves it: a person filtered on one recurring topic and
      // healthy on everything else never reaches three in a row and was
      // therefore never reported at all.
      const contentRefusal = providerFailure.kind === "content_filter";
      // The third permanent kind, and the only one on this list whose remedy
      // belongs to a HUMAN: a 402 clears when somebody adds credits and never
      // otherwise, so counting it toward "N consecutive provider failures" both
      // inflates a reliability signal about a healthy route and routes the
      // alert to a manager AGENT who cannot act on it.
      const emptyAccount = providerFailure.kind === "insufficient_credits";
      if (!permanentRequestFailure && !contentRefusal && !emptyAccount) consecutiveProviderFailures += 1;
      // #399 part 2: a hard provider-configuration failure ("Provider is not
      // configured: <name>") is not a transient outage the reliability
      // escalation should wait N turns for — the pane simply cannot run. Render
      // one legible failure card (reason + remedy + log path) and persist the
      // underlying error to the exception log, instead of letting Pi dump the
      // raw string into the pane.
      const configError = providerConfigurationError(providerFailure.errorMessage);
      if (configError && !providerConfigurationCardShown) {
        providerConfigurationCardShown = true;
        logOrganizationException(context, "provider-not-configured", providerFailure.errorMessage, { provider: configError.provider });
        showPaneFailureCard(pi, {
          provider: configError.provider,
          personId: context.personId,
          logPath: join(organizationLogsDirectory(context), "exceptions.jsonl"),
        });
      }
      // The permanent counterpart: say plainly that the request could not fit,
      // and give the two numbers, so the reader is not left guessing whether
      // the network is down. Named cause, once, instead of a silent loop of
      // identical rejections that look like an outage.
      const tooLarge = providerRequestTooLargeError(providerFailure.errorMessage);
      if (tooLarge && tooLarge.limit !== requestTooLargeCardShownForLimit) {
        requestTooLargeCardShownForLimit = tooLarge.limit;
        logOrganizationException(context, "provider-request-too-large", providerFailure.errorMessage, {
          limit: tooLarge.limit,
          requested: tooLarge.requested,
        });
        showPaneFailureCard(pi, {
          personId: context.personId,
          logPath: join(organizationLogsDirectory(context), "exceptions.jsonl"),
          requested: tooLarge.requested,
          limit: tooLarge.limit,
        });
      }
      // The third card, and the one the operator was actually looking at when
      // they reported this: Pi's raw `Provider finish_reason: content_filter`
      // under the provider's own canned refusal in Chinese, which reads like
      // the agent said something rather than like the route declined to answer.
      // Say what happened, say the mail is gone, and give the two remedies that
      // exist — neither of which is waiting.
      if (contentRefusal && !contentFilterCardShown) {
        contentFilterCardShown = true;
        logOrganizationException(context, "provider-content-filter", providerFailure.errorMessage, {
          consumedDeliveries: deliveriesConsumedThisTurn.length,
        });
        showPaneFailureCard(pi, {
          personId: context.personId,
          logPath: join(organizationLogsDirectory(context), "exceptions.jsonl"),
          contentFiltered: true,
          consumedDeliveries: deliveriesConsumedThisTurn.length,
        });
      }
      // The one failure on this path the OPERATOR is the only possible actor
      // for. It gets the card like the others, and — unlike any of them — a
      // `ui.notify`, because the person who can fix it is the one holding the
      // terminal and not anybody in the roster. Deliberately NO manager mail:
      // mailing an agent "check provider access and model health" about an
      // empty account is what this fix removes.
      if (emptyAccount && !insufficientCreditsCardShown) {
        insufficientCreditsCardShown = true;
        logOrganizationException(context, "provider-insufficient-credits", providerFailure.errorMessage, {});
        showPaneFailureCard(pi, {
          personId: context.personId,
          logPath: join(organizationLogsDirectory(context), "exceptions.jsonl"),
          insufficientCredits: true,
        });
        try {
          latestExtensionContext?.ui.notify(
            `The provider account is out of credits (402), so @${displayHandle(context.organization, context.personId)} cannot run a turn. Nothing was retried and no model or session was changed; add credits to clear it.`,
            "error",
          );
        } catch { /* The card and the exception log remain the record. */ }
      }
      appendOrganizationEvent(context, {
        event: "provider-turn-failed",
        personId: context.personId,
        ...providerFailure,
        episodeId: providerFailureEpisodeId,
        // Turns are diagnostic only: replay could duplicate an assignment,
        // message, or trade. (TOMBSTONE #751/P4: the bounded-handoff prompt
        // used to be the one narrow exception here, because its transition
        // stayed durably pending until `org_reflect` committed it. Both are
        // deleted, so nothing is auto-retried.)
        automaticRetry: false,
        consecutiveFailures: consecutiveProviderFailures,
        at: new Date().toISOString(),
      });
      // THE BOUNCE. The one loss that was completely silent before: the
      // envelope was receipted at turn start, the turn then died, and the
      // sender's request ceased to exist with no signal to anybody. Iris sent
      // two operational requests into a void on a live box and had no
      // way to learn either was destroyed.
      //
      // Sent from inside the broken person, which is exactly why it works:
      // `sendOrganizationMessage` is an extension-side call and needs no model
      // turn, so a pane whose provider refuses every turn can still tell the
      // truth about that. One message per distinct SENDER, listing the envelope
      // ids that sender lost, with an id derived from the episode and the
      // sender so a replayed handler cannot double-send.
      // EVERY FAILED KIND, not only the refusal that led us here. The
      // destruction is a property of the ACCEPTANCE BOUNDARY, not of why the
      // turn died: the envelope is receipted at turn start and the turn then
      // ends without an answer, and that is as true of a 502 as of a content
      // refusal. Measured on a live box: ~2,226 accepted envelopes were
      // followed by a failed turn within 180s — about 22% of ALL mail in the
      // period — of which content_filter is 67. Bouncing only the refusal would
      // have left ~97% of the real loss silent through the next outage.
      //
      // The flood worry that argued for the narrow scope was real and the data
      // says where the flood actually came from: 313 "Provider reliability
      // alert" mails cycling through mailboxes that were themselves failing.
      // That amplifier is system mail, and system mail is excluded below — so
      // what is left is one bounce per destroyed REAL message, bounded by the
      // senders' own send rate, which is the honest volume.
      if (deliveriesConsumedThisTurn.length) {
        const bySender = new Map<string, string[]>();
        for (const { id, fromPersonId } of deliveriesConsumedThisTurn) {
          // A bounce, a content-refusal alert or a provider-health alert is not
          // a person's request and has no answer owed to it. Bouncing one
          // starts the ping-pong `isSystemFailureMessageId` exists to stop.
          if (isSystemFailureMessageId(id)) continue;
          bySender.set(fromPersonId, [...(bySender.get(fromPersonId) ?? []), id]);
        }
        const bouncedAt = new Date().toISOString();
        for (const [sender, ids] of [...bySender].sort(([left], [right]) => left.localeCompare(right))) {
          // A person cannot mail themselves, and a system-notice sender is not
          // a roster person who could read a bounce.
          if (!sender || sender === context.personId) continue;
          const identity = createHash("sha256").update(JSON.stringify({
            organization: context.organization,
            personId: context.personId,
            episodeId: providerFailureEpisodeId,
            sender,
            ids,
          })).digest("hex").slice(0, 24);
          try {
            const one = ids.length === 1;
            // WHAT THE SENDER HAS TO DECIDE is whether to resend, and the one
            // fact that changes the answer is whether anything already ran.
            // `hadToolCall` is the honest signal for it: a turn that got as far
            // as a tool call may have executed it before the provider dropped
            // the turn, so a blind resend can duplicate a trade or an
            // assignment — the same reason nothing here replays.
            const partial = providerFailure.hadToolCall
              ? " The turn had already begun a tool call when it failed, so some of its work may have run — check before resending."
              : "";
            await sendOrganizationMessage(context, {
              to: sender,
              body: `@${displayHandle(context.organization, context.personId)} could not process ${one ? "your message" : "your messages"} (${ids.join(", ")}): the turn ended before completion (${providerFailure.kind}), so ${one ? "it was" : "they were"} receipted and NOT read. Nothing was retried and nothing is queued.${partial} Resend if it still matters, or route the work to somebody else.`,
            }, { id: `content-filter-bounce-${identity}`, now: bouncedAt });
            appendOrganizationEvent(context, {
              event: "message-bounced",
              personId: context.personId,
              recipientPersonId: sender,
              messageIds: ids,
              kind: providerFailure.kind,
              episodeId: providerFailureEpisodeId,
              automaticRetry: false,
              at: bouncedAt,
            });
          } catch (error) {
            // A bounce that cannot be sent must not turn a provider refusal
            // into an extension crash — the pane is already the degraded one.
            // The durable record of the loss is what survives.
            appendOrganizationEvent(context, {
              event: "message-bounce-deferred",
              personId: context.personId,
              recipientPersonId: sender,
              messageIds: ids,
              episodeId: providerFailureEpisodeId,
              error: safeExceptionMessage(error),
              at: new Date().toISOString(),
            });
            logOrganizationException(context, "message-bounce", error, { recipientPersonId: sender });
          }
        }
      }
      // FIRST OCCURRENCE, not the third that never comes. Separate from the
      // consecutive-failure escalation below in both its trigger and its
      // words: that one says "check provider access and model health", which is
      // the wrong instruction here — the provider is healthy and refused the
      // content.
      if (contentRefusal && !contentFilterEscalated) {
        contentFilterEscalated = true;
        try {
          const manifest = await loadIntercomOrganization(context);
          const recipient = directManagerId(manifest, currentPerson(context, manifest));
          const at = new Date().toISOString();
          appendOrganizationEvent(context, {
            event: "provider-failure-escalated",
            personId: context.personId,
            kind: providerFailure.kind,
            episodeId: providerFailureEpisodeId,
            consecutiveFailures: consecutiveProviderFailures,
            recipientPersonId: recipient,
            automaticRetry: false,
            modelChanged: false,
            sessionReplaced: false,
            at,
          });
          if (recipient) {
            const identity = createHash("sha256").update(JSON.stringify({
              organization: context.organization,
              personId: context.personId,
              kind: "content_filter",
            })).digest("hex").slice(0, 24);
            await sendOrganizationMessage(context, {
              to: recipient,
              body: `Content refusal for @${displayHandle(context.organization, context.personId)}: the provider declined a turn on what it contained (content_filter). The provider is healthy — this is not an outage and no access or model-health check will find anything. The turn was not replayed, no session or model was changed, and any message that turn had already receipted was returned to its sender unread. It will keep happening for the same material: re-scope what this person is asked to work on, or move them to a model whose filter does not fire on it. Reported once; the durable trail is provider-turn-failed in the company bus.`,
            }, { id: `content-filter-${identity}`, now: at });
          } else {
            try {
              latestExtensionContext?.ui.notify(
                "The provider refused a turn on content; no turn was replayed and no session or model was changed.",
                "error",
              );
            } catch { /* Durable escalation event remains authoritative. */ }
          }
        } catch (error) {
          appendOrganizationEvent(context, {
            event: "provider-failure-escalation-deferred",
            personId: context.personId,
            kind: providerFailure.kind,
            episodeId: providerFailureEpisodeId,
            consecutiveFailures: consecutiveProviderFailures,
            automaticRetry: false,
            error: safeExceptionMessage(error),
            at: new Date().toISOString(),
          });
          logOrganizationException(context, "provider-failure-escalation", error, { kind: providerFailure.kind });
        }
      }
      if (consecutiveProviderFailures >= ORGANIZATION_PROVIDER_FAILURE_ESCALATION_LIMIT && !providerFailureEscalated) {
        providerFailureEscalated = true;
        try {
          const manifest = await loadIntercomOrganization(context);
          const failedPerson = currentPerson(context, manifest);
          const recipient = directManagerId(manifest, failedPerson);
          const at = new Date().toISOString();
          appendOrganizationEvent(context, {
            event: "provider-failure-escalated",
            personId: context.personId,
            kind: providerFailure.kind,
            episodeId: providerFailureEpisodeId,
            consecutiveFailures: consecutiveProviderFailures,
            recipientPersonId: recipient,
            automaticRetry: false,
            modelChanged: false,
            sessionReplaced: false,
            at,
          });
          if (recipient) {
            const identity = createHash("sha256").update(JSON.stringify({
              organization: context.organization,
              personId: context.personId,
              episodeId: providerFailureEpisodeId,
            })).digest("hex").slice(0, 24);
            await sendOrganizationMessage(context, {
              to: recipient,
              body: `Provider reliability alert for @${displayHandle(context.organization, context.personId)}: ${consecutiveProviderFailures} consecutive turns ended before completion (last: ${providerFailure.kind}). No turn was replayed and no session was changed. The route is the operator's own Pi, which this company does not choose or record — check that Pi's provider access and model health, then explicitly choose the next action.`,
            }, { id: `provider-health-${identity}`, now: at });
            // No wake call. The recipient here is this person's DIRECT MANAGER,
            // so the same upward refusal applied: a company-wide runtime launch
            // is the head of the root department's to make, and a person never
            // manages the manager they are escalating to. The delivery is the
            // wake — `/v1/org/mailbox/delta` nudges chiefd's reconcile duty,
            // which reads the pending row and starts exactly its recipient.
          } else {
            try {
              latestExtensionContext?.ui.notify(
                `${consecutiveProviderFailures} consecutive provider failures; no automatic replay or session change was attempted.`,
                "error",
              );
            } catch { /* Durable escalation event remains authoritative. */ }
          }
        } catch (error) {
          // Provider diagnostics run at Pi's terminal agent_end boundary. Broken
          // authority or mailbox storage must not turn a provider outage into an
          // extension crash. Leave one compact durable diagnostic for the health
          // monitor and surface the same bounded incident locally when possible.
          appendOrganizationEvent(context, {
            event: "provider-failure-escalation-deferred",
            personId: context.personId,
            kind: providerFailure.kind,
            episodeId: providerFailureEpisodeId,
            consecutiveFailures: consecutiveProviderFailures,
            automaticRetry: false,
            error: safeExceptionMessage(error),
            at: new Date().toISOString(),
          });
          logOrganizationException(context, "provider-failure-escalation", error, {
            kind: providerFailure.kind,
            consecutiveFailures: consecutiveProviderFailures,
          });
          try {
            latestExtensionContext?.ui.notify(
              `${consecutiveProviderFailures} consecutive provider failures could not be routed to management. Check the organization exception log; no turn was replayed and no model or session was changed.`,
              "error",
            );
          } catch { /* The compact exception log remains the fallback authority. */ }
        }
      }
    } else {
      consecutiveProviderFailures = 0;
      providerFailureEscalated = false;
      providerFailureEpisodeId = undefined;
      // `contentFilterEscalated` is deliberately NOT cleared here — see its
      // declaration. The card below is, because a card is for the reader at
      // this pane and a refusal after a healthy turn is fresh information.
      contentFilterCardShown = false;
      insufficientCreditsCardShown = false;
      // A TURN THAT COMPLETED IS NOT NECESSARILY A TURN THAT WORKED.
      //
      // This is the third member of the looks-finished-but-isn't family, after
      // the filtered turn and the busy-but-silent compaction: the model PRINTS
      // its tool call as text, nothing executes, and every signal says the turn
      // completed — because by every signal it did. Measured: 215 occurrences
      // across 13 people, 61% within six transcript rows of a resume notice.
      const printed = printedToolCall(event);
      if (printed) {
        // RECORDED ON EVERY DETECTION, even when the corrective is suppressed.
        // The `postResume` correlation is the diagnostic that made this findable
        // and it stays queryable whether or not anything was sent.
        appendOrganizationEvent(context, {
          event: "tool-call-printed-as-text",
          personId: context.personId,
          toolName: printed.toolName,
          postResume: sawResumeNoticeRecently,
          corrected: printedToolCallCorrectives < PRINTED_TOOL_CALL_CORRECTIVE_LIMIT,
          at: new Date().toISOString(),
        });
        if (printedToolCallCorrectives < PRINTED_TOOL_CALL_CORRECTIVE_LIMIT) {
          printedToolCallCorrectives += 1;
          // NOT A REPLAY, and the distinction is the whole of #751/P4's
          // reconciliation. That tombstone forbids re-running a turn because a
          // replayed turn can duplicate an assignment, a message or a trade.
          // Nothing executed here: the printed call is TEXT. So this is new
          // input about an observable defect in the transcript, not a second
          // attempt at work that may already have happened.
          try {
            // `followUp` rather than `steer`, and no `triggerTurn`: this
            // arrives at a turn that has just ENDED, so the next turn carries
            // it. `sendUserMessage` takes the narrower option shape than
            // `queuedPiDelivery` builds, and the same call the input-requeue
            // rescue makes is the right one here.
            pi.sendUserMessage(
              `Your last reply printed a tool call as TEXT (<invoke name="${printed.toolName}">…), so nothing ran. `
                + "Re-issue it as a real tool call. Do not repeat the surrounding prose.",
              { deliverAs: "followUp" },
            );
          } catch (error) {
            logOrganizationException(context, "printed-tool-call-corrective", error, {
              toolName: printed.toolName,
            });
          }
        } else if (!printedToolCallCardShown) {
          // PAST THE CAP THE MODEL IS THE PROBLEM, not the turn. A fourth
          // corrective would be a loop against a model that has already ignored
          // three, so the operator gets the card instead.
          printedToolCallCardShown = true;
          logOrganizationException(context, "tool-call-printed-as-text", "the model printed tool calls as text repeatedly", {
            toolName: printed.toolName,
            corrections: printedToolCallCorrectives,
          });
          showPaneFailureCard(pi, {
            personId: context.personId,
            logPath: join(organizationLogsDirectory(context), "exceptions.jsonl"),
            printedToolCalls: printedToolCallCorrectives,
          });
        }
      } else {
        // A CLEAN TURN RE-ARMS THE CARD, like every other card here: it proves
        // the pane left the state the card described.
        printedToolCallCardShown = false;
      }
      // A turn that COMPLETED is the evidence that re-arms both failure cards.
      // It proves the pane left the state the card described — credentials now
      // exist, or the request now fits — so the next such failure is genuinely
      // new information and must be said again rather than swallowed by a guard
      // that was spent hours ago.
      providerConfigurationCardShown = false;
      requestTooLargeCardShownForLimit = undefined;
    }
    // ONE LIST PER TURN. Cleared on EVERY path — failed, bounced or completed —
    // so a later failure can never bounce a message an earlier, healthy turn
    // actually read. Last statement in the handler on purpose: everything above
    // that needs the list has already read it.
    deliveriesConsumedThisTurn = [];
    sawResumeNoticeRecently = false;
    // TOMBSTONE (#751/P4): an interrupted-reflection recovery ran here. It
    // inspected the ended turn for a half-emitted `org_reflect` call and
    // re-requested the bounded handoff. Both the tool and the handoff are
    // deleted, so a provider-interrupted turn is now only a provider
    // diagnostic.
  });
  // THE COMPACTION IS WORK, AND IT IS SILENT WORK.
  //
  // A compaction emits no turn events while it runs, so `noteTurnProgress`
  // never fires and the settle countdown keeps counting through it. Measured
  // consequence on a live box: a person mid-compaction at ~90% of a 1M
  // context had their window reaped, destroying the compaction after it had
  // paid for a summarize call over 909k tokens — and every wake since
  // re-triggers the same compaction into the same countdown.
  //
  // This hook exists in every Pi on the box (0.80.3, 0.80.10 and 0.84.3, all
  // verified) and it covers BOTH producers — Pi's own auto-compact and a
  // chief-initiated one. If a FUTURE Pi drops it, `registerCompactionBeat`
  // below leaves the feature inert and says so once, rather than failing a
  // launch: a missing hook must never be the reason a pane cannot boot.
  const registerCompactionBeat = () => {
    const on = (pi as unknown as { on?: (name: string, handler: () => void) => void }).on;
    if (typeof on !== "function") return;
    try {
      on.call(pi, "session_before_compact", () => { beginBusyWork("compaction"); });
    } catch (error) {
      appendOrganizationEvent(context, {
        event: "compaction-beat-unavailable",
        personId: context.personId,
        hook: "session_before_compact",
        error: safeExceptionMessage(error),
        at: new Date().toISOString(),
      });
    }
  };
  registerCompactionBeat();
  pi.on("session_compact", async (event, compactContext) => {
    // The compaction ENDED — beat the truth. `endBusyWork` reports idle only
    // when no turn is in flight, because a compaction routinely completes into
    // a queued prompt that starts the instant the branch shrinks.
    endBusyWork();
    const request = (await projectSessionMaintenanceForRuntime(context)).running.find((candidate) => candidate.action === "compact"
      && candidate.id === nativeCompaction.requestId);
    // `event.fromExtension` is NOT consulted, and neither is the entry's
    // `fromHook`. They are one predicate under two names — "a
    // `session_before_compact` handler supplied the summary" — and nothing
    // registers that hook, so gating the receipt on either made a completed
    // compaction unreceiptable. The identity of this entry is the request we
    // have in flight plus the session and the anchor it recorded; see
    // `isAnchoredNativeCompactionEntry`.
    if (!request) return;
    const entry = event.compactionEntry as { type?: unknown; id?: unknown; parentId?: unknown };
    const expectedParent = request.compactAnchorEntryId === EMPTY_SESSION_COMPACTION_ANCHOR
      ? null
      : request.compactAnchorEntryId;
    // A running compact request that never persisted its anchor has no witness
    // to be receipted against, so it refuses here rather than reaching the
    // predicate — the same refusal the entry-shape check has always produced.
    if (sessionManagerOf(compactContext)?.getSessionId?.() !== request.compactSessionId
      || expectedParent === undefined
      || !isAnchoredNativeCompactionEntry(entry, expectedParent)) {
      const failed = await sessionMaintenanceCommand(context, "finish", {
        requestId: request.id,
        status: "failed",
        error: "Pi emitted a compaction entry that did not match the durable native session anchor; refusing to compact twice.",
      }) as SessionMaintenanceRequest;
      showMaintenanceCard(pi, failed, "failed");
      return;
    }
    nativeCompaction.completedEntryId = entry.id;
    const completed = await sessionMaintenanceCommand(context, "finish", {
      requestId: request.id,
      status: "completed",
      compactEntryId: entry.id,
    }) as SessionMaintenanceRequest;
    showMaintenanceCard(pi, completed, "completed");
  });
  pi.on("agent_settled", guardStaleLifecycle("agent-settled-stale-context", async (_event: unknown, settledContext: any) => {
    latestExtensionContext = settledContext as ExtensionContext;
    turnInFlight = false;
    piTurnInFlight = false;
    disarmTurnWatchdog();
    // THE transition to idle -- and the only place the settle countdown is
    // allowed to start. Everything above cancels it; this releases it.
    noteAgentActivityBeat(false);
    // A settled lifecycle boundary is also authoritative startup readiness.
    // Some Pi hosts install an extension into an already-running session and
    // therefore do not replay session_start, but they do emit agent_settled.
    // Allow the bounded passive retry loop from this proven-safe boundary.
    sessionMaintenanceStartupReady = true;
    // Snapshot before the first await. Pi 0.80.10 already reports idle here,
    // so every later start event must invalidate this exact settled epoch.
    const settledLease = sessionMaintenanceLifecycleFence.settled(latestExtensionContext);
    if (idleResumeTimer) clearTimeout(idleResumeTimer);
    idleResumeTimer = undefined;
    if (idleResumeMaintenanceFallbackTimer) clearTimeout(idleResumeMaintenanceFallbackTimer);
    idleResumeMaintenanceFallbackTimer = undefined;
    idleResumeMaintenanceWaitEpoch = undefined;
    // Pi has either consumed its accepted follow-up queue or rejected a hidden
    // fire-and-forget request. Release unresolved leases only at this explicit
    // lifecycle boundary; the durable files remain the retry authority.
    mailboxDeliveryAttempts.clear();
    const companyMaintenancePending = await companyMaintenanceBlocked();
    let delivered = companyMaintenancePending ? 0 : await drain();
    // TOMBSTONE (#751/P4): a `lifecycleFollowUpPending` short-circuit stood
    // here. It let the settle that ENDED a bounded-handoff turn fall through
    // without scheduling ordinary background review. Nothing creates that
    // synthetic follow-up turn now.
    // Before a settled person is parked, compact a large context once. The
    // durable request keeps this idempotent across extension/process restarts.
    // A stale lease skips only branch-mutating maintenance; ordinary durable
    // recovery behavior keeps its existing lifecycle semantics.
    const canMaintain = sessionMaintenanceLifecycleFence.isCurrent(settledLease, latestExtensionContext);
    // NOTHING IS RETURNED FROM THIS HANDLER ANY MORE. It used to answer with a
    // `newSession` so Pi would replace the session at the settle boundary —
    // `AgentSettledEventResult`, which is chief's own patch and which upstream
    // types away as `undefined`. Operator ruling: *"yeah kill it"*. The
    // `agent_settled` SUBSCRIPTION stays; upstream fires it, and the beats, the
    // compact-at-settle and the parks all hang off it.
    const maintenanceStarted = canMaintain ? await processMaintenance(settledLease) : false;
    if (maintenanceStarted) {
      workResumePending = false;
      return;
    }
    // Completing this person's target never releases the fleet. Until the
    // final immutable target is terminal, do not reconcile activity, drain
    // mail, or resume work.
    if (await companyMaintenanceBlocked()) {
      workResumePending = true;
      return;
    }
    const automaticCompaction = canMaintain
      ? await queueAutomaticParkCompaction(
        context,
        latestExtensionContext,
        sessionMaintenanceLifecycleFence,
        settledLease,
      )
      : false;
    if (automaticCompaction) {
      workResumePending = false;
      return;
    }
    if (companyMaintenancePending) delivered = await drain();
    // TOMBSTONE (#751/P4): `reconcileActivity` ran here and, when it had asked
    // the pane for a bounded handoff, took priority over the work-resume
    // prompt below. Nothing asks for a handoff any more, so a settled turn
    // goes straight from mail to work resume.
    if (delivered) workResumePending = false;
    else await requestWorkResume();
  }));
  pi.on("session_shutdown", async () => {
    sessionMaintenanceShuttingDown = true;
    sessionMaintenanceStartupReady = false;
    sessionContextEpoch += 1;
    sessionMaintenanceLifecycleFence.invalidate();
    if (idleResumeTimer) clearTimeout(idleResumeTimer);
    // Leave no timer behind that could open a gate for a session that is gone.
    if (firstRunFallbackTimer) clearTimeout(firstRunFallbackTimer);
    firstRunFallbackTimer = undefined;
    if (idleResumeMaintenanceFallbackTimer) clearTimeout(idleResumeMaintenanceFallbackTimer);
    idleResumeMaintenanceFallbackTimer = undefined;
    idleResumeMaintenanceWaitEpoch = undefined;
    // #827: no floor timer to clear — kill the SSE reader so no late-arriving
    // doc-change can start a new cycle after shutdown begins.
    sseWatcher?.close();
    disarmTurnWatchdog();
    turnInFlight = false;
    piTurnInFlight = false;
    // No later tick can begin after the flag/clear above. Await the one bounded
    // launcher operation already owned by either a passive poll or settled
    // lifecycle so shutdown never leaves a detached promise to mutate after
    // the extension is gone.
    if (sessionMaintenancePollInFlight) await sessionMaintenancePollInFlight.catch(() => undefined);
    if (sessionMaintenanceInFlight) await sessionMaintenanceInFlight.catch(() => undefined);
    await Promise.all([...idleResumeInFlight].map((operation) => operation.catch(() => undefined)));
    await Promise.all([...postCompactionResumeInFlight].map((operation) => operation.catch(() => undefined)));
    if (mailboxDrainInFlight) await mailboxDrainInFlight.promise.catch(() => undefined);
  });
  // The slot covers request through first response byte: that is where an
  // upstream feels concurrent load, while streaming decode is already-admitted
  // work. These are registered last so they never displace an existing
  // handler's position, and every one of them is a belt-and-braces release for
  // the case where the response hook does not fire at all.
  pi.on("turn_end", async () => { turnInFlight = false; piTurnInFlight = false; disarmTurnWatchdog(); });
}

export default async function organizationIntercom(pi: ExtensionAPI) {
  await installOrganizationIntercom(pi);
}
