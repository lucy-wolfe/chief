// Regression lock for the terminal->chief lucy plugin subset decision
// (E0-S3, #754). 34 of terminal's 48 rules were kept (25 core/template +
// 9 generic app-layer rules); 14 terminal-domain rules (Durable Objects,
// Cloudflare, Privy, sandboxd, zipbox, terminal-core layering) were dropped.
// Two of the 34 have since been RETIRED — see RETIRED_RULES below — so 32
// are registered today and the original 48 still balance across the three
// categories.
// This test hardcodes both lists so a drift — a rule silently added,
// removed, or renamed — turns red instead of slipping through unnoticed.
//
// `enforce-web-client-service-suffix` moved DROPPED -> KEPT in E6-S1 (#806):
// E0-S3 dropped it as "terminal-domain", but the rule is entirely generic
// (gated only on the literal path `/apps/web/src/`, no Cloudflare/Durable
// Object/Privy reference at all) — it was misclassified because chief had
// no `apps/web` yet when E0-S3 ran. E6-S1 creates `apps/web`, so the
// exclusion's premise no longer holds; see DECISIONS.md.
//
// `no-promise-to-serializer` is chief's FIRST genuinely NEW rule (#849) —
// not a terminal port. It gets its own list (`NEW_CHIEF_RULES`) rather than
// being folded into `KEPT_RULES`, so "34 of terminal's 48" stays true
// forever and this file keeps distinguishing "ported" from "invented here"
// instead of quietly becoming a count that drifts from its own history.
//
// `no-unknown-callback-return` (#866) is the second chief-invented rule:
// flags a callback-seam field typed `=> unknown`/`=> any`, which launders
// promise-ness past no-floating-promises/no-misused-promises.
//
// `no-unbounded-spawn-in-test` (#855) is the third: requires an explicit
// deadline on every subprocess spawn inside a test -- the structural form
// of #847's sweep, so a new instance (like #798's) can't arrive unnoticed.

import { execFileSync } from 'node:child_process'
import { readdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import plugin from '../index.js'

const here = dirname(fileURLToPath(import.meta.url))
const rulesDir = join(here, '..', 'rules')

/**
 * KEPT at the port, RETIRED since, because the rule's SUBJECT left the tree.
 *
 * This category exists so the historical accounting below stays true. E0-S3's
 * finding was "34 of terminal's 48 rules are kept"; that is a fact about a
 * decision made on a date, and deleting a rule must not quietly turn it into
 * a smaller number that matches nothing anybody decided. KEPT + DROPPED +
 * RETIRED still accounts for all 48.
 *
 * A retired rule is treated exactly like a dropped one from here on: not
 * registered, and no file in `rules/`. The difference is only in the history
 * — DROPPED was never wanted here, RETIRED was wanted and outlived its
 * subject.
 *
 * Each row carries `subjectPattern`: the thing its rule can only fire on,
 * asserted absent from tracked source below, so the category proves its
 * premise instead of stating it.
 *
 * @type {ReadonlyArray<{rule: string, subjectPattern: string}>}
 */
const RETIRED_RULES = [
  // Both fired only on `bignumber.js` imports and BigNumber values. The
  // dependency had zero importers anywhere in `apps`, `packages` or
  // `scripts` and was removed; with it gone, neither rule can match a line
  // that could exist. A lint rule whose subject is not in the tree does not
  // protect anything — it reports green by seeing nothing, which is the
  // failure mode this repository names in half its guards.
  //
  // `subjectPattern` is the IMPORT, not the word. Both rules can only fire on
  // a value that came from the `bignumber.js` module: one inspects the import
  // declaration directly, the other a BigNumber value, which requires the
  // import to exist. A bare-word pattern was tried first and reported three
  // files -- a cross-reference in a sibling rule's comment, a message string
  // in an unrelated rule, and this file naming the pattern in order to check
  // it. None of those is a subject the rules could fire on, and widening the
  // premise to cover prose would have made the check unpassable for a reason
  // that has nothing to do with the claim.
  { rule: 'enforce-bignumber-default-import', subjectPattern: "(from|require\\()\\s*['\"]bignumber\\.js" },
  { rule: 'no-bignumber-to-string', subjectPattern: "(from|require\\()\\s*['\"]bignumber\\.js" },
]

/** Just the names, for the accounting assertions below. */
const RETIRED_RULE_NAMES = RETIRED_RULES.map((row) => row.rule)

const KEPT_RULES = [
  // core/template rules (23)
  'enforce-test-file-location',
  'enforce-test-import-alias',
  'enforce-url-constructor-two-args',
  'exact-package-json-dependency-versions',
  'no-async-in-utils',
  'no-barrel-re-export',
  'no-console-usage',
  'no-dead-address-literal',
  'no-default-in-enum-switch',
  'no-empty-file',
  'no-exported-type-outside-types-dir',
  'no-generic-filenames',
  'no-indexed-type-access',
  'no-inline-zod-infer',
  'no-json-stringify',
  'no-optional-nullable',
  'no-pass-through-alias-export',
  'no-process-env',
  'no-raw-null-check',
  'no-raw-zod-bigint',
  'no-v8-ignore',
  'prefer-switch-for-enum',
  'require-eslint-disable-explanation',
  // generic app-layer rules (9)
  'enforce-as-json-response',
  'enforce-handle-action',
  'enforce-web-client-service-suffix',
  'no-direct-db-outside-stores',
  'no-fetch-outside-scoped-helpers',
  'no-node-env-default',
  'no-public-host-bind',
  'no-response-return-in-services',
  'no-service-import-in-helpers'
]

// Rules invented for chief, never present in terminal's 48. Each entry
// should carry the issue that added it, same discipline as KEPT_RULES'
// per-rule provenance.
const NEW_CHIEF_RULES = [
  'no-promise-to-serializer', // #849
  'no-unknown-callback-return', // #866
  'no-unbounded-spawn-in-test' // #855
]

const DROPPED_RULES = [
  'core-layer-boundaries',
  'enforce-do-extends-abstract',
  'no-direct-do-namespace-access',
  'no-inline-do-name',
  'enforce-lucy-services-naming',
  'enforce-resolved-api-url',
  'lucy-helpers-must-be-class-based',
  'no-apps-api-backend-calls',
  'no-controller-instantiation-outside-helper-cloudflare',
  'no-cross-module-logger',
  'no-use-privy-auth-session',
  'no-zipbox-app-api-routes',
  'sandbox-epoch-single-writer',
  'sandboxd-no-public-port'
]

describe('lucy plugin rule catalog', () => {
  it('exports exactly the 32 still-kept (ported) rules plus the chief-invented ones, sorted list matches', () => {
    const exported = Object.keys(plugin.rules).sort()
    expect(exported).toEqual([...KEPT_RULES, ...NEW_CHIEF_RULES].sort())
  })

  it('exports exactly 35 rules (32 still-ported + 3 invented for chief)', () => {
    expect(Object.keys(plugin.rules)).toHaveLength(32 + NEW_CHIEF_RULES.length)
  })

  it('no chief-invented rule name collides with a ported or dropped terminal rule', () => {
    for (const invented of NEW_CHIEF_RULES) {
      expect(KEPT_RULES).not.toContain(invented)
      expect(DROPPED_RULES).not.toContain(invented)
    }
  })

  it('every file in rules/ is registered in index.js and vice versa', () => {
    const filesOnDisk = readdirSync(rulesDir)
      .filter((f) => f.endsWith('.js'))
      .map((f) => f.replace(/\.js$/, ''))
      .sort()
    const registered = Object.keys(plugin.rules).sort()
    expect(filesOnDisk).toEqual(registered)
  })

  it('does not register any of the 14 dropped terminal-domain rules', () => {
    for (const dropped of DROPPED_RULES) {
      expect(plugin.rules).not.toHaveProperty(dropped)
    }
  })

  it('does not have a rules/ file for any dropped rule', () => {
    const filesOnDisk = new Set(
      readdirSync(rulesDir)
        .filter((f) => f.endsWith('.js'))
        .map((f) => f.replace(/\.js$/, ''))
    )
    for (const dropped of DROPPED_RULES) {
      expect(filesOnDisk.has(dropped)).toBe(false)
    }
  })

  it('kept + dropped + retired still accounts for all 48 original terminal rules', () => {
    expect(KEPT_RULES).toHaveLength(32)
    expect(DROPPED_RULES).toHaveLength(14)
    expect(RETIRED_RULE_NAMES).toHaveLength(2)
    expect(KEPT_RULES.length + DROPPED_RULES.length + RETIRED_RULE_NAMES.length).toBe(48)
    const categories = [KEPT_RULES, DROPPED_RULES, RETIRED_RULE_NAMES]
    for (const [a, b] of [[0, 1], [0, 2], [1, 2]]) {
      expect(categories[a].filter((r) => categories[b].includes(r))).toEqual([])
    }
  })

  it('every retired rule has PROVEN its premise: its subject is absent from the tree', () => {
    // THE CATEGORY MUST NOT BECOME A HIDING PLACE.
    //
    // RETIRED means "the rule's SUBJECT left the tree" -- a stronger claim
    // than DROPPED, which only means "never wanted here". Stated in prose,
    // that premise is unchecked, and a category with an unchecked premise is
    // somewhere a deletion can be filed rather than justified: retire a rule
    // whose subject is alive and the tree silently loses a live check.
    //
    // So each row names the pattern its rule can only fire on, and that
    // pattern must match NOTHING in tracked source. This is the mirror of the
    // redaction guard's RULED_SURVIVORS rows, which assert their terms are
    // still PRESENT -- the same liveness idea from the other side, and the
    // two are worth more taught twice than either alone.
    const repoRoot = join(here, '..', '..', '..')
    for (const { rule, subjectPattern } of RETIRED_RULES) {
      let hits
      try {
        hits = execFileSync('git', ['grep', '-lIiE', subjectPattern, '--', 'apps', 'packages', 'scripts'], {
          cwd: repoRoot,
          encoding: 'utf8',
        })
          .split('\n')
          .filter(Boolean)
          // This file names the pattern in order to check it, and the ledger
          // records the retirement. Neither is a live subject.
          .filter((path) => !path.endsWith('RuleCatalog.test.mjs'))
      } catch (error) {
        if (error.status === 1) hits = []
        // git grep exits 1 on no match, which is this assertion's success.
        // Any other failure means the search did not run, and a search that
        // did not run has not passed.
        else throw error
      }
      expect(
        hits,
        `${rule} is RETIRED, which claims its subject left the tree -- but ${subjectPattern} still ` +
          'appears in tracked source. Either the rule should not have been retired, or those sites ' +
          'need removing first. A retired category that does not check its own premise is where a ' +
          'live rule goes to be deleted quietly.'
      ).toEqual([])
    }
  })

  it('does not register any retired rule, and keeps no rules/ file for one', () => {
    const filesOnDisk = new Set(
      readdirSync(rulesDir)
        .filter((f) => f.endsWith('.js'))
        .map((f) => f.replace(/\.js$/, ''))
    )
    for (const retired of RETIRED_RULE_NAMES) {
      expect(plugin.rules).not.toHaveProperty(retired)
      expect(filesOnDisk.has(retired)).toBe(false)
    }
  })
})
