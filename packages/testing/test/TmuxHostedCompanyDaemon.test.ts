/**
 * THE LAUNCHER ROOT THIS HARNESS BOOTS A COMPANY AGAINST.
 *
 * `chiefd run` resolves its launcher root — the checkout it materializes
 * people from and inventories Pi resources out of — from `--launcher-root`,
 * `ORG_LAUNCHER_ROOT`, or, when neither is given, `~/.chief/launcher-root`.
 * That last one is a record in the OPERATOR's home directory. On a shared
 * build box it is one file for every agent on the machine, so a harness that
 * does not pass the flag boots against whatever checkout the last agent
 * happened to write there. Observed 2026-08-09: a company booted by this
 * harness recorded `org_settings.launcher_root = /root/wt-web`, a different
 * agent's worktree. Nothing failed — the suite was simply green about the
 * wrong tree.
 *
 * The value is pinned at FIRST BOOT and cannot be repaired afterwards: chiefd
 * records it into `org_settings` on the first completed materialization, and
 * the recorded value then outranks `~/.chief/launcher-root`,
 * `ORG_LAUNCHER_ROOT` and `--launcher-root` alike.
 *
 * The boot test below drives the real `startTmuxHostedCompany` against stub
 * `chiefd`/`beacond` executables that record their own argv, rather than
 * asserting on `chiefdRunArgv` alone: a pure-function assertion would stay
 * green if the spawn site stopped calling it and inlined its own argument
 * list, which is exactly the shape the flag went missing in.
 */
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { isAbsolute, join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import { chiefdRunArgv, startTmuxHostedCompany } from '@/TmuxHostedCompanyDaemon'

/** A stub `beacond`: health always up, every company row accepted. */
const BEACOND_STUB = `#!/usr/bin/env node
const http = require('node:http')
const [host, port] = (process.env.BEACOND_BIND || '127.0.0.1:0').split(':')
http
  .createServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end('{}')
  })
  .listen(Number(port), host)
`

/**
 * A stub `chiefd run`: writes the argv it was given to `CHIEF_TEST_ARGV_LOG`,
 * one argument per line, then answers every path so the harness's readiness
 * probe resolves.
 */
const CHIEFD_STUB = `#!/usr/bin/env node
const http = require('node:http')
const fs = require('node:fs')
fs.writeFileSync(process.env.CHIEF_TEST_ARGV_LOG, process.argv.slice(2).join('\\n'))
const [host, port] = (process.env.CHIEFD_STORE_BIND || '127.0.0.1:0').split(':')
http
  .createServer((_req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end('{}')
  })
  .listen(Number(port), host)
`

describe('chiefdRunArgv (#862): the launcher root is an explicit argument', () => {
  const argv = chiefdRunArgv({
    dir: '/work/cobalt',
    tmuxSocket: 'chief-test-abc123',
    repoRoot: '/root/wt-under-test'
  })

  it('passes --launcher-root, and passes the checkout under test as its value', () => {
    const flagIndex = argv.indexOf('--launcher-root')
    expect(flagIndex).toBeGreaterThanOrEqual(0)
    expect(argv[flagIndex + 1]).toBe('/root/wt-under-test')
  })

  it('carries the caller-supplied root, not a constant that happens to match', () => {
    // The other direction. A hardcoded `--launcher-root /root/wt-under-test`
    // would satisfy the assertion above and still boot every company on this
    // machine against one tree.
    const other = chiefdRunArgv({
      dir: '/work/cobalt',
      tmuxSocket: 'chief-test-abc123',
      repoRoot: '/root/wt-somewhere-else'
    })
    expect(other[other.indexOf('--launcher-root') + 1]).toBe('/root/wt-somewhere-else')
  })

  it('still refuses --serve-only: this harness exists for the tmux host capability', () => {
    expect(argv).not.toContain('--serve-only')
  })

  /** THE DAEMON IS TOLD A DIRECTORY AND NOTHING ELSE ABOUT WHICH COMPANY.
   *
   * `--company <slug>` and `--data-root <orgsRoot>` were the pair, and the
   * pair is what made one slug under two roots two companies fighting over
   * one key. A daemon that still accepted either would be a second way to
   * name a company. */
  it('names the company by its directory alone — no --company, no --data-root', () => {
    expect(argv.slice(0, 3)).toStrictEqual(['run', '--dir', '/work/cobalt'])
    expect(argv).not.toContain('--company')
    expect(argv).not.toContain('--data-root')
  })

  it('passes --pi-binary as an ABSOLUTE path inside the checkout under test', () => {
    // `chiefd run` refuses to start without this, and refuses a relative one:
    // it is what every person's pane execs, and the bare name `pi` it used to
    // default to was resolved by whatever PATH the pane inherited. Pinned to
    // the checkout for the same reason `--launcher-root` is.
    const flagIndex = argv.indexOf('--pi-binary')
    expect(flagIndex).toBeGreaterThanOrEqual(0)
    const value = argv[flagIndex + 1]
    expect(value).toBe('/root/wt-under-test/node_modules/.bin/pi')
    expect(isAbsolute(String(value))).toBe(true)
  })

  it('carries the caller-supplied root into --pi-binary too, not a constant', () => {
    const other = chiefdRunArgv({
      dir: '/work/cobalt',
      tmuxSocket: 'chief-test-abc123',
      repoRoot: '/root/wt-somewhere-else'
    })
    expect(other[other.indexOf('--pi-binary') + 1]).toBe(
      '/root/wt-somewhere-else/node_modules/.bin/pi'
    )
  })

  // #751/P9 renamed the flag chiefd parses. `chiefd run`'s arg loop matches
  // `--runtime-socket` and nothing else, so the old spelling is not an alias
  // that still works — it is an unrecognized flag, and the boot silently falls
  // back to the slug-derived socket instead of the per-test one `stop()` kills.
  // Asserting BOTH directions: the surviving flag is present and carries the
  // caller's socket, and the retired spelling is gone rather than duplicated.
  it('passes the runtime socket under the flag chiefd actually parses (--runtime-socket)', () => {
    const flagIndex = argv.indexOf('--runtime-socket')
    expect(flagIndex).toBeGreaterThanOrEqual(0)
    expect(argv[flagIndex + 1]).toBe('chief-test-abc123')
    expect(argv).not.toContain('--tmux-socket')
  })
})

describe('startTmuxHostedCompany (#862): a booted company never falls back to the shared home', () => {
  const previousCargoTargetDir = process.env.CARGO_TARGET_DIR
  let repoRoot: string
  let argvLog: string

  /** A repo-shaped tree whose `chief`/`chiefd`/`beacond` are the stubs
   * above.
   *
   * `chief` is here because the harness checks for it by name before it
   * spawns anything (`assertChiefdBinaryBuilt`), and it is right to: `chief`
   * is an operator client that `exec`s its sibling daemon `chiefd`, so a
   * debug directory holding only one of the pair produces a child that dies at
   * exec time and a failure that reads like anything but a missing file. A stub
   * repo that ships only two of the three binaries is not repo-shaped — it is
   * the exact shape that precondition exists to reject, and omitting it here
   * made this test fail against a harness that was behaving correctly.
   *
   * It carries the same source as `chiefd`: nothing execs it in this test, and
   * a stub that would answer identically if something ever did is the honest
   * one to leave behind. */
  function makeStubRepo(): string {
    const dir = mkdtempSync(join(tmpdir(), 'chief-launcher-root-'))
    // Pin CommonJS for the stubs: they are extensionless executables, so node
    // resolves their module kind from the nearest package.json.
    writeFileSync(join(dir, 'package.json'), '{ "type": "commonjs" }\n')
    const debugDir = join(dir, 'apps', 'chiefd', 'target', 'debug')
    mkdirSync(debugDir, { recursive: true })
    for (const [name, source] of [
      ['chief', CHIEFD_STUB],
      ['chiefd', CHIEFD_STUB],
      ['beacond', BEACOND_STUB]
    ] as const) {
      const path = join(debugDir, name)
      writeFileSync(path, source)
      chmodSync(path, 0o755)
    }
    return dir
  }

  beforeEach(() => {
    // The stubs live at the in-repo path; an inherited CARGO_TARGET_DIR would
    // send `resolveChiefBinaryPath` somewhere else entirely.
    delete process.env.CARGO_TARGET_DIR
    repoRoot = makeStubRepo()
    argvLog = join(repoRoot, 'chiefd-argv.txt')
  })

  afterEach(() => {
    rmSync(repoRoot, { recursive: true, force: true })
    if (typeof previousCargoTargetDir === 'string') {
      process.env.CARGO_TARGET_DIR = previousCargoTargetDir
    } else {
      delete process.env.CARGO_TARGET_DIR
    }
  })

  it('spawns chiefd with --launcher-root pinned to the checkout under test', async () => {
    const company = await startTmuxHostedCompany({
      slug: 'cobalt',
      repoRoot,
      env: { CHIEF_TEST_ARGV_LOG: argvLog }
    })
    try {
      const spawnedArgv = readFileSync(argvLog, 'utf8').split('\n')
      const flagIndex = spawnedArgv.indexOf('--launcher-root')
      expect(flagIndex).toBeGreaterThanOrEqual(0)
      expect(spawnedArgv[flagIndex + 1]).toBe(repoRoot)
      // And the daemon was pointed at the harness's own company directory, so
      // the pinned launcher root is the only thing this assertion is about.
      expect(spawnedArgv[spawnedArgv.indexOf('--dir') + 1]).toBe(company.dir)
    } finally {
      await company.stop()
    }
  })
})
