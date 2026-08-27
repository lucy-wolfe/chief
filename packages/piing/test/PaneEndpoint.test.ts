import { afterEach, beforeEach, describe, expect, test } from 'vitest'

import {
  OrgChiefdUrlUnsetError as IntercomUnsetError,
  readDurableDocumentCached as intercomRead
} from '../extensions/organization-intercom'
import {
  OrgChiefdUrlUnsetError as TeamUiUnsetError,
  readFooterStoreDocument
} from '../extensions/team-ui'
// E4-S8 (#794): "the in-pane fixed-port fallbacks die here" -- a pane's
// base URL is the ORG_CHIEFD_URL the chiefd that spawned it stamped in; there
// is no fixed-port fallback anywhere in the tree (ruling D0/D1). Each of the
// two converted extensions throws its own OrgChiefdUrlUnsetError instead of
// guessing http://127.0.0.1:8792, which could belong to another company's
// daemon entirely.
//
// BOTH no longer read that variable at all (#983): each resolves ITS OWN
// company's daemon from beacond and CARRIES the answer — the intercom on its
// runtime context, team-ui as an explicit argument on every read.
//
// The refusal is unchanged, and this file asserts BOTH — that an address-less
// call still refuses rather than guessing, and that a call carrying its own
// address is untouched by whatever the process holds. What resolution itself
// proves is in `CompanyDaemonResolution.test.ts` (the intercom) and
// `PaneExtensionDaemonResolution.test.ts` (team-ui).

/* eslint-disable lucy/no-process-env, lucy/no-raw-null-check */
/* This whole file's subject IS the ORG_CHIEFD_URL env var these extensions
   used to read; there is no src/common/env.ts indirection to import here, and
   it must be the live process env, because the claim under test is about what
   the ambient process holds. Both are now asserted to IGNORE it — a
   context that carries no address refuses, and a context that carries one is
   untouched by whatever the process happens to hold. */

describe('PaneEndpoint (E4-S8): no daemon address means a refusal, never a fixed-port guess', () => {
  const previous = process.env.ORG_CHIEFD_URL

  beforeEach(() => {
    delete process.env.ORG_CHIEFD_URL
  })

  afterEach(() => {
    if (previous === undefined) delete process.env.ORG_CHIEFD_URL
    else process.env.ORG_CHIEFD_URL = previous
  })

  test('team-ui.ts: a read carrying no chiefd URL rejects with OrgChiefdUrlUnsetError', async () => {
    await expect(
      readFooterStoreDocument(undefined, '0123456789ab', 'supervision', undefined)
    ).rejects.toBeInstanceOf(TeamUiUnsetError)
  })

  test('team-ui.ts: a read that CARRIES its URL is immune to the ambient variable', async () => {
    await expect(
      readFooterStoreDocument('http://127.0.0.1:1', '0123456789ab', 'supervision', undefined)
    ).rejects.not.toBeInstanceOf(TeamUiUnsetError)
  })

  test('organization-intercom.ts: a context carrying no chiefd URL rejects with OrgChiefdUrlUnsetError', async () => {
    // This one reads its endpoint off the CONTEXT its install RESOLVED, not
    // off the ambient process — so the unset case is a context with no
    // `chiefdUrl`, and deleting the variable (which this block does) cannot
    // reach it. The refusal is the same one, for the same reason: there is no
    // fixed-port fallback, because a guessed address may belong to another
    // company's daemon.
    await expect(
      intercomRead(
        {
          organizationDir: '/tmp/orgs/acme',
          identityDir: '/tmp/orgs/acme/.chief',
          organization: 'acme',
          personId: 'ceo',
          launcherRoot: '/tmp/launcher'
        },
        'supervision'
      )
    ).rejects.toBeInstanceOf(IntercomUnsetError)
  })

  test('organization-intercom.ts: a context that CARRIES its URL is immune to the ambient variable', async () => {
    // The direction that matters for a multi-company host: with the process
    // variable deleted (this block's `beforeEach`), a call still resolves its
    // address — so no `org_*` path can be steered by whatever the process
    // happens to hold. It reaches a port nothing is listening on, which is a
    // TRANSPORT failure and emphatically not `OrgChiefdUrlUnsetError`.
    await expect(
      intercomRead(
        {
          organizationDir: '/tmp/acme',
          identityDir: '/tmp/acme/.chief',
          organization: 'acme',
          personId: 'ceo',
          launcherRoot: '/tmp/launcher',
          chiefdUrl: 'http://127.0.0.1:1',
          companyKey: '0123456789ab'
        },
        'supervision'
      )
    ).rejects.not.toBeInstanceOf(IntercomUnsetError)
  })

  test('every OrgChiefdUrlUnsetError is a distinct per-file class (no shared import masking a fixed-port fallback)', () => {
    // Each copied extension is self-contained (materialized independently
    // into its own pi-home), so each intentionally defines its OWN class --
    // this is not two re-exports of one shared error.
    expect(TeamUiUnsetError).not.toBe(IntercomUnsetError)
  })

  test('self-check: both classes really do extend Error with the expected name (not vacuously green)', () => {
    for (const ErrorClass of [TeamUiUnsetError, IntercomUnsetError]) {
      const instance = new ErrorClass()
      expect(instance).toBeInstanceOf(Error)
      expect(instance.name).toBe('OrgChiefdUrlUnsetError')
      expect(instance.message.length).toBeGreaterThan(0)
    }
  })
})
/* eslint-enable lucy/no-process-env, lucy/no-raw-null-check */
