// P3: the TypeScript CLI stays deleted, and chiefd never shells out to a
// JavaScript CLI again.
//
// THE MANDATE THIS ENFORCES, stated as a property: chiefd is the backend and it
// is client-agnostic. The Rust binary owns every operator command. There must
// be no code path in which the Rust binary hands an unrecognised argv to a
// JavaScript process.
//
// THE DISTINCTION THAT MATTERS, and the reason this guard is three narrow rules
// rather than one broad one:
//
//   * Spawning **Pi** is legitimate and must stay possible. Pi is the agent
//     runtime; its CLI begins `#!/usr/bin/env node`, and a JavaScript runtime is
//     genuinely required to open a Pi session. `lifecycle/founder.rs` spawns it
//     directly and that is the intended end state, not a tolerated exception.
//   * Spawning **bun** to run a TypeScript program that parses argv and
//     dispatches a command is not. That program was a second claimant for
//     chiefd's command surface — the one that answered `chief ls` with
//     `unknown command 'ls'` — and it is deleted.
//
// So the rules below never mention Pi, and they never ban the WORD "bun":
// operator-facing refusals legitimately say "Run 'bun run release' from the
// checkout", because that is an instruction to a human, not a process this
// binary starts. What is banned is bun appearing where a program name goes.
//
// WHY A REPO GUARD RATHER THAN ONLY A RUST TEST. `lifecycle.rs` carries the
// inverted in-crate assertion (it used to read "the one legitimate Bun reach
// must survive"), but it can only see the file it includes. This one sees the
// whole tree, including the package manifests, the workflows and the shell
// scripts through which the app could be resurrected without touching Rust at
// all.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, sep } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { skipSet } from '../tree-walk-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

/**
 * #1041: every path under `apps/cli` that git can SEE — tracked, staged, or
 * untracked-and-not-ignored. This replaced `existsSync('apps/cli')`.
 *
 * The invariant is "the TypeScript CLI is deleted", and the subject of that
 * sentence is SOURCE, not an inode. `existsSync` answered a different
 * question — "is there a directory at this path on this disk" — and a
 * surviving `apps/cli/node_modules` that a `git clean` exclusion spared was
 * enough to turn this guard red. It was correct about what it saw and wrong
 * about what it meant, which is the same instrument-cannot-see-its-subject
 * shape as #1041's other five instances. A build artifact left behind by a
 * package manager is not a second claimant for chiefd's command surface.
 *
 * `--exclude-standard` is what makes the distinction: it drops the
 * gitignored leftovers and keeps the case that actually matters, an
 * untracked-but-not-ignored `apps/cli/src/Main.ts` somebody wrote and has
 * not staged yet. That case stays caught — the guard gets narrower on
 * artifacts and loses nothing on source.
 */
/** True when `path` is an existing file. */
function statSafe(path) {
  try {
    return statSync(path).isFile()
  } catch {
    return false
  }
}

function gitVisibleUnderAppsCli() {
  const out = execFileSync(
    'git',
    ['ls-files', '--cached', '--others', '--exclude-standard', '--', 'apps/cli'],
    { cwd: repoRoot, encoding: 'utf8' }
  )
  return out.split('\n').filter((line) => line.length > 0)
}

/**
 * Directories never worth walking: not source, or not ours.
 *
 * The shared members — build output, and the checkouts that are not this one —
 * come from `tree-walk-lib`, so a new hazard is fixed once for every walking
 * guard rather than in whichever ones somebody remembers. `patches` is this
 * guard's own: vendored diffs quote deleted code by definition.
 */
const SKIP_DIRS = skipSet(['patches'])

/**
 * Files that are a RECORD of the deletion rather than a reference to the
 * deleted thing. A changelog that cannot name what was removed is a changelog
 * that has stopped being useful; a plan that cannot quote its own scope is a
 * plan nobody can check the work against. Deliberately a small, closed set of
 * prose surfaces — not `**\/*.md`, which would let a live instruction hide in a
 * doc.
 */
const HISTORY = new Set(['CHANGELOG.md', 'DECISIONS.md', 'AGENTS.md', 'CLAUDE.md'])

/** Prose directories: the same argument as [`HISTORY`], applied to a tree.
 *  `plans` came out with the open-source release -- a plan is a LOCAL working
 *  document now, so no path under it is ever scanned and the entry could
 *  never match. */
const HISTORY_DIRS = ['docs', 'conformance']

/** Is this path a historical record rather than a live instruction? */
function isHistory(relPath) {
  if (HISTORY.has(relPath)) return true
  return HISTORY_DIRS.some((dir) => relPath === dir || relPath.startsWith(`${dir}/`))
}

/** Every file in the repo worth scanning, repo-relative and `/`-separated. */
function repoFiles(root = repoRoot) {
  const out = []
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (SKIP_DIRS.has(entry.name)) continue
      const full = join(dir, entry.name)
      if (entry.isDirectory()) walk(full)
      else if (entry.isFile()) out.push(relative(root, full).split(sep).join('/'))
    }
  }
  walk(root)
  return out.sort()
}

/**
 * Rust source with every comment removed.
 *
 * Line comments (`//`, `///`, `//!`) only — this tree does not use Rust block
 * comments, and a half-implemented block-comment stripper that silently
 * mis-parses would make the scan blind, which is worse than not stripping. A string
 * containing `//` (a URL) loses its tail here; that is acceptable because every
 * rule below looks for `bun`, never for a URL.
 */
function rustCode(source) {
  return source
    .split('\n')
    .map((line) => {
      const comment = line.indexOf('//')
      return comment === -1 ? line : line.slice(0, comment)
    })
    .join('\n')
}

/**
 * Every place `bun` appears as a PROGRAM in Rust code.
 *
 * Three shapes, all of them ways a process actually gets started:
 *   - a bare `"bun"` string literal (the deleted resolution ladder's
 *     `.unwrap_or_else(|| "bun".to_string())` tail),
 *   - `Command::new(...)` naming bun,
 *   - a string literal beginning `bun ` (a shelled-out command line).
 *
 * `TEAM_LAUNCHER_BUN` does not match any of them: it is an environment variable
 * a person's own extension reads, forwarded by chiefd and never executed by it,
 * and `\b` does not fire inside `LAUNCHER_BUN`.
 */
function bunProgramSites(code) {
  const sites = []
  for (const [index, line] of code.split('\n').entries()) {
    const hit =
      /"bun"/.test(line) ||
      /Command::new\([^)]*\bbun\b/i.test(line) ||
      /"[^"]*\bbun\s+[^"]*\.(?:ts|js|mjs|cjs)\b/.test(line)
    if (hit) sites.push({ line: index + 1, text: line.trim() })
  }
  return sites
}

/**
 * A file's lines with comment-only lines dropped.
 *
 * Deliberately line-level and language-agnostic (`//`, `*`, `/*`, `#`): a live
 * instruction — a path a program resolves, a command a script runs — never
 * lives on a comment-only line, while a PORT CITATION always does. This tree
 * cites the deleted TypeScript by path in dozens of `//!` provenance headers,
 * and a guard that forbade those would be demanding the code forget where it
 * came from, which is not what "no TS stub" means.
 */
function codeLines(source) {
  return source
    .split('\n')
    .filter((line) => {
      const trimmed = line.trimStart()
      return !(
        trimmed.startsWith('//') ||
        trimmed.startsWith('*') ||
        trimmed.startsWith('/*') ||
        trimmed.startsWith('#')
      )
    })
    .join('\n')
}

/** The deleted entry points, as a path a program could resolve. */
const DELETED_ENTRY = /apps\/cli\/src\/(?:Main|FounderPi|common\/Env)\.ts/

const files = repoFiles()

test('the scan is not vacuous: it walks a real repository', () => {
  // A guard whose scan silently returns nothing passes forever while proving
  // nothing. Every rule below is a "no match" assertion, so this floor is the
  // only thing standing between them and a green that means "found no files".
  assert.ok(files.length > 500, `expected a real tree, walked ${files.length} files`)
  assert.ok(
    files.some((file) => file === 'apps/chiefd/crates/chief-cli/src/founder.rs'),
    'the Founder spawn must be in scope — if this file is not walked, the whole guard is theatre'
  )
  assert.ok(
    files.filter((file) => file.endsWith('.rs')).length > 100,
    'the Rust tree must be in scope'
  )
})

test('apps/cli does not exist', () => {
  assert.deepEqual(
    gitVisibleUnderAppsCli(),
    [],
    'apps/cli is deleted. The operator surface is the chiefd binary; a TypeScript app that parses argv is a second claimant for it.'
  )
  // Not merely absent from git — absent from the workspace globs' expansion,
  // which is what a manifest and a lockfile agree about.
  //
  // #1041: a workspace member is a directory WITH A package.json, not any
  // directory the glob's group happens to contain. Before this, a leftover
  // `apps/cli/node_modules` was enough to make `apps/cli` an entry here and
  // fail the assertion — the same disk-versus-invariant confusion the first
  // assertion above carried. A bare directory is not a member; bun would not
  // install it, and the lockfile would not name it.
  const manifest = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'))
  for (const pattern of manifest.workspaces ?? []) {
    const [group] = pattern.split('/')
    const members = readdirSync(join(repoRoot, group), { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .filter((entry) => statSafe(join(repoRoot, group, entry.name, 'package.json')))
      .map((entry) => `${group}/${entry.name}`)
    assert.ok(!members.includes('apps/cli'), 'apps/cli must not be a workspace member')
  }
})

test('nothing outside the historical record names a deleted TypeScript entry point', () => {
  const offenders = []
  for (const file of files) {
    if (isHistory(file)) continue
    if (file === relative(repoRoot, fileURLToPath(import.meta.url)).split(sep).join('/')) continue
    let source
    try {
      source = readFileSync(join(repoRoot, file), 'utf8')
    } catch {
      continue // a binary or unreadable file names no module
    }
    if (DELETED_ENTRY.test(codeLines(source))) offenders.push(file)
  }
  assert.deepEqual(
    offenders,
    [],
    'these files still name a deleted TypeScript entry point; a reference that cannot resolve is exactly what breaks `bun run release` from a clean clone'
  )
})

test('no Rust source starts bun as a program', () => {
  const offenders = []
  for (const file of files) {
    if (!file.endsWith('.rs')) continue
    const sites = bunProgramSites(rustCode(readFileSync(join(repoRoot, file), 'utf8')))
    for (const site of sites) offenders.push(`${file}:${site.line} ${site.text}`)
  }
  assert.deepEqual(
    offenders,
    [],
    'the Rust binary must not hand an argv to a JavaScript CLI. Spawning Pi is fine — Pi is the agent runtime; spawning bun to run a program that parses a command is not.'
  )
})

/**
 * EMPTY. No production source names the deleted package any more.
 *
 * The one row here was `packages/piing/extensions/organization-intercom.ts`,
 * whose `defaultLauncherRunner` spawned
 * `bun run <launcherRoot>/apps/cli/src/Main.ts <verb>` and whose
 * `OrganizationRuntimeContext.cliPath` built that path with
 * `join(launcherRoot, "apps", "cli", "src", "Main.ts")`. The row said, in its
 * own words, that when the intercom-to-HTTP port landed and the file stopped
 * naming the path this guard would go red and the row would have to be
 * deleted. That is what happened: every verb reached chiefd over HTTP,
 * `runChecked` was left as the only caller of a `LauncherRunner` and had no
 * call sites of its own, and the whole transport — the spawn, the field that
 * addressed it, and the ~400 lines around them — was deleted rather than
 * ported. This comment is the record; the empty set is the assertion.
 *
 * It stays an EXACT SET, not an allowlist. A new production reference fails
 * here, and so would a row left behind after its offender went away — the same
 * rot as an allowlist a file move orphaned.
 */
const KNOWN_REMAINING = []

/** Files this rule reads: production source and the documents a human follows.
 *  Not test files or fixtures — those legitimately QUOTE the deleted path (a
 *  regex over historical port citations, a captured cargo log, a mutation
 *  fixture that wires a fake suite), and the entry-point rule above already
 *  covers anything they could execute. */
function namesDeletedPackage(file) {
  if (isHistory(file)) return false
  if (file.startsWith('.github/')) return false
  if (/(^|\/)tests?\//.test(file) || /\.test\.[a-z]+$/.test(file) || file.includes('/fixtures/')) {
    return false
  }
  if (file === relative(repoRoot, fileURLToPath(import.meta.url)).split(sep).join('/')) return false
  // CODE ONLY. Markdown never resolves a path: a package README recording that
  // a deleted preload "statically imported apps/cli/src/legacy/foundation/paths"
  // is a true sentence about the past, and demanding it be rewritten would make
  // this guard a prose editor. The entry-point rule above still covers every
  // document outside the historical record.
  if (!/\.(rs|ts|tsx|mjs|js|sh|json|jsonc|ya?ml)$/.test(file)) return false
  let source
  try {
    source = codeLines(readFileSync(join(repoRoot, file), 'utf8'))
  } catch {
    return false
  }
  return (
    /apps\/cli\b/.test(source) ||
    /'apps',\s*'cli'/.test(source) ||
    /"apps",\s*"cli"/.test(source)
  )
}

test('production source names the deleted package in exactly the one place a named packet owns', () => {
  // BROADER than the entry-point rule above, and it exists because that rule
  // was not enough: two piing tests walked `apps/cli/src` as a DIRECTORY,
  // named no entry point at all, and so passed every check while throwing
  // ENOENT on a clean clone and passing on every warm one — the deleted tree
  // still on disk. That is the invisible-dangling-reference shape this whole
  // packet exists to end, so the guard reads the directory name too.
  assert.deepEqual(
    files.filter(namesDeletedPackage),
    KNOWN_REMAINING,
    'a NEW production reference to the deleted package, or a KNOWN one that is finally gone — either way this row set must change with it'
  )
})

test('no package script, workflow or shell script runs the deleted app', () => {
  const manifest = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'))
  for (const [name, command] of Object.entries(manifest.scripts ?? {})) {
    assert.ok(!DELETED_ENTRY.test(command), `package.json script "${name}" runs a deleted entry point`)
    assert.ok(!/\bbun\s+apps\//.test(command), `package.json script "${name}" runs an app entry point through bun`)
  }
  const offenders = []
  for (const file of files) {
    if (isHistory(file)) continue
    if (!/\.(ya?ml|sh|mjs|ts)$/.test(file)) continue
    if (file === relative(repoRoot, fileURLToPath(import.meta.url)).split(sep).join('/')) continue
    const source = codeLines(readFileSync(join(repoRoot, file), 'utf8'))
    for (const [index, line] of source.split('\n').entries()) {
      if (/\bbun\s+apps\/cli\b/.test(line)) offenders.push(`${file}:${index + 1}`)
    }
  }
  assert.deepEqual(offenders, [], 'these lines invoke the deleted TypeScript app through bun')
})

// ---------------------------------------------------------------------------
// The detector's own red proof. Every rule above asserts an ABSENCE, so each
// one is indistinguishable from a broken matcher until it has been seen to
// fire. These run the same functions against fixtures that contain the real
// shapes.
// ---------------------------------------------------------------------------

test('demonstrated red: the bun-program matcher fires on every shape it claims to catch', () => {
  const caught = [
    'let bun = env.unwrap_or_else(|| "bun".to_string());',
    'Command::new("bun").arg(&cli).status()',
    'let bun = resolve(); Command::new(&bun).arg(entry)',
    'Command::new("sh").arg("-c").arg("bun apps/cli/src/Main.ts founder-pi")'
  ]
  for (const line of caught) {
    assert.equal(bunProgramSites(line).length, 1, `must be caught: ${line}`)
  }
  const allowed = [
    // Operator guidance, not a spawn: the human is being told what to type.
    '"Pi is required. Run \'bun run release\' from the checkout."',
    // An env var a person's own extension reads; chiefd forwards, never runs it.
    'const FORWARDED: [&str; 1] = ["TEAM_LAUNCHER_BUN"];',
    // The intended end state: the program is a RESOLVED value, not a literal.
    'let pi = preflight::pi_runtime_or_refusal()?; Command::new(&pi)',
    // A denylist literal a skill scanner matches text against, never spawned.
    'b"bun add",'
  ]
  for (const line of allowed) {
    assert.deepEqual(bunProgramSites(line), [], `must be allowed: ${line}`)
  }
})

test('comment-only lines are a citation, not a reference: a port header may name the deleted app', () => {
  const header = '//! Ported from the deleted `apps/cli/src/Main.ts`.\nlet x = 1;\n'
  assert.equal(DELETED_ENTRY.test(codeLines(header)), false)
  // ...but the same path in CODE is still caught.
  assert.equal(DELETED_ENTRY.test(codeLines('root.join("apps/cli/src/Main.ts")\n')), true)
})

test('demonstrated red: comment stripping does not blind the matcher to real code on the same line', () => {
  const code = rustCode('    let bun = "bun".to_string(); // resolved above\n')
  assert.equal(bunProgramSites(code).length, 1)
  // ...and a comment ALONE is not a finding, which is the whole reason the
  // stripper exists: history has to stay writable.
  assert.deepEqual(bunProgramSites(rustCode('// this used to spawn "bun" against Main.ts\n')), [])
})

test('demonstrated red: the deleted-entry matcher fires on the real path shapes', () => {
  for (const shape of [
    'root.join("apps/cli/src/Main.ts")',
    "import { runFounderPi } from './apps/cli/src/FounderPi.ts'",
    '"entry": ["apps/cli/src/Main.ts"]'
  ]) {
    assert.ok(DELETED_ENTRY.test(shape), `must be caught: ${shape}`)
  }
  assert.equal(DELETED_ENTRY.test('apps/web/src/Main.ts'), false)
})

test('the history exemption is a closed set, not a blanket markdown pass', () => {
  assert.equal(isHistory('CHANGELOG.md'), true)
  assert.equal(isHistory('docs/ORGANIZATION_ARCHITECTURE.md'), true)
  assert.equal(isHistory('docs/testing/parked-suite-triage.json'), true)
  // A live surface that happens to be markdown is NOT exempt: README is how a
  // human learns which command to run.
  assert.equal(isHistory('README.md'), false)
  assert.equal(isHistory('scripts/clean-install-smoke.sh'), false)
  assert.equal(isHistory('.github/workflows/ci.yml'), false)
})

test('the walk really skips node_modules, or every rule above becomes a coin flip', () => {
  assert.ok(!files.some((file) => file.includes('node_modules/')))
  assert.ok(!files.some((file) => file.startsWith('target/')))
  // And it really does reach deep source, rather than stopping at the top level.
  assert.ok(files.some((file) => file.startsWith('apps/chiefd/crates/chiefd-host/src/')))
})

test('the intended end state is present, not merely the banned one absent', () => {
  // The strongest failure mode for this whole file is deleting the Founder
  // spawn instead of porting it: every "no bun" rule above would pass on a
  // chiefd that cannot open a Founder session at all.
  const founder = readFileSync(
    join(repoRoot, 'apps/chiefd/crates/chief-cli/src/founder.rs'),
    'utf8'
  )
  assert.match(founder, /founder_pi::founder_pi_argv/, 'chiefd must still build the Founder Pi argv')
  const argv = readFileSync(
    join(repoRoot, 'apps/chiefd/crates/chief-cli/src/founder_pi.rs'),
    'utf8'
  )
  assert.match(argv, /chiefd_launch_company/, 'the Founder must still be given its one durable action')
  assert.ok(statSync(join(repoRoot, 'apps/chiefd/crates/chief-cli/src/founder_pi.rs')).size > 0)
})
