import { mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import {
  assertChiefdBinaryBuilt,
  assertChiefdBinaryCurrent,
  chiefdBinarySkipTitle,
  chiefdBinaryTestGate,
  chiefdBuildCommand,
  isRunningInCI,
  newestChiefdSource,
  resolveChiefBinaryPath,
  resolveChiefdDaemonBinaryPath,
  resolveChiefdTargetRoot
} from '@/ChiefdBinary'

describe('resolveChiefdTargetRoot / resolveChiefBinaryPath', () => {
  const previousCargoTargetDir = process.env.CARGO_TARGET_DIR

  beforeEach(() => {
    delete process.env.CARGO_TARGET_DIR
  })

  afterEach(() => {
    if (typeof previousCargoTargetDir === 'string') {
      process.env.CARGO_TARGET_DIR = previousCargoTargetDir
    } else {
      delete process.env.CARGO_TARGET_DIR
    }
  })

  it('defaults to <repoRoot>/apps/chiefd/target when CARGO_TARGET_DIR is unset', () => {
    expect(resolveChiefdTargetRoot('/repo')).toBe(join('/repo', 'apps', 'chiefd', 'target'))
  })

  it('honors CARGO_TARGET_DIR verbatim when set (gh#143 regression)', () => {
    process.env.CARGO_TARGET_DIR = '/custom/target/dir'
    expect(resolveChiefdTargetRoot('/repo')).toBe('/custom/target/dir')
  })

  it('resolveChiefBinaryPath appends debug/chief to the target root', () => {
    expect(resolveChiefBinaryPath('/repo')).toBe(
      join('/repo', 'apps', 'chiefd', 'target', 'debug', 'chief')
    )
  })

  it('resolveChiefdDaemonBinaryPath names the SIBLING binary harnesses run', () => {
    // P6 split `chiefd` into the operator client `chief` and the daemon; the
    // client execs its sibling for `run` and `docstore-only`, which is every
    // mode these harnesses boot. Same directory, different file.
    expect(resolveChiefdDaemonBinaryPath('/repo')).toBe(
      join('/repo', 'apps', 'chiefd', 'target', 'debug', 'chiefd')
    )
    expect(resolveChiefdDaemonBinaryPath('/repo')).not.toBe(resolveChiefBinaryPath('/repo'))
  })
})

describe('chiefdBuildCommand', () => {
  it('is the exact cargo build command, and builds BOTH halves of the split', () => {
    expect(chiefdBuildCommand()).toBe(
      'cargo build --locked --manifest-path apps/chiefd/Cargo.toml --bin chief --bin chiefd'
    )
  })
})

describe('assertChiefdBinaryBuilt', () => {
  // #945-followup: same leak as the `chiefdBinaryTestGate` block below --
  // `resolveChiefdTargetRoot` resolves `CARGO_TARGET_DIR` verbatim,
  // ignoring the bogus repoRoot passed here, so a real `CARGO_TARGET_DIR`
  // with a real binary silently defeated both "does not exist" assertions.
  const previousCargoTargetDir = process.env.CARGO_TARGET_DIR

  beforeEach(() => {
    delete process.env.CARGO_TARGET_DIR
  })

  afterEach(() => {
    if (typeof previousCargoTargetDir === 'string') {
      process.env.CARGO_TARGET_DIR = previousCargoTargetDir
    } else {
      delete process.env.CARGO_TARGET_DIR
    }
  })

  it('throws with the exact build command when the binary does not exist', () => {
    // The command is read from its one definition, not transcribed: a regex
    // copy of it here rots the moment the build command gains a `--bin`.
    expect(() => assertChiefdBinaryBuilt('/definitely/not/a/real/repo/root')).toThrow(
      chiefdBuildCommand()
    )
  })

  it('names the missing binary path in the error', () => {
    expect(() => assertChiefdBinaryBuilt('/definitely/not/a/real/repo/root')).toThrow(
      /chiefd binary not found at/
    )
  })
})

describe('isRunningInCI (#846)', () => {
  const previousCi = process.env.CI

  afterEach(() => {
    if (typeof previousCi === 'string') {
      process.env.CI = previousCi
    } else {
      delete process.env.CI
    }
  })

  it('is false when CI is unset', () => {
    delete process.env.CI
    expect(isRunningInCI()).toBe(false)
  })

  it('is true when CI is set, regardless of its value', () => {
    process.env.CI = 'false'
    expect(isRunningInCI()).toBe(true)
  })
})

describe('chiefdBinaryTestGate / chiefdBinarySkipTitle (#846)', () => {
  // A real temp directory standing in for "repoRoot" — real filesystem
  // state, not a mocked existsSync, so this exercises the exact same
  // resolveChiefBinaryPath/existsSync path the real gate uses.
  //
  // #945-followup: `resolveChiefdTargetRoot` (ChiefdBinary.ts) resolves
  // `CARGO_TARGET_DIR` VERBATIM when set, ignoring `repoRoot` entirely —
  // so `chiefdBinaryTestGate(fakeRepoRoot)` silently checked the REAL
  // target dir instead of this fixture's empty one whenever
  // `CARGO_TARGET_DIR` reached the test process, turning every "binary
  // absent" simulation below into a false "present" the moment a real
  // binary existed there. Before #939 correctly declared `CARGO_TARGET_DIR`
  // for `test:unit`, turbo silently stripped it, so this leak was never
  // exercised — the gate this file exists to prove was itself running
  // under an environment the real gate never runs in. Neutralized the same
  // way the `resolveChiefdTargetRoot / resolveChiefBinaryPath` describe
  // block above already does: this suite must control every input its own
  // simulation depends on, not rely on the ambient environment happening
  // to be clean.
  let fakeRepoRoot: string
  const previousCi = process.env.CI
  const previousCargoTargetDir = process.env.CARGO_TARGET_DIR

  beforeEach(() => {
    fakeRepoRoot = mkdtempSync(join(tmpdir(), 'chiefd-binary-gate-fixture-'))
    delete process.env.CI
    delete process.env.CARGO_TARGET_DIR
  })

  afterEach(() => {
    rmSync(fakeRepoRoot, { recursive: true, force: true })
    if (typeof previousCi === 'string') {
      process.env.CI = previousCi
    } else {
      delete process.env.CI
    }
    if (typeof previousCargoTargetDir === 'string') {
      process.env.CARGO_TARGET_DIR = previousCargoTargetDir
    } else {
      delete process.env.CARGO_TARGET_DIR
    }
  })

  it('state 1 — both binaries present: reports present:true, no throw', () => {
    const binaryPath = resolveChiefBinaryPath(fakeRepoRoot)
    mkdirSync(join(fakeRepoRoot, 'apps', 'chiefd', 'target', 'debug'), { recursive: true })
    writeFileSync(binaryPath, '')
    writeFileSync(resolveChiefdDaemonBinaryPath(fakeRepoRoot), '')

    const gate = chiefdBinaryTestGate(fakeRepoRoot)
    expect(gate).toEqual({ present: true, binaryPath })
  })

  it('state 1b — the CLIENT alone is not enough: harnesses boot a daemon mode', () => {
    // The confusing half of the P6 split, and the reason the gate checks two
    // files: `chiefd docstore-only` execs its sibling, so a build that
    // produced only `chiefd` gives a harness that starts a process which dies
    // on exec. That reads as a daemon that "failed to become reachable" — a
    // timeout, minutes later, naming nothing. Caught here instead.
    mkdirSync(join(fakeRepoRoot, 'apps', 'chiefd', 'target', 'debug'), { recursive: true })
    writeFileSync(resolveChiefBinaryPath(fakeRepoRoot), '')

    expect(chiefdBinaryTestGate(fakeRepoRoot).present).toBe(false)
    expect(() => assertChiefdBinaryBuilt(fakeRepoRoot)).toThrow(
      new RegExp(`chiefd binary not found at ${resolveChiefdDaemonBinaryPath(fakeRepoRoot)}`)
    )
  })

  it('state 2 — binary absent, NOT in CI: reports present:false, no throw', () => {
    delete process.env.CI
    const gate = chiefdBinaryTestGate(fakeRepoRoot)
    expect(gate.present).toBe(false)
    expect(gate.binaryPath).toBe(resolveChiefBinaryPath(fakeRepoRoot))
  })

  it('state 3 — binary absent, IN CI: throws the exact build command, never returns a skip result', () => {
    process.env.CI = 'true'
    expect(() => chiefdBinaryTestGate(fakeRepoRoot)).toThrow(chiefdBuildCommand())
  })

  it('chiefdBinarySkipTitle names the suite, the missing path, and the build command — the visible skip banner', () => {
    const gate = chiefdBinaryTestGate(fakeRepoRoot)
    const title = chiefdBinarySkipTitle('my suite', gate)
    expect(title).toContain('SKIPPING')
    expect(title).toContain('my suite')
    expect(title).toContain(gate.binaryPath)
    expect(title).toContain(chiefdBuildCommand())
  })
})

/**
 * EXISTENCE IS NOT IDENTITY.
 *
 * On 2026-08-09 a `cargo build --locked` refused on a stale
 * `Cargo.lock`, left a six-hour-old binary at the resolved path, and the suite
 * ran the whole tool contract against a daemon that predated the code under
 * test. Two tests went red — a 404 for a route the old binary never had, and an
 * authentication test reporting that an unenrolled key was ACCEPTED. The second
 * was escalated as a live security hole. It was not one: the check simply was
 * not in the binary. Both directions are asserted here, because a staleness
 * guard that only ever passes is the same guard that was missing.
 */
describe('assertChiefdBinaryCurrent', () => {
  const scratch: string[] = []

  afterEach(() => {
    for (const dir of scratch.splice(0)) rmSync(dir, { recursive: true, force: true })
  })

  /** A repo-shaped scratch tree: one Rust source, one binary, chosen mtimes. */
  function tree(sourceMs: number, binaryMs: number): { root: string; binaryPath: string } {
    const root = mkdtempSync(join(tmpdir(), 'chiefd-staleness-'))
    scratch.push(root)
    const crate = join(root, 'apps', 'chiefd', 'crates', 'chiefd-core', 'src')
    mkdirSync(crate, { recursive: true })
    const source = join(crate, 'lib.rs')
    writeFileSync(source, 'pub fn answer() -> u8 { 42 }\n')
    utimesSync(source, new Date(sourceMs), new Date(sourceMs))

    const debugDir = join(root, 'apps', 'chiefd', 'target', 'debug')
    mkdirSync(debugDir, { recursive: true })
    const binaryPath = join(debugDir, 'chiefd')
    writeFileSync(binaryPath, 'ELF')
    utimesSync(binaryPath, new Date(binaryMs), new Date(binaryMs))
    return { root, binaryPath }
  }

  const SOURCE_AT = Date.parse('2026-08-09T23:33:00.000Z')

  it('accepts a binary built after its sources', () => {
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT + 60_000)
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).not.toThrow()
  })

  it('refuses a binary older than its sources, and SAYS that is what happened', () => {
    // The real gap: built 19:23, source landed 23:33.
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT - 4 * 60 * 60 * 1000)
    let thrown: Error | undefined
    try {
      assertChiefdBinaryCurrent(binaryPath, root)
    } catch (error) {
      thrown = error instanceof Error ? error : new Error(String(error))
    }
    expect(thrown, 'a stale binary must be refused, not silently used').toBeDefined()
    expect(
      thrown?.message,
      'the message is the whole value of this guard — "predates the code under ' +
        'test" is the sentence that would have saved an evening'
    ).toMatch(/predates the code under test/i)
    // It must also name BOTH sides, or the reader cannot tell which to rebuild.
    expect(thrown?.message).toContain(binaryPath)
    expect(thrown?.message).toContain('lib.rs')
    expect(thrown?.message).toContain(chiefdBuildCommand())
  })

  /** Adds a cargo-shaped `<binary>.d` naming exactly the sources that binary
   *  was built from — the file cargo itself writes on every build. */
  function withDepFile(binaryPath: string, dependencies: readonly string[]): void {
    writeFileSync(`${binaryPath}.d`, `${binaryPath}: ${dependencies.join(' ')}\n`)
  }

  // The defect this closes, measured on a real tree: `chiefd` does not
  // depend on `chief-cli`, so a `cargo fmt` of one `chief-cli` file rebuilds
  // `chiefd` and correctly leaves `chiefd` alone — and the whole-tree
  // walk then called the untouched daemon stale FOREVER, because no rebuild
  // can move the mtime of a binary cargo has nothing to recompile. Ten
  // chiefing contract suites and the piing tool-contract suite went red with a
  // message instructing a rebuild that provably could not fix them.
  it('judges a binary by ITS OWN dependencies, not by the newest file in the workspace', () => {
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT + 60_000)
    // A source this binary does not depend on, changed long after it was built.
    const unrelatedDir = join(root, 'apps', 'chiefd', 'crates', 'chief-cli', 'src')
    mkdirSync(unrelatedDir, { recursive: true })
    const unrelated = join(unrelatedDir, 'preflight.rs')
    writeFileSync(unrelated, 'pub fn decide() {}\n')
    const muchLater = SOURCE_AT + 10 * 60 * 60 * 1000
    utimesSync(unrelated, new Date(muchLater), new Date(muchLater))

    const own = join(root, 'apps', 'chiefd', 'crates', 'chiefd-core', 'src', 'lib.rs')
    withDepFile(binaryPath, [own])

    expect(
      () => assertChiefdBinaryCurrent(binaryPath, root),
      'a file outside this binary’s dependency graph must not make it stale'
    ).not.toThrow()
  })

  it('still refuses when a source the binary DOES depend on is newer', () => {
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT - 60_000)
    const own = join(root, 'apps', 'chiefd', 'crates', 'chiefd-core', 'src', 'lib.rs')
    withDepFile(binaryPath, [own])
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).toThrow(
      /predates the code under test/i
    )
  })

  // Fail closed: the lenient answer's failure mode is a stale daemon standing
  // in for a fresh one, which is the incident this module exists to prevent.
  it('falls back to the whole-tree walk when cargo wrote no usable dep file', () => {
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT - 60_000)
    // No `.d` at all.
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).toThrow(
      /predates the code under test/i
    )
    // A `.d` that is not the shape cargo writes is no better than none.
    writeFileSync(`${binaryPath}.d`, 'this is not a makefile rule\n')
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).toThrow(
      /predates the code under test/i
    )
  })

  it('ignores target/, so a build is never compared against its own output', () => {
    // A file written INTO target after the build must not make the binary look
    // stale — otherwise the guard fires on every run and gets deleted.
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT + 1000)
    const artifact = join(root, 'apps', 'chiefd', 'target', 'debug', 'build.rs')
    mkdirSync(dirname(artifact), { recursive: true })
    writeFileSync(artifact, '// build output')
    utimesSync(artifact, new Date(SOURCE_AT + 99_000), new Date(SOURCE_AT + 99_000))
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).not.toThrow()
  })

  it('watches the lockfile too — the exact file whose staleness caused this', () => {
    const { root, binaryPath } = tree(SOURCE_AT, SOURCE_AT + 1000)
    const lock = join(root, 'apps', 'chiefd', 'Cargo.lock')
    writeFileSync(lock, '# lock')
    utimesSync(lock, new Date(SOURCE_AT + 5000), new Date(SOURCE_AT + 5000))
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).toThrow(
      /predates the code under test/i
    )
  })

  it('newestChiefdSource is undefined for a tree with no chiefd sources at all', () => {
    const root = mkdtempSync(join(tmpdir(), 'chiefd-staleness-empty-'))
    scratch.push(root)
    expect(newestChiefdSource(root)).toBeUndefined()
    // ...and an absent source set must never be treated as "stale".
    const debugDir = join(root, 'apps', 'chiefd', 'target', 'debug')
    mkdirSync(debugDir, { recursive: true })
    const binaryPath = join(debugDir, 'chiefd')
    writeFileSync(binaryPath, 'ELF')
    expect(() => assertChiefdBinaryCurrent(binaryPath, root)).not.toThrow()
  })
})
