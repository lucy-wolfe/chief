import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@test/support/Nullish'
import { withoutComments } from '@test/support/TypeScriptSource'
import ts from 'typescript'
import { describe, expect, test } from 'vitest'

// #751/G9 Step 1 — name the seam mechanically, before moving anything.
//
// `organization-intercom.ts` is four things fused into one file
//, and the port order falls out of
// which is which. This test is that classification, checked in as data:
//
//   A — Harness adapter. The Pi extension contract: `installOrganizationIntercom`,
//       the hooks and schedulers Pi calls, the tool-name sets it registers, the
//       runtime context it reads about ITSELF, and the decoded shapes it hands
//       between Pi and chiefd. STAYS TypeScript permanently — a Rust process is
//       not inside Pi's event loop, and the type of a payload belongs with the
//       code that decodes it, not with the authority that owns its content.
//   B — Presentation. Cards, accents, mention colorization, roster formatting.
//       STAYS TypeScript: terminal layout for one TUI, governed by
//       `docs/cards-style.md`.
//   C — Business decisions. Refusal policy, retry policy, queue admission,
//       validation, and state reconstruction. MOVES to Rust (Mandate 3). This
//       is the count the port is measured against: it may only go down.
//   D — The second transport. `spawn`ing the launcher CLI as a subprocess for
//       verbs chiefd already owns. DELETED, not ported (audit §4, violation 2:
//       `piing -> apps/cli` inverts the layering law). BUCKET D IS EMPTY AND
//       MUST STAY EMPTY: the deletion landed, so the ceiling is 0 and there is
//       nothing left in this bucket to shrink. A row appearing here again is
//       the transport growing back, not a residue being counted.
//
// The buckets are a judgement, and recording a judgement as data is the point:
// it makes "C went from N to N-12" a fact a later packet can check rather than
// a claim it can make. An export missing a row fails this test, which is what
// stops the file quietly re-growing a business decision after the port.

const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url))
const SOURCE_PATH = `${PACKAGE_ROOT}extensions/organization-intercom.ts`
const SOURCE = readFileSync(SOURCE_PATH, 'utf8')
/** Prose mentions a "docstore fetch (…)" and quotes route paths; the
 * quarantine counts below are about code. */
const SOURCE_CODE = withoutComments(SOURCE)

type Bucket = 'A' | 'B' | 'C' | 'D'

/** Every top-level exported name, with the bucket it belongs to TODAY. */
const CLASSIFICATION: Readonly<Record<string, Bucket>> = {
  // ---- A: harness adapter (Pi extension contract; stays TypeScript) --------
  // Pi's own delivery-option contract, and the CONDITION under which a turn may
  // be requested at all -- harness wiring by definition. Exported solely so its
  // test can drive busy and idle separately; the rule it encodes (never ask a
  // pane mid-run to start a turn) is Pi's API shape, not a business decision.
  queuedPiDeliveryForTest: 'A',
  // #1208. All three are the SAME KIND of thing as the row above: they encode
  // Pi's own contract, not a decision about how a company works. `prompt()`
  // throws on a bare call while a run is active -- that is an API shape -- so
  // the rule that re-queues such a call, the shape of the line that records the
  // rescue, and the seam a test drives the boot gate through are harness
  // adapters by definition. None of them owes a move to Rust: there is nothing
  // here for a daemon to decide.
  inputInterceptionDecision: 'A',
  inputRequeueLogDetail: 'A',
  firstRunGateForTest: 'A',
  QueuedPiDeliveryOptions: 'A',
  // Pi's own custom-entry type name for an intercom message. It is the string
  // Pi hands back on `message_start`, so it is the harness contract by
  // definition -- exported so a test harness drives the delivery path with the
  // PRODUCT's constant instead of re-spelling the literal, which is a test of
  // its own spelling. Nothing here is a decision about how a company works.
  MESSAGE_TYPE: 'A',
  // Reads an ended turn's own message content for a defect in the TRANSCRIPT --
  // the same boundary and the same kind of judgement as
  // `providerFailureDiagnostic` beside it, which is bucket A for the same
  // reason: it classifies what Pi handed back, and owes nothing to a daemon.
  printedToolCall: 'A',
  ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS: 'A',
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES: 'A',
  ORGANIZATION_MANAGER_TOOL_NAMES: 'A',
  // Same bucket as the list it was split out of, and for the same reason: a
  // catalog of tool NAMES is Pi-harness wiring, not a business decision. The
  // decision it expresses -- who may grow a subtree -- lives in the authority
  // layer and in `org_ops`, both of which already answer it in Rust.
  ORGANIZATION_SUBTREE_TOOL_NAMES: 'A',
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES: 'A',
  ORGANIZATION_BASELINE_TOOL_NAMES: 'A',
  ORGANIZATION_HEALTH_NOTICE_KINDS: 'A',
  IntercomOrganizationManifest: 'A',
  // #1046. The same bucket as the manifest it is a field of, and for the same
  // reason: a person record is a DECODED SHAPE handed between Pi and chiefd. It
  // decides nothing. Exported so a scope predicate that takes a person can be
  // unit-tested without a daemon.
  PersonRecord: 'A',
  OrganizationRuntimeContext: 'A',
  OrganizationEnvelope: 'A',
  LauncherSystemNoticePresentation: 'A',
  SseWatcherLike: 'A',
  InstallOrganizationIntercomOptions: 'A',
  OrganizationIntercomInterval: 'A',
  OrganizationIntercomScheduler: 'A',
  ORGANIZATION_PARKED_MAINTENANCE_RECONCILE_INTERVAL_MS: 'A',
  // A, with the same reasoning as the watchdog bounds beside it: the DECISION
  // lives in chiefd (`MIN_REMINDER_INTERVAL_MS`,
  // `MIN_RECURRING_REMINDER_INTERVAL_MS`, which refuse server-side); these two
  // exist so the tool SCHEMA an agent reads does not advertise a cadence the
  // daemon will reject. They are harness-facing mirrors, pinned against the
  // Rust source by `ReminderFloorParity.test.ts` so they cannot drift into a
  // second opinion — which is what would make them C.
  MIN_REMINDER_INTERVAL_MS: 'A',
  // A: Pi-lifecycle plumbing — whether a prompt parked in the boot window still
  // needs driving. Exported because the RESCUE calls it and a test pins it, not
  // as a test seam; the decision is about Pi's queues, not a business rule that
  // owes a move to Rust.
  workResumeNeedsRedrive: 'A',
  MIN_RECURRING_REMINDER_INTERVAL_MS: 'A',
  ORGANIZATION_SSE_MAINTENANCE_STORES: 'A',
  ORGANIZATION_TURN_WATCHDOG_MS: 'A',
  ORGANIZATION_TURN_WATCHDOG_INTERVAL_MS: 'A',
  ORGANIZATION_TURN_WATCHDOG_ESCALATION_MS: 'A',
  ORGANIZATION_IDLE_RESUME_READY_ATTEMPTS: 'A',
  ORGANIZATION_IDLE_RESUME_MAINTENANCE_FALLBACK_MS: 'A',
  authoritativeRuntimePane: 'A',
  readOrganizationRuntimeContext: 'A',
  // #983. A, for the same reason its sibling above is A: this is the context
  // the extension reads about ITSELF. It decides nothing about the company —
  // it asks beacond, the authority that already owns the answer, which daemon
  // owns this install's slug. Not C: there is no policy here to owe Rust, and
  // Rust already resolves the same way through the same registry.
  resolveOrganizationRuntimeContext: 'A',
  loadIntercomOrganization: 'A',
  resolveInstallerStructuralRoot: 'A',
  resetAppendOrganizationEventFailureLoggedOnceForTests: 'A',
  appendOrganizationEventOnce: 'A',
  OrganizationRosterPersonObservation: 'A',
  OrganizationRosterObservation: 'A',
  OrgChiefdUrlUnsetError: 'A',
  // The company daemon this install talks to, plus the person whose key signs
  // for it. Bucket A: it decides nothing — it is the address and identity the
  // harness resolved for THIS install, threaded rather than re-read from the
  // process, which is precisely "the runtime context it reads about ITSELF".
  ChiefdEndpoint: 'A',
  resetConditionalReadCacheForTest: 'A',
  readDurableDocumentCached: 'A',
  // Bucket A, both of them, and for the same reason `ChiefdEndpoint` is:
  // neither decides anything. `organizationSseBearer` turns the address and
  // person the harness already resolved for THIS install into the credential
  // its own reader presents — the acquirer itself lives in `@chief/chiefing`
  // and is shared with `team-ui`, so nothing here is a second implementation
  // of anything. Not C: it holds no policy and refuses nothing; chiefd decides
  // what a bearer is worth.
  organizationSseBearer: 'A',
  // `reportPaneKeyRefusal` is the pane's REPORTING path for a key its own
  // reader refused. The refusal RULE is not made here — `readAgentKeypair`
  // applies it, and the daemon applies the identical rule to its own operator
  // key — this only writes what happened to the two trails every other in-pane
  // failure already uses. A diagnostic about the pane's own filesystem is
  // exactly the harness's business: no other process can see that file at the
  // moment the pane needs to know.
  //
  // A HOLDS ONLY WHILE IT STAYS A DIAGNOSTIC. It reports; nothing reads it
  // back. The day any caller BRANCHES on its outcome — retries, refuses, picks
  // a different credential — it has stopped describing and started deciding,
  // and the row is C, owed to Rust. Catching exactly that day is what this
  // table is for, so the condition is written down rather than left to whoever
  // makes the change to notice.
  reportPaneKeyRefusal: 'A',
  loadOrganizationRosterObservation: 'A',
  ExtensionStaleness: 'A',
  extensionStalenessOf: 'A',
  runningExtensionStaleness: 'A',
  assertRunningExtensionIsCurrent: 'A',
  providerFailureDiagnostic: 'A',
  workResumePrompt: 'A',
  // Bucket A, and the bucket's own rule is the argument: this reads Pi's LIVE
  // `sessionManager` — the session id and the in-memory entry list — to decide
  // whether the compaction a durable request asked for is present in the
  // transcript. "A Rust process is not inside Pi's event loop", so no other
  // process can answer it. Not C: it holds no policy, retries nothing, and
  // admits nothing to a queue; it reports one fact about Pi's own transcript
  // that three receipt sites then map straight through. Exported so the fact
  // can be unit-tested without a Pi session.
  nativeCompactionProof: 'A',
  // Whether this phase of this durable maintenance record still has to be
  // announced in the pane. Bucket B: it decides nothing about the record and
  // nothing about policy — the store already owns whether a finish applies or
  // replays — it decides whether the PERSON READING THE PANE has been told
  // yet, which is presentation, and it is exported so the rule can be tested
  // without a Pi session.
  shouldAnnounceMaintenanceCard: 'B',
  // What a failed compaction SAYS when the session could not be summarized.
  // Bucket B: it composes an operator-facing sentence from a provider error and
  // decides nothing — it does not retry, does not queue the replacement it
  // names, and does not change any record's status.
  compactionFailureReason: 'B',
  installOrganizationIntercom: 'A',
  'default:organizationIntercom': 'A',
  // #751/P1: the department a manager describes, as chiefd's
  // `/v1/org/department/create` accepts it. Bucket A, not C: it decides
  // nothing — every id, title and default it cannot know is left EMPTY for
  // chiefd to mint (`mint_department_create_ids`), and what remains is exactly
  // "the decoded shapes it hands between Pi and chiefd", which this bucket
  // says stays TypeScript permanently. It exists as a named export so the
  // mapping can be unit-tested without a daemon.
  IntercomDepartmentSpec: 'A',
  ChiefdDepartmentPersonSeed: 'A',
  ChiefdCreateUnit: 'A',
  ChiefdDepartmentHead: 'A',
  // What becomes of the department an appointee already heads. A for the same
  // reason as the head decision it accompanies: it is a DECODED SHAPE handed
  // between Pi and chiefd and decides nothing. WHICH of its two answers applies
  // — hand over to a named member, or dissolve an emptied unit — is chiefd's
  // refusal to make, and deliberately was not given a pre-flight twin here.
  ChiefdHeadVacancy: 'A',
  ChiefdDepartmentCreateRequest: 'A',
  departmentCreateRequest: 'A',
  // The vacancy payload's SHAPE check, shared by the two verbs that can vacate
  // a headship. A for the same reason `departmentCreateRequest` is, and
  // exported for the same reason: it decides nothing — whether a hand-over
  // names a real member is chiefd's answer — and a named export is how the
  // mapping is unit-testable without a daemon.
  normalizeHeadVacancy: 'A',
  // #1093: repairs a model's double-encoded argument at the `prepareArguments`
  // seam Pi calls BEFORE TypeBox validation. Bucket A and nothing else — it is
  // pure Pi-harness plumbing that decides no product question: it parses a
  // string only where the tool's own declared schema already says "object", and
  // hands the SAME arguments onward for the SAME validator to judge. There is
  // nothing here chiefd could own, because chiefd never sees a tool call.
  // Exported so the repair is unit-testable without a daemon or a provider.
  unwrapStringifiedArguments: 'A',
  // #751/P3: the person an `org_hire` call describes, as
  // `/v1/org/person/hire` accepts it. Bucket A for the same reason
  // `departmentCreateRequest` is: it decides nothing. The id, title and task
  // class it cannot know are sent EMPTY for chiefd to mint (`mint_hire_ids`),
  // and what remains is the decoded shape handed between Pi and chiefd. Named
  // exports so the mapping is unit-testable without a daemon.
  ChiefdHireRequest: 'A',
  hireRequest: 'A',
  // #751/P4: chiefd's `RuntimeLaunchReport` as this file decodes it. Bucket A
  // for the same reason the two above are — it is a wire shape, not a decision,
  // and only the fields a card reads are modelled.
  ChiefdRuntimeLaunchReport: 'A',

  // ---- B: presentation (stays TypeScript) ---------------------------------
  // THE DELIVERED ENVELOPE'S GUIDANCE, and the four rows below are one
  // decision, so they are justified once.
  //
  // B, not C, and the line is the same one `runtimeConvergenceWarning` draws.
  // Nothing here decides anything about the organization. Who is a manager is
  // already decided, in Rust, in two places that this only READS —
  // `PersonKind` on the person record, and `resource_catalog::is_manager` /
  // `RoleSkill::of`, which are what actually install a person's skill. This
  // turns that settled fact into the right words for the reader, which is
  // exactly bucket B.
  //
  // It is here at all because the copy has to be beside the cursor. The
  // manager-does-the-work regression survived every fix written into a
  // document read once at boot; the string in front of the model at the moment
  // work ARRIVES is composed here, on the delivery path, and nowhere else. A
  // Rust process is not inside Pi's event loop and cannot compose it.
  //
  // `recipientRole`'s one judgement is a wording policy, not a business one:
  // when the manifest cannot be read it answers `unknown` and the guidance
  // then CLAIMS no role, rather than defaulting to a role and telling a manager
  // it is a worker. Deciding what to say when you do not know is bucket B's
  // own question.
  RecipientRole: 'B',
  recipientRole: 'B',
  // Exported solely so both branches are unit-testable without booting a
  // company — the same reason `queuedPiDeliveryForTest` is exported, and the
  // reason a single-branch test is what let the byte-identical guidance ship.
  messageContextForTest: 'B',
  mailboxBatchContextForTest: 'B',
  // The USERNAME rules. `recipientsForTest` exposes the recipient resolver so
  // "a username resolves", "an id still resolves" and "an ambiguous username is
  // refused naming both" can be asserted as RULES rather than inferred from a
  // delivered message — which is exactly how the old behaviour survived, since
  // every surface agreed with every other surface and all of them showed the
  // key. `primeManifestForTest` seeds the display-time roster a live pane
  // already keeps, so the presentation half is testable without a company.
  // Both are B: they are about what a person is SHOWN and what an agent may
  // type, not about where a decision lives.
  recipientsForTest: 'B',
  // The refusal classification: which failures are the CALLER's and which are
  // the system's. Presentation, because the whole subject is what the card
  // tells a reader about whose fault it is.
  refusalResultForTest: 'B',
  isCallerRefusalCardForTest: 'B',
  showsSystemFaultTagForTest: 'B',
  callerRefusalForTest: 'B',
  messageWakeDispositionForTest: 'B',
  primeManifestForTest: 'B',
  renderOrganizationCard: 'B',
  isOperatorIdentity: 'B',
  organizationPersonAccents: 'B',
  organizationPersonAccentHex: 'B',
  colorizePersonMentions: 'B',
  organizationMessageSenderAccent: 'B',
  launcherSystemNoticePresentation: 'B',
  formatOrganizationRoster: 'B',
  // #751/P4. B and not C, and the line is worth drawing: this decides NOTHING
  // about the mutation or about convergence — chiefd has already decided both
  // and said so in its report. All this does is word that answer for a card,
  // which is exactly what bucket B is. It is exported so its truthful-or-absent
  // rule is unit-testable; a C row here would also breach a ceiling that is
  // already at its limit, and paying for a copy helper out of the
  // business-decision budget would be the wrong accounting.
  runtimeConvergenceWarning: 'B',
  // Bucket B for the same reason as `runtimeConvergenceWarning`: the DECISION
  // is `isTransientTransportFailure` (a C row already), and this only words its
  // answer. Exported so the rule it now keeps -- never report a recovery that
  // is not running -- is unit-testable without booting a company.
  transientDegradeMessage: 'B',
  // #1046. B for the same accounting as `runtimeConvergenceWarning` above:
  // `departmentScopeDenial` (C, below) makes the decision, and these two only
  // word its two outcomes for the caller. The wording is the product here — a
  // static remediation sentence sent a CEO into a create the core refuses — so
  // it is exported to be unit-testable without booting a company.
  unknownDepartmentMessage: 'B',
  hiringPathAdvice: 'B',
  // The roster's per-person authority field. B, and the accounting is the same
  // as `hiringPathAdvice` above: `departmentScopeDenial` (C),
  // `headedDepartmentId` and `authorityRootDepartmentId` make every decision
  // here, and these three only compose and word the answers for a roster line.
  // A C row would claim a second opinion about authority, which is exactly what
  // this packet exists to prevent. Exported so the agreement between the words
  // and the gate is unit-testable without booting a company.
  PersonAuthorityView: 'B',
  personAuthority: 'B',
  personAuthorityText: 'B',

  // ---- C: business decisions (move to Rust) -------------------------------
  ORGANIZATION_PROVIDER_FAILURE_ESCALATION_LIMIT: 'C',
  ORGANIZATION_MAILBOX_MAX_OUTSTANDING_DELIVERIES: 'C',
  ORGANIZATION_MAILBOX_BATCH_THRESHOLD: 'C',
  ORGANIZATION_MAILBOX_BATCH_MAX_ITEMS: 'C',
  ORGANIZATION_SESSION_MAINTENANCE_START_RETRY_DELAYS_MS: 'C',
  sendOrganizationMessage: 'C',
  isTransientTransportFailure: 'C',
  withTransientReadRetryAsync: 'C',
  assertDurableMaintenanceRecords: 'C',
  projectSessionMaintenanceForRuntime: 'C',
  executeAtomicPersonTransfer: 'C',
  drainOrganizationMailbox: 'C',
  archiveStartedOrganizationMailboxMessage: 'C',
  hasOpenOrganizationWork: 'C',
  ResumeRecoveryDecision: 'C',
  classifyResumeRecovery: 'C',
  driveResumeRecovery: 'C',
  // #1046. C, honestly: this is refusal policy, and chiefd holds the same rule
  // (`requester-out-of-scope`). It is a pre-flight copy that exists to answer a
  // caller before the round-trip, and it owes a move to Rust like every other C
  // row. It became an export because the defect it fixes was invisible from
  // outside: the predicate returned ONE boolean for two different answers, and
  // no test could tell "no such department" from "you lack authority" until the
  // two were named.
  departmentScopeDenial: 'C'
  // #1146's two rows — `resolveModelForRequest` and `unknownModelMessage` —
  // stood here and are DELETED with the functions they classified. Both were
  // reachable only from the tombstoned `set_model` verb and had zero
  // production callers. Chief does not choose models; Pi owns that. The
  // classification was right while the code existed, and a classification of
  // deleted code is not a record worth keeping.

  // ---- D: the second transport (deleted, not ported) ----------------------
  // EMPTY, and that is the finished state. The fourteen rows that stood here
  // — `LauncherCommandResult`, `LauncherRunner`, `CoordinatedLauncherRunner`,
  // `createLauncherCommandQueue`, `launcherControlPlaneEnvironment`,
  // `launcherWorkerActionEnvironment`, `launcherRuntimeBinary`,
  // `ATTESTED_WORKER_ACTION_VERBS`, `isAttestedWorkerActionCommand`,
  // `LAUNCHER_COMMAND_TIMEOUT_MS`, `defaultLauncherRunner`,
  // `LauncherCommandError`, `describeLauncherCommandFailure` and `runChecked`
  // — are deleted from the extension, not moved. `runChecked` was the only
  // caller of a `LauncherRunner`, so once the last verb reached chiefd over
  // HTTP the whole family became unreachable at once and left together.
  // `DELETED_TRANSPORT_NAMES` below is what keeps them gone.
}

/**
 * Bucket D's inventory, as a set of names that must not appear in the
 * extension's CODE again.
 *
 * The classification table can only see EXPORTS. That was enough while the
 * transport was live and every piece of it was exported, but a re-grown
 * transport could be entirely private and the table would never notice. So the
 * absence is asserted directly against the source instead, which is also what
 * lets the D ceiling be a real zero rather than a number nobody can reach.
 */
const DELETED_TRANSPORT_NAMES = [
  'LauncherCommandResult',
  'LauncherRunner',
  'CoordinatedLauncherRunner',
  'createLauncherCommandQueue',
  'launcherControlPlaneEnvironment',
  'launcherWorkerActionEnvironment',
  'launcherRuntimeBinary',
  'ATTESTED_WORKER_ACTION_VERBS',
  'isAttestedWorkerActionCommand',
  'LAUNCHER_COMMAND_TIMEOUT_MS',
  'LAUNCHER_COMMAND_OUTPUT_LIMIT',
  'defaultLauncherRunner',
  'LauncherCommandError',
  'describeLauncherCommandFailure',
  'waitForLauncherRetry',
  'runChecked'
] as const

/** Ceilings, not targets. Every one of these is a quantity the port drives
 * DOWN; a packet that raises one has re-grown what G9 exists to remove. A/B are
 * deliberately absent — those buckets may legitimately grow. */
// 56 -> 55 (2026-08-24): `/v1/org/activity/command-status` is deleted with the
// pending-park gate that was its only reader. The gate asked whether a park was
// already pending before spending a compact, and a routine idle park is born
// terminal — so it never appeared in `pendingTransitions` and the read could
// only answer no. A route literal whose reader could never act on the answer is
// exactly the kind of seam this ceiling is counting.
// 62 -> 63 (2026-08-11): `/v1/org/activity/agent-state`, the settle
// countdown's idleness beat. The pane is the only thing that knows whether its
// agent is mid-turn, so this is a route that could not have been folded into an
// existing one -- which is the question this ceiling exists to force, and the
// answer it will accept.
const CEILINGS = { C: 23, D: 0, subprocessCallSites: 0, routeLiterals: 55 } as const
// THE SUBPROCESS IS AT ZERO. The last four families took
// `subprocessCallSites` 4 -> 0 and `routeLiterals` 49 -> 62. No verb in this
// file reaches chiefd by spawning anything any more; a Pi extension talks to
// the daemon it is already connected to, over the daemon's own API.
//
//   * ACTIVITY STATUS -> `/v1/org/activity/command-status`. Required
//     non-`Option` `callerPersonId`, whose chiefd doc-comment reads "the
//     person the trusted adapter authenticated. Never from a Pi payload" —
//     supplied here, spread last.
//   * MULTI-UNIT RESUME -> `/v1/org/department/resume-many`. The
//     `--socket`/`--session` this helper appended were not even accepted by
//     the CLI's own argument check for `department launch --units`, so the
//     verb had a second, older break underneath the missing file.
//   * SESSION MAINTENANCE, all eleven verbs -> ten routes (`queue` and
//     `auto-compact` share one, exactly as the CLI had it). Six of them take
//     `identity: {personId}` as a required non-`Option`
//     nested struct and no call site in this file carried it; `apply` and
//     `complete` take `personId` plus a generation; `start` takes a required
//     `action` the call site never sent; `auto-compact` requires
//     `personId`/`requestedBy` its only call site never sent. Every request
//     struct in the family is `deny_unknown_fields`, so the flat
//     `processId`/`sessionId`/`claimToken` this file speaks is reshaped into
//     the nested `claim`/`sourceClaim`/`targetClaim` chiefd models, in one
//     place rather than at thirty call sites.
//   * HIRE PREFLIGHT -> deleted outright (chief-home-is-cwd §3/§4e). It read a
//     `/v1/org/resource-catalog/read` route for the installed skill, extension
//     and package ids a hire could name. A hire names none — Pi loads the
//     company's `.pi/skills` through one symlink — so both the preflight and
//     the route are gone rather than reclassified.
//
// `reconcile-parked` is the one verb that changed ROUTE rather than transport:
// its only call site sends `{}`, and
// `/v1/org/session-maintenance/reconcile-parked` requires a caller-supplied
// `parkedPersonIds`, so it could never have worked. Its consumer reads
// `{actionId, requestId, personId}` off each skipped row, which is exactly
// `/v1/org/company-session-action/skip-parked` — the route that derives the
// parked set itself. The consumer named the right authority; the transport
// named the wrong one.
//
// BUCKET D WENT 14 -> 0, AND THAT WAS THE LAST PACKET IN THE SEAM.
// `subprocessCallSites` reached zero first: every verb moved onto a chiefd
// route, and the transport was left standing with nothing calling it. This is
// what "deleted, not ported" finally looked like — `runChecked` was the only
// function that ever invoked a `LauncherRunner`, so with its last call site
// gone the runner, the queue that serialized it, the two child environments,
// the attested-verb table, the bun resolution, the timeout/output bounds and
// the structured `LauncherCommandError` were all unreachable at once. About
// four hundred lines and fifteen `runner: LauncherRunner` parameters that no
// body read went with them; the extension's own tombstone records what each
// piece did. Nothing was ported and nothing was kept for later: the only
// survivors are `isTransientTransportFailure` and
// `TRANSIENT_TRANSPORT_RETRY_DELAYS_MS`, which are bucket C, and whose one
// remaining reader is the boot retry ladder `withTransientReadRetryAsync`.
// The model and thinking switches took `subprocessCallSites` 6 -> 4 and
// `routeLiterals` 47 -> 49. Two families, one packet, because they are twins:
// same shape, same two required identity fields, one dimension apart.
// The five mutating TASK verbs took `subprocessCallSites` 7 -> 6 and
// `routeLiterals` 42 -> 47. The acting identity (`creatorId`/`actorId`) is
// now supplied by the tool rather than derived by the CLI from the pane's
// launcher-injected identity, which is why the `--socket`/`--session` argv
// went with the subprocess.
// The durable-REMINDER family took `subprocessCallSites` 8 -> 7 and
// `routeLiterals` 39 -> 42 (`/v1/reminders/arm`, `/v1/reminders/list`,
// `/v1/reminders/stop`) — the expected trade: one subprocess becomes three
// named routes in a closed union. This was the URGENT one. Deleting the Pi
// `/loop` addon made reminders the ONLY recurrence mechanism, and all three
// reminder tools were answering `unknown command 'org'` because the CLI they
// spawned now serves only `founder-pi` — so the product had no working way to
// schedule anything at all.
// Deleting the `@koltmcbride/pi-loop` addon took `subprocessCallSites` 9 -> 8.
// Unlike the reflection deletion above, this one DOES move the count:
// `org_stop_loops` owned a whole `runChecked(context, runner, ["org", "loops",
// "stop", ...])` site of its own rather than sharing a helper with a surviving
// verb, so the tool and its transport left together. `routeLiterals` is
// unchanged: the tool never reached a chiefd route from this file.
// #751/P4 (delete "reflection") took `C` 28 -> 25. Three C rows went away with
// the concept, not to Rust: `SETTLED_REFLECTION_FOLLOW_UP_ENABLED` (the flag
// that had already parked the settled bounded-handoff follow-up),
// `pruneReflectionDeliveryStates` (the LRU over per-transition prompt-delivery
// state), and `reconcileSettledOrganizationActivity` (the settled-boundary
// prompt sender itself). A reflection was a bounded handoff document an agent
// wrote via `org_reflect` before park/bench/transfer/offboard; the product no
// longer has one, so the decisions these three encoded are not owed to Rust
// either -- they are simply gone. `subprocessCallSites` does NOT move:
// `org activity reflect` shared `activityCommand`'s single `runChecked` site
// with the surviving `org activity status`, so deleting the verb removed a
// branch, not a call site.
// #751/P4 packet 2 (the goals/assignments family) took `subprocessCallSites`
// 12 -> 9. That family is now DELETED outright with the goals feature, so its
// route literals are gone too; `routeLiterals` is a CEILING, so the fall is
// free.
//
// #751/P4 packet 1 took `subprocessCallSites` 14 -> 12 and `routeLiterals` 27 -> 29, the
// same one-literal-per-verb trade the note below describes, for
// `/v1/org/runtime/launch` and `/v1/org/lifecycle-status/read`.
//
// This packet is the one that says why a route proof is not a proof. P1, P2.1
// and P3 each moved their own verb onto a chiefd route, proved the route with a
// direct POST, and left `reconcileRuntime` — the helper EVERY mutating tool
// calls after its durable write — spawning `org reconcile` at the deleted CLI.
// A CEO's `org_launch_department` therefore committed the department and then
// failed with `chiefd: unknown command 'org'`. Three green packets, one broken
// operation, and no assertion anywhere between them: the tool does strictly
// more than the route, and only the tool is the product. `reconcileRuntime` had
// ONE call site and FIFTEEN callers, which is exactly why it survived three
// audits that counted call sites.
//
// Bucket D does NOT move here and that is honest: `reconcileRuntime` was never
// an export, so no D row dies with it. The count that moved is
// `subprocessCallSites`, which is the one that measures the transport.
//
// #751/P3 took `subprocessCallSites` 15 -> 14 and `routeLiterals` 16 -> 27.
// The eleven `runLifecycle` call sites — reparent, move-members, appoint-head,
// replace-head-and-offboard (x2), hire, bench, recall, start-person,
// stop-person, offboard — moved onto chiefd's own API, which deleted
// `runLifecycle` (the file's second `runChecked` site) outright. That is the
// trade this ceiling documents: eleven verbs left the subprocess and eleven
// route literals arrived with them, one per verb reached. `hire` is two
// literals for one verb (`hire-preview` then `hire`) because the route requires
// the preview's selection back as its `expected*` attestation, and `bench`/
// `offboard` name the LIFECYCLE spelling of their verb rather than the bare
// structural one — see `StaffingRoutePath`'s own comment for why the structural
// route is not a shortcut to it.
// `routeLiterals` 12 -> 13 (#751/P1, `/v1/org/department/create`) -> 16
// (#751/P2 packet 1: `pause`, `resume`, `remove-tree`), and this is the one
// ceiling here that is ALLOWED to rise. It was written to catch a call
// that skipped the typed client. The port inverts that for a moment: taking a
// verb OFF the launcher subprocess and onto chiefd's own API necessarily adds
// the route it now calls. A rise is only legitimate as part of that trade —
// one literal per verb family moved, and `subprocessCallSites` must fall to
// match by the time the family is done (department create is the first half of
// its family; resume/stop/remove still shell out, which is why D has not moved
// yet). A literal added WITHOUT a verb leaving the subprocess is the original
// defect and still fails here.
// `routeLiterals` is 12, not the plan's inventory figure of 11: the audit
// counted concrete `/v1/*` paths, and the twelfth is the templated
// `` `/v1/org/${storeName}/read` `` that the supervision/activity/
// session-maintenance aggregates share. It IS a route literal for quarantine
// purposes — a new store name added to it is a new route reached without a
// named chiefing method, which is exactly what the ceiling is watching for.

/** Top-level exported names, via the TypeScript parser rather than a regex —
 * a regex over 14k lines mistakes a nested `export` in a template literal or a
 * multi-line declaration for a top-level one, and the whole value of this fence
 * is that its inventory is exactly the real one. */
function exportedNames(): string[] {
  const sourceFile = ts.createSourceFile(
    SOURCE_PATH,
    SOURCE,
    ts.ScriptTarget.ES2022,
    /* setParentNodes */ true
  )
  const names: string[] = []
  for (const statement of sourceFile.statements) {
    if (ts.isExportDeclaration(statement)) {
      const clause = statement.exportClause
      if (clause && ts.isNamedExports(clause)) {
        for (const element of clause.elements) names.push(element.name.text)
      }
      continue
    }
    const modifiers = ts.canHaveModifiers(statement) ? (ts.getModifiers(statement) ?? []) : []
    if (!modifiers.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword)) continue
    const isDefault = modifiers.some((modifier) => modifier.kind === ts.SyntaxKind.DefaultKeyword)
    if (ts.isVariableStatement(statement)) {
      for (const declaration of statement.declarationList.declarations) {
        names.push(declaration.name.getText(sourceFile))
      }
      continue
    }
    if (
      ts.isFunctionDeclaration(statement) ||
      ts.isClassDeclaration(statement) ||
      ts.isInterfaceDeclaration(statement) ||
      ts.isTypeAliasDeclaration(statement)
    ) {
      const declared = statement.name?.text ?? ''
      names.push(isDefault ? `default:${declared}` : declared)
      continue
    }
    names.push(`UNRECOGNIZED:${ts.SyntaxKind[statement.kind]}`)
  }
  return names
}

function countIn(source: string, needle: string): number {
  return source.split(needle).length - 1
}

describe('IntercomSeamClassification (#751/G9-S1)', () => {
  const names = exportedNames()

  test('the parser found the real export surface, not a fragment of it', () => {
    expect(names.length).toBeGreaterThan(80)
    expect(names).toContain('installOrganizationIntercom')
    expect(names).toContain('default:organizationIntercom')
    expect(names.filter((name) => name.startsWith('UNRECOGNIZED:'))).toEqual([])
  })

  test('every top-level export is classified A/B/C/D', () => {
    const unclassified = names.filter((name) => isNullish(CLASSIFICATION[name]))
    expect(
      unclassified,
      'A new top-level export in organization-intercom.ts has no seam classification. ' +
        'Decide which of the four things it is and add ' +
        'a row: A = Pi harness adapter, B = presentation, C = business decision that owes a ' +
        'move to Rust, D = the second transport that is being deleted. If the honest answer ' +
        'is C or D, the quarantine says it should not be being added here at all.'
    ).toEqual([])
  })

  test('no classification row outlives the export it classifies', () => {
    const live = new Set(names)
    const stale = Object.keys(CLASSIFICATION).filter((name) => !live.has(name))
    expect(
      stale,
      'These names are classified but no longer exported. Delete the rows — and if a C or D ' +
        'row went away because the logic MOVED, lower the matching ceiling in CEILINGS in the ' +
        'same commit, so the next packet inherits the ground that was actually gained.'
    ).toEqual([])
  })

  test('the business-decision and second-transport buckets only ever shrink', () => {
    const counts: Record<Bucket, number> = { A: 0, B: 0, C: 0, D: 0 }
    for (const name of names) {
      const bucket = CLASSIFICATION[name]
      if (isNullish(bucket)) continue
      counts[bucket] += 1
    }
    // Reported together so a failure states the whole shape of the file, not
    // just the number that moved.
    expect(counts.A + counts.B + counts.C + counts.D).toBe(names.length)
    expect(counts.C, `bucket C (business decisions owed to Rust): ${counts.C}`).toBeLessThanOrEqual(
      CEILINGS.C
    )
    expect(counts.D, `bucket D (second transport, to delete): ${counts.D}`).toBeLessThanOrEqual(
      CEILINGS.D
    )
  })

  test('quarantine: the subprocess transport is GONE, declaration included', () => {
    // This used to subtract 1 for `runChecked`'s own declaration and count what
    // was left. There is no declaration to subtract any more: the count is of
    // every occurrence, and it is zero.
    expect(
      countIn(SOURCE_CODE, 'runChecked('),
      'A launcher-subprocess call site, or `runChecked` itself, is back. Every verb in this ' +
        "file reaches chiefd over chiefd's own API; a subprocess here is a second transport, " +
        'not a residue.'
    ).toBe(CEILINGS.subprocessCallSites)
    // The `spawn` they all funnelled through is gone with them, and so is the
    // import that made one reachable. `spawnSync` survives alone, for
    // `authoritativeRuntimePane`'s read-only tmux pane discovery.
    expect(SOURCE_CODE).not.toMatch(/\bspawn\s*\(/)
    expect(SOURCE).toContain('import { spawnSync } from "node:child_process";')
  })

  test('quarantine: every deleted transport name stays deleted', () => {
    // The classification table can only see exports; a re-grown transport
    // could be private. So the names are checked against the source directly.
    const resurrected = DELETED_TRANSPORT_NAMES.filter((name) => SOURCE_CODE.includes(name))
    expect(
      resurrected,
      'These names were deleted with the launcher-subprocess transport (#751/G9). A name back ' +
        'in CODE (comments are stripped, so the tombstone may keep naming them) means the ' +
        'second transport is being rebuilt. Reach chiefd over its own API instead.'
    ).toEqual([])
  })

  test('the deleted-name fence is not vacuous: it matches the shapes it claims to catch', () => {
    // Every assertion above is an absence. This is the red proof.
    expect(DELETED_TRANSPORT_NAMES.length).toBeGreaterThan(10)
    const resurrection = 'const runner: LauncherRunner = defaultLauncherRunner;'
    expect(DELETED_TRANSPORT_NAMES.filter((name) => resurrection.includes(name))).toEqual([
      'LauncherRunner',
      'defaultLauncherRunner'
    ])
    expect(countIn('await runChecked(context, args)', 'runChecked(')).toBe(1)
    expect(/\bspawn\s*\(/.test('const child = spawn(binary, args);')).toBe(true)
    // …and it does not fire on the surviving `spawnSync` carve-out.
    expect(/\bspawn\s*\(/.test('const result = spawnSync("tmux", args);')).toBe(false)
  })

  test('quarantine: no raw fetch, and no new chiefd route literal', () => {
    // Every chiefd call goes through `@chief/chiefing/extension-runtime`
    // (pinned in detail by IntercomChiefingCalls.test.ts). A raw `fetch` or a
    // new `/v1/` literal is a call that skipped the client.
    expect(SOURCE_CODE).not.toMatch(/[^.\w]fetch\s*\(/)
    const routeLiterals = new Set(SOURCE_CODE.match(/["'`]\/v1\/[^"'`]*["'`]/g) ?? [])
    expect(
      routeLiterals.size,
      `distinct /v1/ route literals: ${[...routeLiterals].sort().join(', ')}`
    ).toBeLessThanOrEqual(CEILINGS.routeLiterals)
  })
})
