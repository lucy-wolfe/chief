// The ONLY barrel — every public symbol of @chief/chiefing is re-exported
// from here. No epic may import chiefing internals (src/ deep paths).

import { fileURLToPath } from 'node:url'

/**
 * Absolute source entry for the copied extension-runtime subpath.
 *
 * E3-S6 maps `@chief/chiefing/extension-runtime` to this TypeScript file and
 * materializes its relative-import closure into a Pi home. `../src` resolves
 * to the same source tree when this barrel itself was loaded from either
 * `src/index.ts` or the package's emitted `dist/index.js`.
 */
export function chiefingExtensionRuntimeSourceEntry(): string {
  return fileURLToPath(new URL('../src/extensionruntime/index.ts', import.meta.url))
}

export { ChiefdClient } from '@/ChiefdClient'
export {
  BEACOND_URL_ENV,
  beacondUrlFromEnvironment,
  DEFAULT_BEACOND_URL,
  parseCompanyRow
} from '@/discovery/Company'
export { companyStoreDbPath } from '@/discovery/CompanyStorePath'
export { DiscoveryClient, resolveCompanyChiefdUrl } from '@/discovery/DiscoveryClient'
export { personPiHome } from '@/discovery/PersonPiHome'
export {
  parseDaemonRendezvous,
  readDaemonRendezvous,
  RENDEZVOUS_FILENAME,
  rendezvousPath
} from '@/discovery/Rendezvous'
export {
  AuthAcquisitionError,
  BeacondUnavailableError,
  ChiefdUnavailableError,
  CompanyLifecycleRefusalError,
  CompanyNotRunningError,
  DiscoveryRefusalError,
  isTransientBeacondError,
  isTransientChiefdError,
  OrgRowRefusalError,
  PersonContractsRefusalError,
  ReminderRefusalError,
  SeqConflictError,
  UnknownCompanyError
} from '@/Errors'
export { AgentTokenManager } from '@/resources/AgentToken'
export { AggregatesClient } from '@/resources/Aggregates'
export { ApiHostLaunchProfileClient } from '@/resources/ApiHostLaunchProfile'
export { AuthClient } from '@/resources/Auth'
export {
  CHIEFD_HOST_URL_ENV,
  chiefdHostUrlFromEnvironment,
  CompanyLifecycleClient,
  DEFAULT_CHIEFD_HOST_URL
} from '@/resources/CompanyLifecycle'
export { DocsClient } from '@/resources/Docs'
export {
  FOUNDER_URL_ENV,
  FounderLaunchClient,
  founderUrlFromEnvironment
} from '@/resources/FounderLaunch'
export {
  AUTH_DOMAIN_TAG,
  authChallengeMessage,
  ensurePersonIdentityKey,
  generateAgentKeypair,
  IDENTITY_KEY_FILENAME,
  loadOrCreateAgentKeypair,
  operatorKeyPath,
  publicSpkiBase64FromPrivatePem,
  readAgentKeypair,
  readIdentityKeyPem,
  signAuthChallenge,
  verifyAuthChallenge
} from '@/resources/Identity'
export { MailboxClient } from '@/resources/Mailbox'
export { ManifestClient } from '@/resources/Manifest'
export { OrgSliceClient } from '@/resources/OrgSlice'
// The one conflict `code` a lost CAS sequence answers with. Public because it
// is a two-sided contract with Rust that `scripts/test/refusal-taxonomy.test.mjs`
// pins by reading this constant out of the source — the publisher-route sweep
// deleted the last CAS client method, so an in-package caller no longer keeps
// it alive, and the contract outlives the client that used to consume it.
export { SEQ_CONFLICT_CODE } from '@/resources/OrgRoutes'
export { paneChiefdTransport, paneTokenManager } from '@/resources/PaneIdentity'
export { PersonContractsClient } from '@/resources/PersonContracts'
export { MIN_REMINDER_INTERVAL_MS, RemindersClient } from '@/resources/Reminders'
export { RowStoresClient } from '@/resources/RowStores'
export { RuntimeClient } from '@/resources/Runtime'
export { SessionLifecycleClient } from '@/resources/SessionLifecycle'
export { SettingsClient } from '@/resources/Settings'
export { StaffingClient } from '@/resources/Staffing'
export { readSseFrames, SseFrameDecoder } from '@/sse/SseFrames'
export { activeSseHubCount, subscribeSse } from '@/sse/SseHub'
export { computeBackoffDelayMs, SseWatcher } from '@/sse/SseWatcher'
export { describeFetchFailure, fetchFailureDetail } from '@/transport/FetchFailure'
export { FetchTransport } from '@/transport/FetchTransport'
export {
  awaitedDelay,
  CONNECT_RETRY_BACKOFFS_MS,
  ENSURE_SCHEMA_RETRY_DELAYS_MS,
  retryDelayWithJitter
} from '@/transport/RetryPolicy'
export type {
  ApiHostActuation,
  ApiHostLaunchProfile,
  ApiHostLaunchProfileRead
} from '@/types/ApiHostLaunchProfile'
export type {
  AgentKeypair,
  AgentKeypairRead,
  AgentKeyRefusal,
  AgentKeyRefusalReason,
  ChallengeResponse,
  PaneIdentity,
  PaneKeyRefusalReporter,
  TokenResponse
} from '@/types/Auth'
export type {
  CompanyLaunchResult,
  CompanyLifecyclePhase,
  CompanyLifecyclePhaseName,
  CompanyStopResult,
  CreateCompanyInput
} from '@/types/CompanyLifecycle'
export {
  COMPANY_LIFECYCLE_PHASE_NAMES,
  isCompanyLifecyclePhaseName
} from '@/types/CompanyLifecycle'
export type {
  BeacondUnavailableKind,
  CompanyRow,
  DaemonRendezvous,
  DiscoveryClientOptions
} from '@/types/Discovery'
export type {
  DocsRuntime,
  HealthProbe,
  WriterQueueCurrent,
  WriterQueueSnapshot
} from '@/types/Health'
export type {
  ContractUnitMetadata,
  ContractUnitSeed,
  DepartmentRecord,
  DepartmentSeed,
  EmploymentState,
  HirePersonSeed,
  OrganizationManifest,
  OrganizationPolicy,
  OrganizationSpec,
  OrganizationUnitKind,
  PersonKind,
  PersonRecord,
  PersonSeed,
  UnitState
} from '@/types/Organization'
export { ORGANIZATION_SCHEMA_VERSION, ROOT_DEPARTMENT_ID } from '@/types/Organization'
export type {
  AtomicDirectOutcome,
  FounderLaunchInput,
  FounderLaunchPhase,
  FounderLaunchResult,
  OrgRowReadResult,
  OrgRowReadResultWithSeq,
  ReadOpts,
  RowReadResult
} from '@/types/OrgDocs'
export type {
  ActivityCommandStatus,
  BuildPersonContractsResult,
  ColdStartClearResult,
  CompanyTreeDepartment,
  CompanyTreePerson,
  CompanyTreeResult,
  GracefulTransition,
  InScopeResult,
  LifecycleDepartmentStatus,
  LifecyclePersonStatus,
  OrganizationLifecycleStatus,
  StaffingLifecycleResult,
  StartAttribution,
  TransitionAction,
  TransitionStatus,
  TreeLinesResult,
  UnitRemovalImpact,
  UnitRemovalPreview,
  UnitSubtreeResult
} from '@/types/OrgSlice'
export type {
  OrganizationPersonContractsDocument,
  PersonContractEntry,
  PersonContractsReadResult
} from '@/types/PersonContracts'
export type {
  ArmReminderInput,
  ListRemindersInput,
  ListRemindersResult,
  Reminder,
  ReminderResult,
  StopReminderInput
} from '@/types/Reminders'
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
} from '@/types/RowDocs'
export type {
  CompanyActionRuntime,
  LaunchInput,
  PersonRuntimeExtensionDrift,
  RuntimeLaunchResult,
  RuntimeOwnership,
  RuntimeOwnershipResult,
  RuntimeStopResult
} from '@/types/Runtime'
export type {
  AckDrainResult,
  CompactAnchor,
  DoorbellOutcome,
  DoorbellPlan,
  DoorbellSettlement,
  MaintenanceAction,
  MaintenanceClaim,
  MaintenanceRequest,
  MaintenanceStatus,
  OperatorEscalationDrainResult,
  OperatorEscalationRecord,
  QueueMaintenanceInput,
  RecoveredMaintenance,
  SessionEpoch,
  SessionMaintenanceLedger,
  WorkerIdentity
} from '@/types/SessionLifecycle'
export type { OrgSettings } from '@/types/Settings'
export type {
  AtomicCreateDepartmentOutcome,
  AtomicDepartmentHead,
  AtomicDepartmentNewPersonSeed,
  AtomicDepartmentStaff,
  AtomicDepartmentUnit,
  AtomicHireOutcome,
  AtomicPersonSeed,
  AtomicRemoveDepartmentOutcome,
  AtomicReparentDepartmentOutcome,
  AtomicStaffingRequester,
  AtomicTransferPersonOutcome,
  Refusal
} from '@/types/Staffing'
export type { ChiefdClientOptions } from '@/types/Transport'
export type { ChiefdUnavailableKind } from '@/types/Transport'
export type { AuthHeaderProvider, HttpResponse, HttpTransport } from '@/types/Transport'
export type {
  SseBearerProvider,
  SseChannelState,
  SseDocChangeEvent,
  SseFrame,
  SseStreamOpener,
  SseSubscription,
  SseWatcherOptions,
  WatchSubscribeOptions
} from '@/types/Watch'
