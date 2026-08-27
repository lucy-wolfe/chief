// #875: pure parsing/diffing logic for the Rust-store-struct <-> chiefing
// TS-type shape drift check. The join: a row document's field set is stated
// twice — once as a `#[derive(Serialize, Deserialize)]` struct under
// `chiefd-core/src/store/`, once as a hand-written TypeScript interface in
// `packages/chiefing/src/types/{RowDocs,PersonContracts,OrgDocs}.ts`. `cargo`
// reads the Rust and cannot see the TypeScript; `tsc` reads the TypeScript
// and believes it. Nothing today compares the two shapes directly — #844's
// `serde_flatten_catchall_conformance.rs` checks Rust-side self-consistency
// (every flatten catch-all has its write-time guard) but never reads a TS
// file, so a NEW field added to a Rust struct that the TS side never picks
// up passes every check the workspace runs today.
//
// This module reads the REAL Rust struct source (regex-based field/attribute
// extraction, matching the codebase's existing source-scanning convention —
// port_provenance.rs, fence_containment.rs, serde_flatten_catchall_conformance.rs
// all parse source text rather than compiling an AST) and the REAL TypeScript
// interface (via the `typescript` compiler API, already a devDependency, for
// a robust parse rather than a second regex reading a second language) —
// never a hand-maintained transcription of either shape. A transcription
// cannot be the source of truth for a test about transcription errors.

import ts from 'typescript'

// ---- Rust struct field extraction ----------------------------------------

const SNAKE_WORD = /_([a-z0-9])/g

/** `cycle_started_at_ms` -> `cycleStartedAtMs`. Rust's own idiomatic snake_case
 * name IS the correct camelCase wire name whenever there is no explicit
 * `#[serde(rename = "...")]` override and no ancestor exists to consult (a
 * single-word field is already correct either way, e.g. `token` -> `token`). */
export function snakeToCamel(name) {
  return name.replace(SNAKE_WORD, (_, c) => c.toUpperCase())
}

/**
 * Extract the `{ ... }` body of `pub struct <structName> { ... }` (or
 * `struct <structName> { ... }`) from Rust source, brace-depth-aware so
 * nested generic angle brackets (`BTreeMap<String, Value>`) and nested
 * braces in doc comments/attributes do not truncate the scan early. Returns
 * null if the struct is not found, is a tuple struct (`struct Foo(...)`),// or is a unit struct (`struct Foo;`) — none of which this check applies to.
 */
/**
 * Rust's visibility clause, as a regex fragment: absent, `pub`, or `pub` with
 * a scope (`pub(crate)`, `pub(super)`, `pub(in a::b)`).
 *
 * Written once and shared by the struct header and the field line because they
 * had the same bug independently — both matched the literal `pub `, so a
 * `pub(crate) struct` was "not found" and a `pub(crate)` field parsed to
 * nothing. One definition means the next visibility form is fixed in one place
 * rather than in whichever of the two somebody happens to hit first.
 */
const VISIBILITY = '(?:pub\\s*(?:\\([^)]*\\))?\\s+)?'

export function extractRustStructBody(source, structName) {
  const header = new RegExp(`(?:^|\\n)\\s*${VISIBILITY}struct ${structName}\\s*\\{`)
  const match = header.exec(source)
  if (!match) return null
  const braceStart = match.index + match[0].length - 1
  let depth = 0
  for (let i = braceStart; i < source.length; i += 1) {
    const ch = source[i]
    if (ch === '{') depth += 1
    else if (ch === '}') {
      depth -= 1
      if (depth === 0) return source.slice(braceStart + 1, i)
    }
  }
  throw new Error(`unbalanced braces reading struct ${structName}`)
}

/**
 * Split a struct body into per-field chunks (attributes/doc-comments +
 * `field_name: Type,`), splitting on top-level (depth-0) commas only — a
 * field's own type may itself contain commas (`Record<string, T>`,
 * `BTreeMap<String, Value>`).
 *
 * Doc-comment lines (`///`/`//!`/`//`) are stripped FIRST, whole-line, before
 * any bracket-depth counting — a doc comment's own prose regularly contains
 * bare `(`/`)`/`<`/`>` with no bearing on struct shape (e.g. "Runtime
 * a bracketed range in prose), and counting those toward depth alongside the real
 * generic/paren nesting corrupts the running depth for everything that
 * follows in the same struct. Comments are never part of the wire shape, so
 * dropping them first is always safe, never lossy for this purpose.
 */
function stripCommentLines(body) {
  return body
    .split('\n')
    .filter((line) => !/^\s*\/\//.test(line))
    .join('\n')
}

function splitFields(rawBody) {
  const body = stripCommentLines(rawBody)
  const chunks = []
  let depth = 0
  let start = 0
  for (let i = 0; i < body.length; i += 1) {
    const ch = body[i]
    if (ch === '<' || ch === '(' || ch === '{' || ch === '[') depth += 1
    else if (ch === '>' || ch === ')' || ch === '}' || ch === ']') depth -= 1
    else if (ch === ',' && depth === 0) {
      chunks.push(body.slice(start, i))
      start = i + 1
    }
  }
  const tail = body.slice(start)
  if (tail.trim()) chunks.push(tail)
  return chunks
}

/**
 * Parse every field of a Rust struct body into `{ rustName, wireName,
 * optional, flatten, skipped }`. `structLevelRenameAll` is `true` when the
 * struct carries `#[serde(rename_all = "camelCase")]` — every field with no
 * explicit per-field `#[serde(rename = "...")]` then uses `snakeToCamel`;
 * without it, an unrenamed field is used AS-IS (the codebase's other
 * convention: explicit per-field rename on every multi-word field, e.g.
 * `goal_delivery_quiesce_rows.rs`'s `GoalDeliveryQuiesce`) — so `wireName`
 * always resolves to
 * the actual wire name, whichever convention the struct uses, and a field
 * that is neither renamed nor plausibly already camelCase (i.e. its
 * snake_to_camel differs from itself) still gets the conservative
 * snake-to-camel treatment, since a bare unrenamed multi-word field with no
 * `rename_all` is far more likely a missed rename than an intentional
 * snake_case wire name — flagging that possibility is exactly this check's
 * job, not a reason to suppress it.
 */
/**
 * One field declaration line: optional visibility, optional raw-identifier
 * escape, the name, a colon.
 *
 * # Why the visibility clause is not just `pub`
 *
 * It was `(?:pub\s+)?`, which sees `pub name:` and a bare private `name:` and
 * NOTHING else. A struct whose fields are `pub(crate)` — every field of it —
 * parsed to the empty field list, and an empty field list DIFFS CLEAN against
 * any TypeScript interface at all. So the failure mode was not a missed field,
 * it was a pair that passes while comparing nothing: a green check that proves
 * nothing, which is worse than the absent check it replaces.
 *
 * Nothing was silently green when this was written — all 30 declared pairs
 * parse non-zero on both sides — because every struct they name happens to use
 * bare `pub`. The trap was waiting for the first `pub(crate)` pair somebody
 * added, which is exactly what `company-tree-person` is.
 *
 * `[non_exhaustive_visibility]` covers `pub(crate)`, `pub(super)` and
 * `pub(in path::to)` in one clause rather than three alternatives, because the
 * grammar is "pub, optionally scoped", and enumerating the scopes is how you
 * miss the fourth one. `r#` is stripped so a raw identifier reports the name
 * serde actually writes.
 */
const FIELD_DECLARATION = new RegExp(`(?:^|\\n)\\s*${VISIBILITY}(?:r#)?([a-z_][a-z0-9_]*)\\s*:`, 'i')

export function parseRustFields(structBody, structLevelRenameAll) {
  const fields = []
  for (const rawChunk of splitFields(structBody)) {
    const chunk = rawChunk.trim()
    if (!chunk) continue
    const fieldMatch = FIELD_DECLARATION.exec(chunk)
    if (!fieldMatch) continue // a lone attribute/comment fragment, not a field line
    const rustName = fieldMatch[1]
    const attrs = chunk.slice(0, fieldMatch.index)
    const typeMatch = /:\s*([^,]+)$/s.exec(chunk)
    const type = typeMatch ? typeMatch[1].trim() : ''

    const flatten = /#\[serde\([^)]*\bflatten\b[^)]*\)\]/.test(attrs)
    const skipped = /#\[serde\([^)]*\bskip\b(?!_serializing_if)[^)]*\)\]/.test(attrs)
    const renameMatch = /#\[serde\([^)]*\brename\s*=\s*"([^"]+)"[^)]*\)\]/.exec(attrs)
    const optionalType = /^Option\s*</.test(type)
    const skipIfNone = /skip_serializing_if\s*=\s*"Option::is_none"/.test(attrs)

    const wireName = renameMatch
      ? renameMatch[1]
      : structLevelRenameAll
        ? snakeToCamel(rustName)
        : snakeToCamel(rustName) // see doc comment: conservative even without rename_all

    fields.push({
      rustName,
      wireName,
      optional: optionalType || skipIfNone,
      flatten,
      skipped,
    })
  }
  return fields
}

/** Whether a struct's `#[derive(...)]`/`#[serde(...)]` block immediately
 * preceding `pub struct <structName>` carries `rename_all = "camelCase"`. */
export function structHasRenameAll(source, structName) {
  const header = new RegExp(
    `((?:#\\[[^\\]]*\\]\\s*\\n)*)\\s*${VISIBILITY}struct ${structName}\\s*\\{`,
  )
  const match = header.exec(source)
  if (!match) return false
  return /rename_all\s*=\s*"camelCase"/.test(match[1])
}

/** The wire field-name set for a named Rust struct in `source` — excludes
 * `flatten`/`skip` fields (a flatten catch-all is #844's concern, a skipped
 * field is never on the wire either direction). Throws if the struct is not
 * found (a stale pair entry is a bug in the pair table, not a silent skip). */
export function rustWireFields(source, structName) {
  const body = extractRustStructBody(source, structName)
  if (body === null) {
    throw new Error(`struct '${structName}' not found (pair table entry is stale?)`)
  }
  const renameAll = structHasRenameAll(source, structName)
  return parseRustFields(body, renameAll)
    .filter((f) => !f.flatten && !f.skipped)
    .map((f) => ({ name: f.wireName, optional: f.optional }))
}

// ---- TypeScript interface field extraction --------------------------------

function findInterface(sourceFile, interfaceName) {
  // Annotated: assignment happens inside the `visit` closure, which TypeScript's
  // evolving-`let` inference does not follow, so an unannotated `found` stays
  // `undefined` and every caller's `found.members` read is against `never`.
  /** @type {import('typescript').InterfaceDeclaration | undefined} */
  let found
  const visit = (node) => {
    if (ts.isInterfaceDeclaration(node) && node.name.text === interfaceName) {
      found = node
      return
    }
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  return found
}

function membersToFields(members, sourceFile) {
  return members
    .filter((member) => ts.isPropertySignature(member) && member.name)
    .map((member) => ({
      name: member.name.getText(sourceFile),
      optional: Boolean(member.questionToken),
    }))
}

/** The top-level property-key set for a named TS interface in `source`, via
 * the real TypeScript compiler API (never a second regex reading a second
 * language). Throws if the interface is not found. */
export function tsInterfaceFields(source, interfaceName) {
  const sourceFile = ts.createSourceFile('shape-drift-check.ts', source, ts.ScriptTarget.Latest, true)
  const found = findInterface(sourceFile, interfaceName)
  if (!found) {
    throw new Error(`TS interface '${interfaceName}' not found (pair table entry is stale?)`)
  }
  return membersToFields(found.members, sourceFile)
}

/**
 * The property-key set of a NESTED inline object-literal type — a property
 * of `interfaceName` whose own type is a `{ ... }` type literal (optionally
 * wrapped in `T | undefined` via `?`), rather than a separately-named
 * interface. Used for the small number of Rust sub-structs
 * (`PendingDoorbell`, `RefusalRecord`) whose chiefing counterpart was never
 * pulled out into its own named interface. Throws if the interface, the
 * property, or a type-literal shape for that property is not found — a
 * silent empty result here would be worse than no check at all (#873's
 * "false positives buried in true ones" lesson, mirrored: a false NEGATIVE
 * from silently returning `[]` is the same failure in the other direction).
 */
export function tsNestedInterfaceFields(source, interfaceName, propertyName) {
  const sourceFile = ts.createSourceFile('shape-drift-check.ts', source, ts.ScriptTarget.Latest, true)
  const found = findInterface(sourceFile, interfaceName)
  if (!found) {
    throw new Error(`TS interface '${interfaceName}' not found (pair table entry is stale?)`)
  }
  const property = found.members.find(
    (member) =>
      ts.isPropertySignature(member) && member.name && member.name.getText(sourceFile) === propertyName,
  )
  // `find` does not narrow, so `property.type` below was being read off the
  // whole `TypeElement` union; re-check the same predicate the find used.
  if (!property || !ts.isPropertySignature(property)) {
    throw new Error(`TS interface '${interfaceName}' has no property '${propertyName}'`)
  }
  let typeNode = property.type
  // `T | undefined` (rare) or a bare optional `T?` — unwrap to the literal.
  if (typeNode && ts.isUnionTypeNode(typeNode)) {
    typeNode = typeNode.types.find((t) => ts.isTypeLiteralNode(t))
  }
  if (!typeNode || !ts.isTypeLiteralNode(typeNode)) {
    throw new Error(
      `TS interface '${interfaceName}'.'${propertyName}' is not an inline object-literal type (pair table entry needs a different extractor)`,
    )
  }
  return membersToFields(typeNode.members, sourceFile)
}

// ---- Diff ------------------------------------------------------------------

/**
 * Compare a Rust struct's wire fields against a TS interface's declared
 * keys. Two DIFFERENT directions, reported separately, because they have
 * different consequences (per #875's own framing):
 *   - `rustOnly`: chiefd sends this field; the TS type never declares it.
 *     DATA-LOSS direction — a caller reading through this type silently
 *     drops the value (or, worse, a caller that re-serializes the parsed
 *     object and republishes it silently omits it).
 *   - `tsOnly`: the TS type declares a field chiefd's struct never sends.
 *     THROWS-AT-BOUNDARY direction (for a required field) — a strict
 *     consumer of the type expects a value that is never actually present,
 *     which surfaces as `undefined` reaching code that assumed a value, or
 *     (with `noUncheckedIndexedAccess`/exactOptionalPropertyTypes-style
 *     discipline) a type error at the point of use once the mismatch is
 *     modeled correctly.
 * An optional Rust field absent from TS, or a TS-optional field absent from
 * Rust, is still reported — optionality does not excuse a field the two
 * sides disagree exists at all; it only changes how badly a value's
 * ABSENCE (as opposed to the field's existence) is tolerated at runtime.
 */
export function diffShapes(rustFields, tsFields) {
  const rustNames = new Set(rustFields.map((f) => f.name))
  const tsNames = new Set(tsFields.map((f) => f.name))
  const rustOnly = rustFields.filter((f) => !tsNames.has(f.name))
  const tsOnly = tsFields.filter((f) => !rustNames.has(f.name))
  return { rustOnly, tsOnly, drifted: rustOnly.length > 0 || tsOnly.length > 0 }
}
