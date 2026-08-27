/**
 * The chiefing extension-runtime subpath is the pane-facing view of chiefing.
 * E4-S8 consumes it from copied Pi extensions and E3-S6
 * materializes this entry plus its dependency graph into a Pi home.
 *
 * That deployment has no `node_modules` and no tsconfig paths mapping, so
 * every reachable source file uses only relative specifiers (or `node:`
 * builtins).
 *
 * It DOES include the discovery client, as of #983. It used to exclude it on
 * the stated grounds that "panes receive an already-resolved chiefd URL" —
 * true only while every install of an org extension was one tmux pane of one
 * company, so the address could be stamped into the process environment as a
 * single variable. (Not naming that variable here is deliberate: rulings
 * D1/D7 ban the name outside `src/discovery`, and a tombstone that trips its
 * own guard gets deleted rather than read.) A host that runs several
 * companies in ONE process has no
 * single correct value for such a variable, and the failure is silent: the
 * wrong company's daemon ANSWERS. So an install now asks beacond which daemon
 * owns ITS company, through the same `resolveCompanyChiefdUrl` every other
 * client uses. One question, one answer-holder.
 *
 * It DOES include auth and identity, as of #751/P7. It used to exclude them
 * on the stated grounds that a pane "needs no credentials" — true only while
 * chiefd authenticated an agent by walking pid ancestry to the pane it
 * descended from. That walk is deleted, so a pane's credential is now the P-256
 * key materialization put in its own pi-home, and the pane must be able to read
 * it and redeem it. Keep this file a re-export view only; a second transport,
 * watcher, or key function would split the contract instead of making it
 * portable.
 */

// Company discovery, pane side (#983): the ONE way any client turns a company
// slug into that company's chiefd base URL. `BeacondUnavailableError` travels
// with it because a pane's boot ladder has to tell "the registry is briefly
// down" apart from "this company does not exist".
export {
  BEACOND_URL_ENV,
  beacondUrlFromEnvironment,
  DEFAULT_BEACOND_URL
} from '../discovery/Company.js'
export { DiscoveryClient, resolveCompanyChiefdUrl } from '../discovery/DiscoveryClient.js'
// The company-directory layout, pane side: a pane is stamped with `<dir>` and
// everything chief owns lives under `<dir>/.chief`, so the segment between the
// two has ONE definition rather than one per reader that joins onto the stamp.
export { personPiHome } from '../discovery/PersonPiHome.js'
// The rendezvous, pane side: a pane's cwd IS its company directory, so it
// learns its own daemon's url AND its own company key from one local file —
// no registry on the path between a pane and its own company.
export { readDaemonRendezvous, rendezvousPath } from '../discovery/Rendezvous.js'
export {
  BeacondUnavailableError,
  ChiefdUnavailableError,
  CompanyNotRunningError,
  isTransientChiefdError,
  OrgRowRefusalError,
  UnknownCompanyError
} from '../Errors.js'
// agent-auth, pane side (#751/P7): a pane proves who it is with the identity
// key in its own pi-home, then presents the token on every chiefd call.
export { AgentTokenManager } from '../resources/AgentToken.js'
export { DocsClient } from '../resources/Docs.js'
// Company genesis is Rust. The Founder extension reaches it through this one
// client; the CLI-subprocess bridge it replaced is deleted.
export {
  FOUNDER_URL_ENV,
  FounderLaunchClient,
  founderUrlFromEnvironment
} from '../resources/FounderLaunch.js'
export { IDENTITY_KEY_FILENAME, readAgentKeypair } from '../resources/Identity.js'
export { postOrgRoute } from '../resources/OrgRoutes.js'
// A4: the ONE pane-side acquirer. Every in-pane caller — the org tools, the
// footer, and every SSE reader — resolves its bearer through this, so a pane
// holds one token cache per (daemon, person, key) rather than one per
// extension that happened to remember to authenticate.
export { paneChiefdTransport, paneTokenManager } from '../resources/PaneIdentity.js'
export { RowStoresClient } from '../resources/RowStores.js'
export { activeSseHubCount, subscribeSse } from '../sse/SseHub.js'
export { computeBackoffDelayMs, SseWatcher } from '../sse/SseWatcher.js'
export { FetchTransport } from '../transport/FetchTransport.js'
export { awaitedDelay, CONNECT_RETRY_BACKOFFS_MS } from '../transport/RetryPolicy.js'
export type {
  AgentKeypair,
  AgentKeypairRead,
  AgentKeyRefusal,
  AgentKeyRefusalReason,
  PaneIdentity,
  PaneKeyRefusalReporter
} from '../types/Auth.js'
export type { CompanyRow, DaemonRendezvous, DiscoveryClientOptions } from '../types/Discovery.js'
export type { DocsRuntime, HealthProbe } from '../types/Health.js'
export type {
  AtomicDirectOutcome,
  DecodedRefusal,
  FounderLaunchInput,
  FounderLaunchPhase,
  FounderLaunchResult,
  OrgRowReadResult,
  ReadOpts,
  RowReadResult,
  WireRowRead
} from '../types/OrgDocs.js'
export type {
  ClearedResult,
  ConvergeSafetyDoc,
  EventOnceMarkerDoc,
  EventOnceMarkerInsertInput,
  HealthMonitorDoc,
  InsertEventOnceMarkerResult,
  LaunchIntentAttribution,
  LaunchIntentDoc,
  MutationJournalDoc,
  MutationJournalRecordDoc,
  OperatorEscalationIntentDoc,
  OperatorEscalationIntentsDoc,
  OperatorEscalationPushDoc,
  PruneEventOnceMarkersResult,
  RuntimeDoc,
  RuntimeOwnerDoc,
  SemanticQueueInsertResult,
  SessionEpochDoc,
  StartPersonResult
} from '../types/RowDocs.js'
export type {
  AuthHeaderProvider,
  ChiefdUnavailableKind,
  HttpResponse,
  HttpTransport
} from '../types/Transport.js'
export type {
  SseBearerProvider,
  SseChannelState,
  SseDocChangeEvent,
  SseStreamOpener,
  SseSubscription,
  SseWatcherOptions,
  WatchSubscribeOptions
} from '../types/Watch.js'
