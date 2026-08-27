import { describe, expect, it } from 'vitest'

import { companyStoreDbPath } from '@/discovery/CompanyStorePath'

describe('companyStoreDbPath', () => {
  /** THE STORE IS INSIDE THE DIRECTORY IT DESCRIBES.
   *
   * The retired layout put it deliberately OUTSIDE, as
   * `<orgsRoot>/.<slug>.chief.db`, so that removing the Pi-artifact tree
   * `<orgsRoot>/<slug>/` could not destroy the database beside it. A company IS
   * a directory now: removing one is `rm -rf <dir>/.chief`, one act, and a
   * store that survived it would be the residue. */
  it('is the directory own .chief/db/chief.db', () => {
    expect(companyStoreDbPath('/work/northstar')).toBe('/work/northstar/.chief/db/chief.db')
  })

  /** Slugless and unconditional. The slug used to be a caller-supplied path
   * segment this function had to guard, because a `/` in it walked out of the
   * data root; nothing caller-supplied reaches the join any more. */
  it('takes the directory alone — no slug reaches the join', () => {
    expect(companyStoreDbPath('/work/a b&c')).toBe('/work/a b&c/.chief/db/chief.db')
    expect(companyStoreDbPath('/work/NorthStar')).toBe('/work/NorthStar/.chief/db/chief.db')
  })

  /** Two directories holding companies with the same display word get two
   * stores, which is the whole point of keying by directory. */
  it('gives two same-named companies two separate stores', () => {
    expect(companyStoreDbPath('/work/acme')).not.toBe(companyStoreDbPath('/elsewhere/acme'))
  })
})
