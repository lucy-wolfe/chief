/**
 * Rust/TypeScript tool-list parity, for every list that exists on both sides.
 *
 * `ManagerToolGate3Parity.test.ts` locked exactly one pair — chiefd's
 * `MANAGER_TOOLS` against `ORGANIZATION_MANAGER_TOOL_NAMES`. Every OTHER tool
 * list chiefd carries was a second source of truth with nothing asserting it
 * agreed with the first: baseline, active-runtime, root-executive and builtin.
 * A divergence in any of them is invisible — the pane is simply launched
 * without a tool the extension registered.
 *
 * Both sides are DERIVED, never re-typed here. The TypeScript side is imported
 * from the module that defines it; the Rust side is parsed out of the Rust
 * source as text (no cargo, no cross-crate build — the TS suite always runs
 * in-repo with both trees present). A copy of either list in this file would
 * be a THIRD source of truth and would make the problem worse.
 *
 * The manager pair is deliberately NOT re-asserted here; it is locked next
 * door in `ManagerToolGate3Parity.test.ts`.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { ORGANIZATION_BASELINE_TOOL_NAMES } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

import {
  BUILTIN_TOOLS as EXTENSION_RUNTIME_BUILTIN_TOOLS,
  ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
  ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
  ORGANIZATION_SUBTREE_TOOL_NAMES
} from '@/extensionruntime/OrganizationTools'
import { BUILTIN_TOOLS } from '@/policy/CapabilityPolicy'

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')
/** chiefd's gate-3 GRANT authority: what lands in a pane's `--tools` allowlist. */
const RESOURCE_CATALOG_RS = join(
  REPO_ROOT,
  'apps/chiefd/crates/chiefd-host/src/converge_apply/resource_catalog.rs'
)
/** chiefd's VALIDATION authority: the tool floor. (The protected-name table
 * stood here too and is deleted with per-person resource selection — see the
 * tombstone in the pair table below.) */
const MATERIALIZE_PLAN_RS = join(
  REPO_ROOT,
  'apps/chiefd/crates/chiefd-host/src/materialize/plan.rs'
)
/** Strip `//` line comments so a quoted string inside a comment is never
 * mistaken for a list entry. */
function withoutLineComments(source: string): string {
  return source.replaceAll(/\/\/[^\n]*/g, '')
}

function quotedStrings(fragment: string): string[] {
  return [...fragment.matchAll(/"([^"]+)"/g)].map((match) => {
    const [, value] = match
    if (typeof value !== 'string') throw new Error('could not read a quoted Rust string')
    return value
  })
}

/** Parse a `[pub ]const NAME: [&str; N] = [ "a", "b", … ];` array literal out of
 * a Rust source file and return its string entries. */
function rustToolArray(file: string, constName: string): string[] {
  const source = readFileSync(file, 'utf8')
  const match = source.match(
    new RegExp(`const ${constName}:\\s*\\[&str;\\s*\\d+\\]\\s*=\\s*\\[([\\s\\S]*?)\\];`)
  )
  if (!match) throw new Error(`could not locate the ${constName} array literal in ${file}`)
  const [, entries] = match
  if (typeof entries !== 'string') throw new Error(`could not read the ${constName} array literal`)
  const parsed = quotedStrings(withoutLineComments(entries))
  if (parsed.length === 0) throw new Error(`${constName} parsed as empty — the reader is broken`)
  return parsed
}

function sorted(names: readonly string[]): string[] {
  return [...new Set(names)].sort()
}

/**
 * Every pair of lists that must agree. `rust` and `ts` are both thunks so a
 * parse failure surfaces inside the test that owns it, not at import time.
 */
const PARITY_PAIRS: {
  label: string
  rust: () => string[]
  ts: () => readonly string[]
  /** What to do when it goes red. */
  repair: string
}[] = [
  {
    label:
      'active-runtime: resource_catalog.rs ACTIVE_RUNTIME_TOOLS ↔ ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES',
    rust: () => rustToolArray(RESOURCE_CATALOG_RS, 'ACTIVE_RUNTIME_TOOLS'),
    ts: () => ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES,
    repair: 'port the change into ACTIVE_RUNTIME_TOOLS in resource_catalog.rs'
  },
  {
    label:
      'root-executive: resource_catalog.rs ROOT_EXECUTIVE_TOOLS ↔ ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES',
    rust: () => rustToolArray(RESOURCE_CATALOG_RS, 'ROOT_EXECUTIVE_TOOLS'),
    ts: () => ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES,
    repair: 'port the change into ROOT_EXECUTIVE_TOOLS in resource_catalog.rs'
  },
  {
    label: 'builtin: materialize/plan.rs BUILTIN_TOOLS ↔ CapabilityPolicy BUILTIN_TOOLS',
    rust: () => rustToolArray(MATERIALIZE_PLAN_RS, 'BUILTIN_TOOLS'),
    ts: () => BUILTIN_TOOLS,
    repair: 'port the change into BUILTIN_TOOLS in materialize/plan.rs'
  },
  {
    // THE PAIR THAT WAS MISSING, added by the B1 gate removal because that
    // commit is what first moved names ACROSS this boundary. Every other list
    // on both sides was pinned; the subtree pair was not, so three names could
    // have been added to the TypeScript catalog and left out of the Rust one
    // with nothing failing — and Pi filters the model toolset to the Rust
    // `--tools` allowlist, so the pane would simply never see them. That is
    // the #30 silent-strip class the rest of this table exists to prevent.
    label: 'subtree: resource_catalog.rs SUBTREE_TOOLS ↔ ORGANIZATION_SUBTREE_TOOL_NAMES',
    rust: () => rustToolArray(RESOURCE_CATALOG_RS, 'SUBTREE_TOOLS'),
    ts: () => ORGANIZATION_SUBTREE_TOOL_NAMES,
    repair:
      'port the change into SUBTREE_TOOLS in resource_catalog.rs — a name missing there is a verb the pane never sees, whatever the catalog says'
  },
  /* TOMBSTONE (chief-home-is-cwd §3/§4e): the `protected:` pair, over
   * `materialize/plan.rs`'s `ORGANIZATION_PROTECTED_TOOL_NAMES` ↔
   * active-runtime ∪ subtree ∪ manager. That Rust table was the validation
   * list for one rule — "a person-selected extension may not shadow an
   * organization tool" — and per-person extension selection is deleted. No
   * extension a person chose reaches a pane (the org extensions arrive as
   * fixed `--extension` argv), so nothing can shadow a name and the table has
   * no subject; it is deleted on the Rust side in the same change. The pairs
   * that remain are the ones that still decide what a pane sees: every
   * `resource_catalog.rs` grant list, plus the builtin floor. */
  {
    // chiefd's baseline grant is not a separate Rust constant: the baseline
    // surface IS `ACTIVE_RUNTIME_TOOLS`. It used to be that surface plus a
    // second `ORGANIZATION_RUNTIME_FENCED_TOOL_NAMES` list holding
    // `org_change_model`/`org_change_thinking`; both verbs and the list they
    // lived in are deleted, so the composition is one list again.
    label: 'baseline: resource_catalog.rs ACTIVE_RUNTIME_TOOLS ↔ ORGANIZATION_BASELINE_TOOL_NAMES',
    rust: () => rustToolArray(RESOURCE_CATALOG_RS, 'ACTIVE_RUNTIME_TOOLS'),
    ts: () => [...ORGANIZATION_BASELINE_TOOL_NAMES],
    repair: 'port the change into ACTIVE_RUNTIME_TOOLS in resource_catalog.rs'
  }
]

describe('Rust/TypeScript tool-list parity (every list that exists on both sides)', () => {
  for (const pair of PARITY_PAIRS) {
    describe(pair.label, () => {
      test('chiefd is missing no TypeScript tool (nothing silently stripped from the pane)', () => {
        const rust = new Set(pair.rust())
        expect(
          pair.ts().filter((tool) => !rust.has(tool)),
          `missing from the Rust side — ${pair.repair}`
        ).toEqual([])
      })

      test('chiefd carries no tool TypeScript does not (no over-grant)', () => {
        const ts = new Set<string>(pair.ts())
        expect(
          pair.rust().filter((tool) => !ts.has(tool)),
          `only on the Rust side — remove it there or add it to the TypeScript authority`
        ).toEqual([])
      })

      test('the two lists are exactly the same set', () => {
        expect(sorted(pair.rust()), pair.repair).toEqual(sorted(pair.ts()))
      })
    })
  }

  test('every parity pair reads a non-empty list from both sides (the readers themselves work)', () => {
    for (const pair of PARITY_PAIRS) {
      expect(pair.rust().length, `${pair.label}: Rust side parsed empty`).toBeGreaterThan(0)
      expect(pair.ts().length, `${pair.label}: TypeScript side is empty`).toBeGreaterThan(0)
    }
  })

  test('the person-seed schema and launcher policy use the same builtin vocabulary', () => {
    expect(
      EXTENSION_RUNTIME_BUILTIN_TOOLS,
      'port builtin changes into the copy-safe extension schema catalog'
    ).toEqual(BUILTIN_TOOLS)
  })
})
