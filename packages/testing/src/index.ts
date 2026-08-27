/**
 * `@chief/testing` — the shared chiefd vitest harnesses (docstore-only, and
 * the full company surface via `chiefd run --serve-only`). Barrel:
 * the only public entrypoint. The vitest-native successor to
 * `bunfig.toml`'s `preload = ["./tests/setup-durable-store.ts"]`: one daemon
 * per package vitest RUN instead of one per test process, with no
 * copy-pasted spawn code across packages.
 */
// DELETED: `import '@/types/Provided'` — an ambient `declare module 'vitest'`
// that typed `inject()` with `chiefdUrl`/`chiefdSlug`/`chiefdDataRoot`. No
// test in the repo calls `inject()`, so it typed nothing; two of the three
// keys named a slug-keyed daemon and a data root that no longer exist. A
// declaration whose only effect is to describe a retired shape is worse than
// absent, because it reads as a contract.

export {
  assertChiefdBinaryBuilt,
  assertChiefdBinaryCurrent,
  chiefdBinarySkipTitle,
  chiefdBinaryTestGate,
  chiefdBuildCommand,
  isRunningInCI,
  newestChiefdSource,
  resolveChiefBinaryPath,
  resolveChiefdDaemonBinaryPath,
  resolveChiefdTargetRoot
} from '@/ChiefdBinary'
export { seedCompany, startCompanyDaemon } from '@/CompanyDaemon'
export {
  formatDaemonLogTail,
  readDaemonLogTail,
  surfaceDaemonLogOnFailure,
  surfaceDaemonOutputOnFailure,
  tailLines
} from '@/DaemonLogOnFailure'
export { allocateEphemeralPort } from '@/EphemeralPort'
export {
  acquireOperatorBearer,
  authChallengeMessage,
  createOperatorFetch,
  operatorKeyPath,
  signAuthChallenge
} from '@/OperatorBearer'
export { createTempDir } from '@/TempDir'
export { chiefdRunArgv, startTmuxHostedCompany } from '@/TmuxHostedCompanyDaemon'
export type { ChiefdBinaryTestGate } from '@/types/ChiefdBinary'
export type { CompanyDaemon, CompanyDaemonOptions } from '@/types/CompanyDaemon'
export type { AuthorizedFetch, OperatorBearerOptions } from '@/types/OperatorBearer'
export type { TempDir } from '@/types/TempDir'
export type {
  ChiefdRunArgvOptions,
  TmuxHostedCompany,
  TmuxHostedCompanyOptions
} from '@/types/TmuxHostedCompany'
