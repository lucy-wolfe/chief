#!/usr/bin/env bun
/**
 * #859: a scaffolded module's throwing stub is type-identical to a real
 * implementation. Repointing an import at one typechecks, lints, passes
 * knip, and passes vitest -- it fails only when the code path is actually
 * exercised, which for a Pi-home materialization symbol means "a live
 * company boots". Demonstrated during #792: an engineer folded seven
 * `IdentityTheme` stub symbols into a bulk repoint alongside genuinely-real
 * modules and self-caught it only by chance, investigating something
 * unrelated (reverted in e4a5a072).
 *
 * The stubbed set is DERIVED from source, never hand-maintained -- a list
 * would itself be a citation that goes stale the moment a story implements
 * one. When E3-S5/E3-S6/E2-S5/E2-S6 land, their symbols leave the derived
 * set automatically and this guard stops flagging them; no allowlist edit
 * required.
 *
 * INDIRECTION MATTERS: some stub modules throw a string literal inline
 * (`throw new Error('not implemented: ...')`); others hoist the message
 * into a module-level `const NOT_IMPLEMENTED = '...'` and throw the
 * identifier. A scan that only matches the literal shape misses the second
 * class entirely -- exactly the blind spot the merger's first version had,
 * which under-counted the true stub surface (26 reported vs 31 actual)
 * because five `packages/chiefing` modules use the hoisted-constant shape.
 * This scanner resolves hoisted constants first, then checks throw sites
 * against BOTH the literal pattern and any resolved identifier.
 *
 * SYMBOL GRANULARITY: the unit that matters is whatever a caller can
 * `import { X } from '@chief/*'` -- for piing's stub modules that is one
 * standalone function per stub; for chiefing's stub modules the throwing
 * methods live inside an exported CLASS, so the importable symbol is the
 * class name, not each method. A class counts as a stub symbol when it has
 * at least one method and EVERY method body is stub-only; a class with one
 * real method (chiefing has none, but the rule must not silently treat a
 * partially-real class as fully safe OR fully stub) is reported separately
 * so a human decides, rather than the guard picking a side.
 */
import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { extname, join, relative } from 'node:path'

const NOT_IMPLEMENTED_RE = /not implemented/i

function walkTs(dir) {
  const out = []
  let entries
  try {
    entries = readdirSync(dir)
  } catch {
    return out
  }
  for (const entry of entries) {
    if (entry === 'node_modules' || entry === '.git' || entry === 'dist' || entry === 'coverage') continue
    const full = join(dir, entry)
    const st = statSync(full)
    if (st.isDirectory()) out.push(...walkTs(full))
    else if (extname(full) === '.ts' && !entry.endsWith('.test.ts')) out.push(full)
  }
  return out
}

/** Matching closing brace for the `{` at `openIndex`, honouring string/
 * template/comment content so a `{`/`}` inside a string never desyncs the
 * depth counter. */
function matchBrace(text, openIndex) {
  let depth = 0
  let i = openIndex
  while (i < text.length) {
    const ch = text[i]
    if (ch === '{') depth++
    else if (ch === '}') {
      depth--
      if (depth === 0) return i
    } else if (ch === "'" || ch === '"' || ch === '`') {
      const quote = ch
      i++
      while (i < text.length && text[i] !== quote) {
        if (text[i] === '\\') i++
        i++
      }
    } else if (ch === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++
    } else if (ch === '/' && text[i + 1] === '*') {
      i += 2
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++
      i++
    }
    i++
  }
  return -1
}

/** Matching closing paren for the `(` at `openIndex`, same string/comment
 * awareness as `matchBrace`. Used to find the end of a parameter list even
 * when a parameter's own type annotation contains parens. */
function matchParen(text, openIndex) {
  let depth = 0
  let i = openIndex
  while (i < text.length) {
    const ch = text[i]
    if (ch === '(') depth++
    else if (ch === ')') {
      depth--
      if (depth === 0) return i
    } else if (ch === "'" || ch === '"' || ch === '`') {
      const quote = ch
      i++
      while (i < text.length && text[i] !== quote) {
        if (text[i] === '\\') i++
        i++
      }
    } else if (ch === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++
    } else if (ch === '/' && text[i + 1] === '*') {
      i += 2
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++
      i++
    }
    i++
  }
  return -1
}

/**
 * From just after a function's parameter-list close paren, find the index
 * of the function BODY's opening brace -- skipping an optional return-type
 * annotation, including one shaped as an object type literal (which has its
 * own `{...}` group that is not the body).
 *
 * Grammar handled: `: Type` (no braces, e.g. `: string`), `: { ... }` (an
 * object type, possibly nested/generic), `: Type<{ ... }>` and similar.
 * Approach: track combined bracket depth across `(`, `[`, `{`, `<` from
 * just after the return-type colon; the FIRST `{` seen while that combined
 * depth is exactly 0 begins a group. If that group, once brace-matched, is
 * immediately followed (skipping whitespace) by another `{`, the first
 * group was the return type and the second is the body. If a matched group
 * is followed by anything else meaningful before another `{`, keep
 * scanning -- this only needs to be correct for the shapes real TypeScript
 * signatures use, not arbitrary input.
 */
function findFunctionBodyBrace(text, fromIndex) {
  let i = fromIndex
  while (i < text.length && /\s/.test(text[i])) i++
  if (text[i] !== ':') {
    // No return-type annotation: the very next `{` is the body.
    return text.indexOf('{', i)
  }
  i++ // past ':'
  // Walk the return-type annotation token by token, brace-matching any
  // object-literal groups we encounter, until we reach a `{` that is NOT
  // immediately followed (after matching it) by more return-type content --
  // i.e. the next non-whitespace character after its own closing `}` is
  // itself `{` or nothing else type-shaped remains before the body.
  while (i < text.length) {
    while (i < text.length && /\s/.test(text[i])) i++
    if (text[i] === '{') {
      const close = matchBrace(text, i)
      if (close === -1) return -1
      let j = close + 1
      while (j < text.length && /\s/.test(text[j])) j++
      if (text[j] === '{') return j // that was the return type; this is the body.
      // Otherwise this `{` WAS the body (a bare object-shaped return type
      // with nothing else following is not valid TS for a function
      // declaration, so reaching here with non-`{` next means our own
      // group was in fact the body all along).
      return i
    }
    if (text[i] === '<' || text[i] === '(' || text[i] === '[') {
      const closer = { '<': '>', '(': ')', '[': ']' }[text[i]]
      let depth = 1
      i++
      while (i < text.length && depth > 0) {
        if (text[i] === '<' && closer === '>') depth++
        else if (text[i] === '(' && closer === ')') depth++
        else if (text[i] === '[' && closer === ']') depth++
        else if (text[i] === closer) depth--
        i++
      }
      continue
    }
    i++
  }
  return -1
}

function stripComments(body) {
  return body
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/.*$/gm, '')
}

/** True iff `body`'s only executable content is one or more throw
 * statements whose message is the literal `not implemented...` string, or
 * a reference to any of `notImplIdentifiers` (module-level hoisted
 * constants already confirmed to hold that literal). */
function isStubOnlyBody(body, notImplIdentifiers) {
  const stripped = stripComments(body).trim()
  if (!stripped) return false
  const identAlt = notImplIdentifiers.length ? notImplIdentifiers.map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|') : '(?!)'
  const throwStatement = new RegExp(
    `throw\\s+new\\s+Error\\s*\\(\\s*(?:${identAlt}|(['"\`])not implemented[\\s\\S]*?\\1)\\s*\\)\\s*;?`,
    'gi'
  )
  const withoutThrows = stripped.replace(throwStatement, '').trim()
  const hadAtLeastOneThrow = throwStatement.test(stripped)
  return hadAtLeastOneThrow && withoutThrows.length === 0
}

/** A method body that provides NO capability at all -- either it throws
 * (see `isStubOnlyBody`) or it is empty (a no-op constructor whose real
 * work is TS parameter-property auto-assignment, which this scanner does
 * not need to understand: an empty body grants no capability either way).
 * Used only for classifying a CLASS as fully-stub -- an empty constructor
 * must not by itself make an otherwise fully-throwing class read as
 * "partially real". */
function isNonFunctionalBody(body, notImplIdentifiers) {
  if (isStubOnlyBody(body, notImplIdentifiers)) return true
  return stripComments(body).trim().length === 0
}

/** Collects `const NAME = 'not implemented...'` / `const NAME = "..."`
 * (single- or multi-statement concatenation via `+`) hoisted constants so
 * throw sites that reference the identifier are recognised too. */
function collectNotImplementedIdentifiers(source) {
  const identifiers = []
  const constRe = /const\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*((?:['"`][\s\S]*?['"`]\s*\+?\s*)+)/g
  let match
  while ((match = constRe.exec(source))) {
    const [, name, value] = match
    if (NOT_IMPLEMENTED_RE.test(value)) identifiers.push(name)
  }
  return identifiers
}

/** Extract every method name + body from a class body (the text strictly
 * between the class's outer braces). Constructor and static members are
 * included -- any of them can be the one thing that makes a class usable. */
function classMethods(classBody) {
  const methods = []
  // Signature-only match, same reasoning as the top-level function scanner:
  // a method's return type can itself be object-shaped (or a generic
  // wrapping one, e.g. `Promise<{ applied: true }>`), so the body brace is
  // located via findFunctionBodyBrace rather than a single regex.
  const methodStart = /(?:^|\n)\s*(?:public\s+|private\s+|protected\s+|static\s+|async\s+|readonly\s+)*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(/g
  let match
  while ((match = methodStart.exec(classBody))) {
    const parenOpen = match.index + match[0].length - 1
    const parenClose = matchParen(classBody, parenOpen)
    if (parenClose === -1) continue
    const openBrace = findFunctionBodyBrace(classBody, parenClose + 1)
    if (openBrace === -1) continue
    const close = matchBrace(classBody, openBrace)
    if (close === -1) continue
    methods.push({ name: match[1], body: classBody.slice(openBrace + 1, close) })
    methodStart.lastIndex = close + 1
  }
  return methods
}

/**
 * Scan one file for exported symbols (`export function NAME` / `export
 * class NAME`) and classify each as a stub (entirely throw-bodied),
 * partially-stub (a class with a mix of stub and real methods), or real.
 * Returns only stub + partially-stub findings -- real exports are not
 * interesting to this guard.
 */
export function scanFileForStubs(filePath, source) {
  const identifiers = collectNotImplementedIdentifiers(source)
  const findings = []

  // Signature-only match (name + open paren) -- the body's opening brace is
  // located by scanning forward with findFunctionBodyBrace, which is aware
  // that a return-type annotation can itself be an object literal type
  // (`): { legacy: string }  {` -- two separate brace groups back to back).
  // A regex alone cannot distinguish "return type's own `{`" from "body's
  // `{`" when the return type starts with one; the earlier version of this
  // scanner silently skipped every such function (IdentityTheme.ts's
  // `organizationPersonThemeFileNames` among them) because its regex
  // treated the return type's opening brace as the body's.
  const fnSigRe = /export\s+(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^>]*>)?\s*\(/g
  let match
  while ((match = fnSigRe.exec(source))) {
    const parenOpen = match.index + match[0].length - 1
    const parenClose = matchParen(source, parenOpen)
    if (parenClose === -1) continue
    const openBrace = findFunctionBodyBrace(source, parenClose + 1)
    if (openBrace === -1) continue
    const close = matchBrace(source, openBrace)
    if (close === -1) continue
    const body = source.slice(openBrace + 1, close)
    if (isStubOnlyBody(body, identifiers)) {
      findings.push({ file: filePath, symbol: match[1], kind: 'function', status: 'stub' })
    }
  }

  const classRe = /export\s+class\s+([A-Za-z_$][A-Za-z0-9_$]*)[^{]*\{/g
  while ((match = classRe.exec(source))) {
    const openBrace = source.indexOf('{', match.index + match[0].length - 1)
    const close = matchBrace(source, openBrace)
    if (close === -1) continue
    const body = source.slice(openBrace + 1, close)
    const methods = classMethods(body)
    if (methods.length === 0) continue
    const stubMethods = methods.filter((m) => isStubOnlyBody(m.body, identifiers))
    const nonFunctionalMethods = methods.filter((m) => isNonFunctionalBody(m.body, identifiers))
    if (stubMethods.length === 0) continue // no throw at all -- not this guard's concern
    if (nonFunctionalMethods.length === methods.length) {
      // Every method either throws or grants no capability (e.g. an empty
      // parameter-property constructor) -- the class is fully unusable.
      findings.push({ file: filePath, symbol: match[1], kind: 'class', status: 'stub' })
    } else {
      findings.push({
        file: filePath,
        symbol: match[1],
        kind: 'class',
        status: 'partially-stub',
        stubMethods: stubMethods.map((m) => m.name),
        realMethods: methods.filter((m) => !nonFunctionalMethods.includes(m)).map((m) => m.name)
      })
    }
  }

  return findings
}

/**
 * The derived stub inventory for the whole `packages/*` tree.
 *
 * #915: `walkTs` swallows `readdirSync` failures (see its own `catch`) so a
 * `packagesDir` that does not exist -- a moved or renamed `packages/` root,
 * the same shape #787/#785 have already produced twice in this repo --
 * would otherwise silently resolve to zero files, which resolves to zero
 * stubs, which is indistinguishable from a genuinely clean tree at the
 * pinned terminal 0/0 assertion in scripts/test/stub-import-guard.test.mjs.
 * That is exactly the #848 class every sibling guard's header warns about
 * (sql-only-state.test.mjs asserts its writer roots non-empty for the
 * identical reason). Refuse before scanning, and print what the scan is
 * about to operate on before any verdict is possible -- the subject, not
 * just the outcome.
 */
export function deriveStubInventory(root) {
  const packagesDir = join(root, 'packages')
  if (!existsSync(packagesDir)) {
    throw new Error(
      `[stub-import-guard] REFUSING TO RUN -- scan root does not exist: ${packagesDir}. ` +
        'The packages/ tree may have moved or been renamed; a "0 stubs" result from a scan ' +
        'root that resolves to nothing is not a clean tree, it is an unrun check (#915).'
    )
  }
  const files = walkTs(packagesDir)
  if (files.length === 0) {
    throw new Error(
      `[stub-import-guard] REFUSING TO RUN -- 0 .ts files found under ${packagesDir}. ` +
        'A scan root that exists but enumerates nothing must not report a clean result (#915, #848).'
    )
  }
  console.error(`[stub-import-guard] scanning ${packagesDir}: ${files.length} .ts file(s)`)
  const findings = []
  for (const file of files) {
    const source = readFileSync(file, 'utf8')
    if (!NOT_IMPLEMENTED_RE.test(source)) continue
    for (const finding of scanFileForStubs(file, source)) {
      findings.push({ ...finding, file: relative(root, finding.file) })
    }
  }
  return findings
}

/** Named imports of `@chief/*` packages in a source file: { pkg, names[] }. */
export function extractChiefImports(source) {
  const results = []
  const importRe = /import\s+(?:type\s+)?\{([^}]*)\}\s*from\s*['"](@chief\/[a-zA-Z0-9_-]+)['"]/g
  let match
  while ((match = importRe.exec(source))) {
    const names = match[1]
      .split(',')
      .map((n) => n.trim())
      .filter(Boolean)
      .map((n) => n.replace(/^type\s+/, '').split(/\s+as\s+/)[0].trim())
    results.push({ pkg: match[2], names })
  }
  return results
}

function packageNameFor(root, file) {
  // packages/<name>/src/... -> @chief/<name>
  const parts = file.split('/')
  const idx = parts.indexOf('packages')
  if (idx === -1 || !parts[idx + 1]) return undefined
  return `@chief/${parts[idx + 1]}`
}

// #914: Node's execFileSync defaults to a 1MB stdout buffer. A packet-owned
// CARGO_TARGET_DIR nested inside the git working tree (the remote-packet
// protocol's own layout, before ENGINEER-BRIEF section 0.2 was patched to
// require a sibling dir) makes `git ls-files --others` return tens of
// thousands of untracked build-artifact paths -- 13,531 in the instance
// that surfaced this. That overflow threw a bare, unattributed `ENOBUFS`
// partway through main(), which read to a caller as `test:stub-import-guard`
// itself failing on a stub-import violation rather than on an
// infrastructure problem unrelated to its subject.
//
// A generous explicit maxBuffer (64 MiB -- comfortably above any untracked
// set this repo has produced, including the 13,531-file incident) makes the
// overflow far less likely to recur in practice. But a bound that can still
// be exceeded must fail with a NAMED reason, not an opaque runtime error:
// every git invocation below is wrapped so ANY failure -- overflow, git
// missing, a bad cwd -- surfaces as an attributed refusal naming the exact
// command that failed, never a silently truncated or partial file list.
// None of `diff --cached`/`diff`/`ls-files --others` legitimately exits
// non-zero in normal operation (unlike `git grep`'s exit-1-for-no-match
// convention elsewhere in this repo's guards), so there is no "expected
// failure" case to special-case out here -- any thrown error IS an
// infrastructure failure. `maxBuffer` is an optional override so a test can
// reproduce the overflow cheaply without generating megabytes of fixture
// files (see scripts/test/stub-import-guard.test.mjs).
const DEFAULT_TOUCHED_FILES_MAX_BUFFER = 64 * 1024 * 1024

/** Every file the change touches: staged, unstaged, and untracked --
 * NOT `git diff --name-only <base>`, which sees neither of the last two
 * and would make the check unable to observe the thing it exists to check
 * (the merger's first version had exactly this gap). */
export function touchedFiles(root, { maxBuffer = DEFAULT_TOUCHED_FILES_MAX_BUFFER } = {}) {
  const run = (args) => {
    let output
    try {
      output = execFileSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer })
    } catch (error) {
      throw new Error(
        `[stub-import-guard] REFUSING TO RUN -- \`git ${args.join(' ')}\` failed while enumerating ` +
          `touched files (${error.code ?? error.message}). This is an infrastructure failure, not a ` +
          'stub-import verdict -- the guard never reports a result computed from a partial or ' +
          'truncated file list. A likely cause is a large untracked-file set (e.g. a build ' +
          'artifact directory nested inside the git working tree) exceeding the buffer; see #914.'
      )
    }
    return output
      .split('\n')
      .map((s) => s.trim())
      .filter(Boolean)
  }
  const staged = run(['diff', '--name-only', '--cached'])
  const unstaged = run(['diff', '--name-only'])
  const untracked = run(['ls-files', '--others', '--exclude-standard'])
  const all = new Set([...staged, ...unstaged, ...untracked])
  return [...all].filter((f) => f.endsWith('.ts') && !f.endsWith('.test.ts'))
}

export function checkTouchedFiles(root, files, inventory) {
  const stubByPkgAndName = new Map()
  for (const entry of inventory) {
    if (entry.status !== 'stub') continue
    const pkg = packageNameFor(root, entry.file)
    if (!pkg) continue
    const key = `${pkg}::${entry.symbol}`
    stubByPkgAndName.set(key, entry)
  }

  const violations = []
  for (const file of files) {
    const full = join(root, file)
    let source
    try {
      source = readFileSync(full, 'utf8')
    } catch {
      continue // deleted/renamed away
    }
    for (const { pkg, names } of extractChiefImports(source)) {
      for (const name of names) {
        const key = `${pkg}::${name}`
        const stub = stubByPkgAndName.get(key)
        if (stub) violations.push({ file, imports: name, from: pkg, implementedBy: stub.file })
      }
    }
  }
  return violations
}

function main() {
  const root = process.cwd()
  const inventory = deriveStubInventory(root)
  const stubs = inventory.filter((f) => f.status === 'stub')
  const partial = inventory.filter((f) => f.status === 'partially-stub')

  if (process.argv.includes('--inventory')) {
    console.log(JSON.stringify({ stubs, partial }, null, 2))
    return
  }

  const files = touchedFiles(root)
  const violations = checkTouchedFiles(root, files, inventory)

  if (partial.length > 0) {
    console.error('stub-import-guard: partially-stub classes found (some methods real, some throwing) -- not auto-classified, review manually:')
    for (const p of partial) console.error(`  ${p.file}: ${p.symbol} (stub methods: ${p.stubMethods.join(', ')})`)
  }

  if (violations.length > 0) {
    console.error(`stub-import-guard: ${violations.length} touched file(s) import a symbol whose implementation always throws:`)
    for (const v of violations) {
      console.error(`  ${v.file} imports '${v.imports}' from ${v.from} -- stub in ${v.implementedBy}`)
    }
    process.exitCode = 1
    return
  }

  console.log(`stub-import-guard: clean. ${stubs.length} stub symbol(s) across ${new Set(stubs.map((s) => s.file)).size} module(s) in the tree; none imported by a touched file.`)
}

// `import.meta.main` is Bun (and Node >= 24) only: under the Node 22 this
// repo's guards run on it is `undefined`, so this entrypoint could never
// fire. Every other script here uses the argv comparison below.
if (import.meta.url === `file://${process.argv[1]}`) main()
