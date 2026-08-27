// Public types for the #776 contract-test support harness
// (`test/contract/support/bootContractDaemon.ts`). Housed here per
// `lucy/no-exported-type-outside-types-dir`.
//
// A6(c): the daemon is a `CompanyDaemon` (`chiefd run --serve-only`), not a
// mount; that mode's own posture is unchanged and it still serves `/v1/docs/*`.
import type { CompanyDaemon } from '@chief/testing'

import type { ChiefdClient } from '@/ChiefdClient'

export interface ContractDaemon {
  readonly daemon: CompanyDaemon
  readonly client: ChiefdClient
  stop(): Promise<void>
}
