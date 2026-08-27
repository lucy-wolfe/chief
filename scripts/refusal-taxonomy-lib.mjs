// The derivations behind `scripts/test/refusal-taxonomy.test.mjs`, kept in
// their own module so the guard can demonstrate red-then-green against
// SYNTHETIC fixtures instead of only asserting against the real tree.
//
// Three properties. The first two were violated in production before #1004:
//
//   1. **One status table.** No route module in `chiefd-api`'s docstore
//      surface may name an HTTP error status itself. `route_error.rs` owns the
//      whole mapping; a route that picks its own status is how a domain
//      refusal ends up answered 500. `runtime_routes.rs` ran every
//      `runtime_lifecycle::*` result through a local `internal()` that did
//      exactly that, for 24 routes.
//
//   2. **The two halves agree.** chiefd's `REFUSAL_STATUSES` and the
//      TypeScript client's `REFUSAL_STATUSES` must be the same set. They were
//      not: the client accepted `{400, 404, 422}` while the server answered
//      domain rules with 409 and 403, so those refusals reached the agent as
//      `chiefd unavailable (http-error)`.
//
//   3. **A lost CAS names itself.** Once 409 became a real taxonomy position,
//      "409" alone stopped identifying WHICH fence moved: a CAS route can
//      answer 409 for a fence that is not the caller's `expectedSeq`
//      (`/v1/org/supervision/publish-cas` runs the whole
//      `ingest_external_document` apply body, whose assignment fences raise
//      `fence-mismatch` / `seq-conflict`). The client therefore
//      discriminates on the body's `code`, and the literal it discriminates on
//      must be the literal every `*_publish_cas` in `writer.rs` actually
//      raises. Silent drift here reads a foreign fence as a stale sequence and
//      sends the caller around a retry loop a fresh read cannot satisfy.
//
// All three are text derivations over the real files. That is deliberate: the
// alternative — a runtime assertion — cannot see a route that has no test, and
// the routes that had no test are the ones that shipped the defect.

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

/** Where the docstore route modules live, relative to the repo root. */
export const DOCSTORE_DIR = join('apps', 'chiefd', 'crates', 'chiefd-api', 'src', 'docstore')

/** The one module allowed to name a status. */
export const TAXONOMY_FILE = 'route_error.rs'

/** The writer actor, which owns every compare-and-swap sequence check. */
export const CAS_WRITER_FILE = join(
  'apps',
  'chiefd',
  'crates',
  'chiefd-core',
  'src',
  'actor',
  'writer.rs'
)

/**
 * Statuses a route module may still name directly, because they are not error
 * classifications at all:
 *
 * * `OK` / `ACCEPTED` — success.
 * * `PAYLOAD_TOO_LARGE` — the body-limit middleware answers before any handler
 *   or taxonomy is reached; there is no `ChiefdError` to classify.
 *
 * Kept as a short, named list rather than a pattern, so adding to it is a
 * decision someone makes in a diff.
 */
export const NON_CLASSIFYING_STATUSES = new Set(['OK', 'ACCEPTED', 'PAYLOAD_TOO_LARGE'])

/**
 * Strip Rust `#[cfg(test)] mod ... { }` blocks.
 *
 * Tests legitimately name statuses — asserting that a route answered 404 is
 * the whole point of a route test. The property under test is about the
 * PRODUCTION path, so the guard reads only that half. Brace counting is enough
 * here because the marker is always at column 0 in these files; a `#[cfg(test)]`
 * nested inside another item would be missed, and that is stated rather than
 * papered over.
 */
export function stripTestModules(source) {
  let out = ''
  let index = 0
  for (;;) {
    const marker = source.indexOf('#[cfg(test)]', index)
    if (marker === -1) {
      out += source.slice(index)
      return out
    }
    out += source.slice(index, marker)
    const open = source.indexOf('{', marker)
    if (open === -1) return out
    let depth = 0
    let cursor = open
    for (; cursor < source.length; cursor += 1) {
      const char = source[cursor]
      if (char === '{') depth += 1
      else if (char === '}') {
        depth -= 1
        if (depth === 0) break
      }
    }
    index = cursor + 1
  }
}

/**
 * Every `StatusCode::<NAME>` a production line in `dir` names, excluding the
 * taxonomy module itself and excluding {@link NON_CLASSIFYING_STATUSES}.
 *
 * Returns `[{file, line, status, text}]` — every entry is a route deciding its
 * own status, which is the thing that must not exist.
 */
export function findRouteOwnedStatuses(dir, { taxonomyFile = TAXONOMY_FILE } = {}) {
  const findings = []
  for (const file of readdirSync(dir).filter((name) => name.endsWith('.rs')).sort()) {
    if (file === taxonomyFile) continue
    const production = stripTestModules(readFileSync(join(dir, file), 'utf8'))
    production.split('\n').forEach((text, index) => {
      const bare = text.trim()
      if (bare.startsWith('//') || bare.startsWith('///') || bare.startsWith('*')) return
      for (const match of text.matchAll(/StatusCode::([A-Z_]+)/g)) {
        if (NON_CLASSIFYING_STATUSES.has(match[1])) continue
        findings.push({ file, line: index + 1, status: match[1], text: bare })
      }
    })
  }
  return findings
}

/** The `.rs` files scanned, so an empty scan can be reported as a defect. */
export function scannedRustFiles(dir, { taxonomyFile = TAXONOMY_FILE } = {}) {
  return readdirSync(dir)
    .filter((name) => name.endsWith('.rs') && name !== taxonomyFile)
    .sort()
}

/** chiefd's `pub const REFUSAL_STATUSES: [u16; N] = [...]`. */
export function parseRustRefusalStatuses(source) {
  const match = /pub const REFUSAL_STATUSES: \[u16; (\d+)\] = \[([^\]]*)\];/.exec(source)
  if (!match) return undefined
  const statuses = match[2]
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map(Number)
  if (statuses.some(Number.isNaN)) return undefined
  // The declared length is part of the contract: a mismatch means someone
  // edited the list and not the type, which Rust would catch — asserting it
  // here keeps the parse honest rather than lenient.
  if (statuses.length !== Number(match[1])) return undefined
  return statuses
}

/** The client's `export const REFUSAL_STATUSES: readonly number[] = [...]`. */
export function parseTsRefusalStatuses(source) {
  const match = /export const REFUSAL_STATUSES: readonly number\[\] = \[([^\]]*)\]/.exec(source)
  if (!match) return undefined
  const statuses = match[1]
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map(Number)
  return statuses.some(Number.isNaN) ? undefined : statuses
}

/**
 * `{missingInClient, missingInServer}` — either being non-empty is the drift
 * this guard exists to name.
 */
export function compareRefusalSets(rust, ts) {
  const server = new Set(rust)
  const client = new Set(ts)
  return {
    missingInClient: [...server].filter((status) => !client.has(status)).sort(),
    missingInServer: [...client].filter((status) => !server.has(status)).sort(),
  }
}

/**
 * A refusal status must be a 4xx and a fault status must not be in the set.
 *
 * Stated as a property rather than a list, so the check survives a deliberate
 * future addition (say 405) without an edit, and still fails the one edit that
 * would reopen the defect: putting 500 or 503 in the refusal set, which is
 * "unavailable" and "refusal" collapsing back into each other from the other
 * direction.
 */
export function refusalSetShapeViolations(statuses) {
  return statuses.filter((status) => !(status >= 400 && status < 500))
}

/**
 * The client's `export const SEQ_CONFLICT_CODE = '...'`, or `undefined` when
 * the declaration is not in the shape this guard can trust.
 */
export function parseTsSeqConflictCode(source) {
  const match = /export const SEQ_CONFLICT_CODE = '([^']*)'/.exec(source)
  return match?.[1] || undefined
}

/**
 * Every `*_publish_cas` method in `writer.rs`, with the `ChiefdError::conflict`
 * codes its PRODUCTION body raises: `[{method, codes}]`, sorted by method.
 *
 * Derived rather than hand-listed for the usual reason — a sixth CAS method
 * added next year is exactly the one nobody would remember to add to a list,
 * and it is the one whose code the client would silently fail to recognise.
 *
 * A method's body runs to the next `pub`/`pub async` item; `///` lines are
 * dropped first, so a doc comment that merely NAMES a code (several do) is
 * never mistaken for a site that raises one.
 */
export function casConflictCodes(source) {
  const production = stripTestModules(source)
  const starts = [...production.matchAll(/pub async fn (\w+_publish_cas)\s*\(/g)]
  return starts
    .map((start, index) => {
      const from = start.index
      const to = index + 1 < starts.length ? starts[index + 1].index : production.length
      const body = production
        .slice(from, to)
        .split('\n')
        .filter((line) => !line.trim().startsWith('///'))
        .join('\n')
      const codes = [...body.matchAll(/ChiefdError::conflict\(\s*"([^"]*)"/g)].map(
        (match) => match[1]
      )
      return { method: start[1], codes: [...new Set(codes)].sort() }
    })
    .sort((left, right) => left.method.localeCompare(right.method))
}

/**
 * `{silent, foreign}` — the two ways the CAS code contract can break.
 *
 * * `silent` — a `*_publish_cas` that raises no conflict code at all. Its seq
 *   check is either gone or unreachable, and the client would never see a
 *   `SeqConflictError` for it.
 * * `foreign` — a `*_publish_cas` that raises a code the client does not
 *   recognise. That 409 reaches the caller as a plain refusal instead of the
 *   retryable conflict it is, so a lost race becomes a hard failure.
 *
 * Both are reported as `[{method, codes}]`, never as a bare boolean, because
 * the method name is the whole of the fix.
 */
export function casCodeDrift(methods, clientCode) {
  return {
    silent: methods.filter((entry) => entry.codes.length === 0),
    foreign: methods.filter(
      (entry) => entry.codes.length > 0 && !entry.codes.every((code) => code === clientCode)
    ),
  }
}
