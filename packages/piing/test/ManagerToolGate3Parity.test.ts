/**
 * THIRD-gate drift guard (#30 class): an org tool is usable on the live daemon
 * only if all THREE gates grant it — the extension REGISTERS it, TS `planPerson`
 * GRANTs it (`ORGANIZATION_MANAGER_TOOL_NAMES` → `person.tools`), AND chiefd's
 * actuator re-derives + GRANTs it (`MANAGER_TOOLS` in
 * `chiefd-host/converge_apply/resource_catalog.rs` → the pane `--tools`
 * allowlist Pi filters the model toolset to). The two lists are hand-maintained
 * parity, and they DRIFTED once (Rust had 27 vs the TS 30, silently stripping
 * org_reparent_department / org_appoint_department_head — the restructure
 * keystones — from every converged manager).
 *
 * This reconciliation test makes the whole drift class fail loudly: any manager
 * tool ADDED to the TS list that is not also ported into the Rust `MANAGER_TOOLS`
 * (or removed from one side only) fails here, in the in-repo TS suite, long
 * before a live deploy silently withholds it. It reads the Rust source as text
 * (no cargo, no cross-crate build) — the TS suite always runs in-repo with both
 * files present.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { ORGANIZATION_MANAGER_TOOL_NAMES } from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

const RESOURCE_CATALOG_RS = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  '..',
  'apps/chiefd/crates/chiefd-host/src/converge_apply/resource_catalog.rs'
)

/** Parse the `const MANAGER_TOOLS: [&str; N] = [ "a", "b", … ];` array literal
 * out of the Rust source and return its string entries. */
function rustManagerTools(): string[] {
  const source = readFileSync(RESOURCE_CATALOG_RS, 'utf8')
  const match = source.match(/const MANAGER_TOOLS:\s*\[&str;\s*\d+\]\s*=\s*\[([\s\S]*?)\];/)
  if (!match)
    throw new Error('could not locate the MANAGER_TOOLS array literal in resource_catalog.rs')
  // Strip `//` line comments first, so a quoted string that appears INSIDE a
  // comment (e.g. `// … "Operator ruling 2026-07-24" …`) is never mistaken for
  // a list entry.
  const [, entries] = match
  if (typeof entries !== 'string') throw new Error('could not read the MANAGER_TOOLS array literal')
  const withoutComments = entries.replace(/\/\/[^\n]*/g, '')
  return [...withoutComments.matchAll(/"([^"]+)"/g)].map((entry) => {
    const [, tool] = entry
    if (typeof tool !== 'string') throw new Error('could not read a MANAGER_TOOLS entry')
    return tool
  })
}

describe('#30 gate-3 parity: chiefd MANAGER_TOOLS ⊇ TS ORGANIZATION_MANAGER_TOOL_NAMES', () => {
  test('every TS manager tool is re-derived by chiefd — no future TS addition can be silently stripped by gate 3', () => {
    const rust = new Set(rustManagerTools())
    const missingFromRust = ORGANIZATION_MANAGER_TOOL_NAMES.filter((tool) => !rust.has(tool))
    // If this fails: port the listed tool(s) into MANAGER_TOOLS in resource_catalog.rs.
    expect(missingFromRust).toEqual([])
  })

  test('chiefd MANAGER_TOOLS grants no manager tool the TS list does not (no over-grant)', () => {
    const ts = new Set<string>(ORGANIZATION_MANAGER_TOOL_NAMES)
    const extraInRust = rustManagerTools().filter((tool) => !ts.has(tool))
    // If this fails: remove the Rust-only tool or add it to the TS authority.
    expect(extraInRust).toEqual([])
  })

  test('the two manager-tool lists are exactly the same set (guards the count comment too)', () => {
    expect([...new Set(rustManagerTools())].sort()).toEqual(
      [...ORGANIZATION_MANAGER_TOOL_NAMES].sort()
    )
  })
})
