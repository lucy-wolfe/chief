/**
 * The Founder's status line, locked by name.
 *
 * A live end-to-end run on a fresh machine read:
 *
 *     launcher • @operator • e2e-main
 *
 * Three separate wrongs in one line, and all three had been reported before:
 *
 *  - `@operator` — `operator` is the human at the keyboard. It is not an
 *    identity this product ships, so the handle pointed at nothing the user
 *    could look up. The one pre-company identity is **Founder**.
 *  - `launcher` — the retired pre-company mode. Its session, its skill and its
 *    role string were all deleted; the footer kept printing the name, which is
 *    the only place a retired product name can still look alive.
 *  - `e2e-main` — the git branch of the ChiefD source clone the Founder's Bun
 *    process happens to run from. A git branch is not a fact about a company,
 *    a person or the Founder, so the field is gone rather than re-sourced.
 *
 * These are pinned as STRINGS, not as "whatever the code says", because the
 * previous fixes were made in one surface and left wrong in another.
 */
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { isNullish } from '@test/support/Nullish'
import {
  footerIdentity,
  footerIdentityFields,
  FOUNDER_FOOTER_IDENTITY,
  organizationFooterIdentity
} from '@test-assets/team-ui'
import { describe, expect, test } from 'vitest'

const TEAM_UI_SOURCE = readFileSync(
  fileURLToPath(new URL('../extensions/team-ui.ts', import.meta.url)),
  'utf8'
)

describe('the pre-company footer identity is Founder', () => {
  test('a pane with no company renders `chiefd • @founder`', () => {
    const identity = footerIdentity({})
    const fields = footerIdentityFields(identity)
    expect(fields.role).toBe('@founder')
    expect(fields.team).toBe('chiefd')
    // The exact composed slot, in order, with nothing between them.
    expect(`${fields.team} • ${fields.role}`).toBe('chiefd • @founder')
  })

  test('neither retired name survives anywhere in the identity', () => {
    const identity = footerIdentity({})
    expect(identity.role).not.toBe('operator')
    expect(identity.team).not.toBe('launcher')
    expect(Object.values(FOUNDER_FOOTER_IDENTITY).join(' ')).not.toMatch(/operator|launcher/)
  })

  test('a stray team-launcher environment cannot resurrect either name', () => {
    // The team-directory / operator-pointer lookup is deleted, not merely
    // out-ranked: an ambient variable left over from the retired mode must not
    // be able to put `@operator` back on a Founder's screen.
    const identity = footerIdentity({
      TEAM_LAUNCHER_ROLE: 'operator',
      TEAM_LAUNCHER_TEAM_DIR: '/tmp/nowhere',
      TEAM_LAUNCHER_OPERATOR_POINTER: '/tmp/nowhere/pointer.json'
    })
    expect(identity).toEqual(FOUNDER_FOOTER_IDENTITY)
  })

  test('the footer renders the username and never the person id', () => {
    // SUPERSEDED AND INVERTED, openly. This asserted
    // `role === '@head-quant-research'` — the person's kebab id with an `@`
    // stuck on the front, which reads as a handle and is not one. The footer
    // is where a reader looks to learn what to call somebody, so an internal
    // key presented there teaches the wrong name to every person who reads it.
    //
    // The id is still carried, because the mailbox and the document store are
    // addressed by it. It is simply not what is shown.
    const identity = footerIdentity({
      ORG_LAUNCHER_ORGANIZATION: 'leo-capital',
      ORG_LAUNCHER_PERSON: 'head-quant-research',
      ORG_LAUNCHER_PERSON_NAME: 'priya'
    })
    expect(identity).toEqual({
      team: 'leo-capital',
      role: 'head-quant-research',
      handle: 'priya'
    })
    expect(footerIdentityFields(identity).role).toBe('@priya')
    expect(footerIdentityFields(identity).role).not.toContain('head-quant-research')
  })

  test('a company pane with no username renders an empty person slot, never the id', () => {
    // The identity is STILL resolved without a username, because this function
    // is not display-only: it also gates daemon resolution and pane identity,
    // so returning undefined would cost the pane its connection rather than
    // just its name. The rule that the key is never shown is enforced where
    // showing happens.
    const identity = organizationFooterIdentity({
      ORG_LAUNCHER_ORGANIZATION: 'leo-capital',
      ORG_LAUNCHER_PERSON: 'head-quant-research'
    })
    expect(identity).toEqual({ team: 'leo-capital', role: 'head-quant-research' })
    if (isNullish(identity)) throw new Error('the identity must resolve without a username')

    const fields = footerIdentityFields(identity)
    expect(fields.role).toBe('')
    expect(fields.role).not.toContain('head-quant-research')
    expect(fields.team).toBe('leo-capital')
  })

  test('an organization is still required to be an organization identity', () => {
    expect(organizationFooterIdentity({ ORG_LAUNCHER_ORGANIZATION: 'leo-capital' })).toBeUndefined()
    expect(
      organizationFooterIdentity({ ORG_LAUNCHER_PERSON: 'head-quant-research' })
    ).toBeUndefined()
  })

  test('the Founder footer is unchanged: its role is already a handle, not a key', () => {
    expect(footerIdentityFields(FOUNDER_FOOTER_IDENTITY).role).toBe('@founder')
  })
})

describe('the git branch field is deleted, not merely hidden', () => {
  test('the footer never asks Pi for a branch', () => {
    // `getGitBranch()` is what produced `e2e-main`, and `onBranchChange` was a
    // re-render channel for it. A field nobody renders must have no reader and
    // no change channel left behind to be re-wired by accident.
    expect(TEAM_UI_SOURCE).not.toContain('getGitBranch')
    expect(TEAM_UI_SOURCE).not.toContain('onBranchChange')
  })

  test('there is no branch field token to paint one with', () => {
    expect(TEAM_UI_SOURCE).not.toMatch(/FOOTER_FIELD_TOKENS\.branch/)
  })
})
