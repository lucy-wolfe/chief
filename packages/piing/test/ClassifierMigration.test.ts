import { ChiefdUnavailableError, OrgRowRefusalError } from '@chief/chiefing/extension-runtime'
import { describe, expect, test } from 'vitest'

import { isTransientTransportFailure } from '../extensions/organization-intercom'

// E4-S8 (#794) classifier migration, per E2's table: ONLY the chiefd arms of
// `isTransientTransportFailure` (`chiefd docstore.*unreachable`,
// `write-service .*unreachable`) migrate to the shared, structural
// `isTransientChiefdError` (true iff `ChiefdUnavailableError` with
// `kind: 'unreachable'`). The tmux/spawn/launcher arms are NOT chiefd
// traffic and stay string-matched, unchanged.

describe('ClassifierMigration (E4-S8): isTransientTransportFailure', () => {
  test('a ChiefdUnavailableError with kind "unreachable" is transient (structural, not message regex)', () => {
    const error = new ChiefdUnavailableError({
      kind: 'unreachable',
      url: 'http://x',
      path: '/v1/org/x/read'
    })
    expect(isTransientTransportFailure(error)).toBe(true)
  })

  test('a ChiefdUnavailableError with kind "timeout" is NOT transient (timeouts are never retried)', () => {
    const error = new ChiefdUnavailableError({
      kind: 'timeout',
      url: 'http://x',
      path: '/v1/org/x/read'
    })
    expect(isTransientTransportFailure(error)).toBe(false)
  })

  test('a ChiefdUnavailableError with kind "http-error" is NOT transient', () => {
    const error = new ChiefdUnavailableError({
      kind: 'http-error',
      url: 'http://x',
      path: '/v1/org/x/read',
      status: 500
    })
    expect(isTransientTransportFailure(error)).toBe(false)
  })

  test('an OrgRowRefusalError (422 refusal) is NOT transient -- a refusal is a genuine failure, never retried', () => {
    const error = new OrgRowRefusalError({
      status: 422,
      code: 'unknown-company',
      detail: 'no such company'
    })
    expect(isTransientTransportFailure(error)).toBe(false)
  })

  test('structural classification does not depend on error.message text at all', () => {
    // Two ChiefdUnavailableError instances with completely different messages
    // but the same kind classify identically -- proving the classifier reads
    // `.kind`, never `.message`.
    const a = new ChiefdUnavailableError({
      kind: 'unreachable',
      url: 'http://a',
      path: '/v1/org/a/read',
      message: 'wildly different text A'
    })
    const b = new ChiefdUnavailableError({
      kind: 'unreachable',
      url: 'http://b',
      path: '/v1/org/b/read',
      message: 'wildly different text B'
    })
    expect(a.message).not.toBe(b.message)
    expect(isTransientTransportFailure(a)).toBe(true)
    expect(isTransientTransportFailure(b)).toBe(true)
  })

  test('the tmux/spawn/launcher arms stay string-matched (never migrated to a structural check)', () => {
    expect(isTransientTransportFailure(new Error('spawn EAGAIN'))).toBe(true)
    expect(isTransientTransportFailure(new Error('spawn EMFILE'))).toBe(true)
    expect(isTransientTransportFailure(new Error('no server running on socket-name'))).toBe(true)
    expect(
      isTransientTransportFailure(new Error('Launcher command ended without an exit status'))
    ).toBe(true)
    expect(
      isTransientTransportFailure(new Error('ChiefD command ended without an exit status'))
    ).toBe(true)
  })

  test('an unrelated ordinary Error is not transient', () => {
    expect(isTransientTransportFailure(new Error("Unknown organization person 'nobody'"))).toBe(
      false
    )
  })
})
