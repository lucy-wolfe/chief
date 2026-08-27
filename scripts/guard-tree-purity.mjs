#!/usr/bin/env node
/**
 * The guard suite must leave the working tree exactly as it found it.
 *
 * WHY THIS EXISTS. Several guards are LIVE PROBES: they write a real file into
 * the real checkout, run the real command, and restore. That is the right
 * design -- it is the only way to prove a gate actually fails on the shape it
 * claims to catch. But a probe that cleans up incompletely turns the guard
 * corpus into a flake generator of the worst kind: the residue is left by one
 * guard and reported, one run LATER, by a different, innocent one.
 *
 * That is not hypothetical. `scripts/test/stub-import-guard.test.mjs` wrote its
 * caller probe to `apps/cli/src/legacy/...` with
 * `mkdirSync(dirname(...), { recursive: true })`, and removed only the file --
 * so every run recreated `apps/cli`, `apps/cli/src` and `apps/cli/src/legacy`
 * as empty directories, in a repository where `apps/cli` is deleted and
 * `scripts/test/no-ts-cli-stub.test.mjs` asserts it does not exist. Each guard
 * passed alone. The suite failed on its NEXT run, naming the wrong file.
 *
 * WHY `git status --porcelain` IS NOT THE CHECK. Git tracks files, not
 * directories: an empty untracked directory is invisible to
 * `git status --porcelain`, to `git ls-files --others`, and to every
 * porcelain-shaped "is the tree clean" test anyone would reach for first. The
 * residue that caused this defect produced a COMPLETELY CLEAN `git status`.
 * So the snapshot below records DIRECTORIES as first-class entries, and file
 * CONTENT hashes rather than mere presence (a probe that restores different
 * bytes than it read is the same class of defect, one layer in).
 *
 * WHAT IS NOT RESIDUE. Anything `.gitignore` covers -- build output, caches,
 * logs, `orgs/`, `runtime/`. Those are computed via real `git check-ignore`
 * semantics rather than a second hand-maintained skip list that would drift
 * from the one the repository already publishes.
 */

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync, readdirSync, readlinkSync } from 'node:fs'
import { join, relative, sep } from 'node:path'

import { skipSet } from './tree-walk-lib.mjs'

/**
 * Directories never walked and never copied: huge, machine-local, and already
 * ignored. This is a SPEED path, not the ignore policy -- `git check-ignore`
 * below is the policy. Walking `node_modules` (over a gigabyte, six figures of
 * files) to then discard it would make the snapshot cost more than the suite.
 */
export const HARD_SKIP_DIRS = skipSet()

/** Every path under `root`, repo-relative and `/`-separated, directories
 * included. Directories are the point: see this file's header. */
function walkAll(root) {
  const out = []
  const walk = (dir) => {
    let entries
    try {
      entries = readdirSync(dir, { withFileTypes: true })
    } catch {
      return
    }
    for (const entry of entries) {
      if (HARD_SKIP_DIRS.has(entry.name)) continue
      const full = join(dir, entry.name)
      const rel = relative(root, full).split(sep).join('/')
      if (entry.isDirectory()) {
        out.push({ rel, full, kind: 'dir' })
        walk(full)
      } else if (entry.isSymbolicLink()) {
        out.push({ rel, full, kind: 'link' })
      } else if (entry.isFile()) {
        out.push({ rel, full, kind: 'file' })
      }
    }
  }
  walk(root)
  return out
}

/**
 * The subset of `paths` that `.gitignore` covers, by real git semantics
 * (nested `.gitignore` files, negations, `core.excludesFile` -- all of it).
 * One batched `git check-ignore` call, never one per path.
 *
 * A tracked file is never reported here even if a pattern would match it,
 * which is exactly right: a tracked file is repository content, and a guard
 * that rewrites one has dirtied the tree no matter what `.gitignore` says.
 */
export function ignoredPaths(root, paths) {
  if (paths.length === 0) return new Set()
  const result = spawnSync('git', ['check-ignore', '--stdin', '-z'], {
    cwd: root,
    input: paths.join('\0'),
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  })
  // Exit 0 = some paths ignored, 1 = none ignored. Anything else (128: not a
  // git repository, git missing) is an instrument failure and must not be
  // read as "nothing is ignored" -- that would make every build artifact the
  // suite legitimately produces look like residue.
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(
      `[guard-tree-purity] REFUSING TO REPORT A RESULT -- \`git check-ignore\` failed in ${root} ` +
        `(status ${result.status}): ${(result.stderr ?? '').trim() || result.error?.message}. ` +
        'Without gitignore semantics this check cannot tell residue from build output.'
    )
  }
  return new Set(result.stdout.split('\0').filter(Boolean))
}

/**
 * A content-addressed picture of the working tree: repo-relative path -> a
 * short description of what is there (`dir`, `file:<sha1>`, `link:<target>`).
 * Ignored paths are dropped; `.git` and the other [`HARD_SKIP_DIRS`] are never
 * walked at all.
 */
export function snapshotTree(root) {
  const entries = walkAll(root)
  const ignored = ignoredPaths(root, entries.map((e) => e.rel))
  const snapshot = new Map()
  for (const entry of entries) {
    if (ignored.has(entry.rel)) continue
    // An ignored directory ignores everything beneath it; `git check-ignore`
    // reports the directory, not each descendant, so prune by prefix too.
    if (entry.rel.includes('/') && isUnderIgnored(entry.rel, ignored)) continue
    if (entry.kind === 'dir') snapshot.set(entry.rel, 'dir')
    else if (entry.kind === 'link') snapshot.set(entry.rel, `link:${readlinkSync(entry.full)}`)
    else snapshot.set(entry.rel, `file:${createHash('sha1').update(readFileSync(entry.full)).digest('hex')}`)
  }
  return snapshot
}

function isUnderIgnored(rel, ignored) {
  const parts = rel.split('/')
  for (let i = 1; i < parts.length; i += 1) {
    if (ignored.has(parts.slice(0, i).join('/'))) return true
  }
  return false
}

/** What changed between two snapshots: `{ added, removed, changed }`, each a
 * sorted list of repo-relative paths. */
export function diffSnapshots(before, after) {
  const added = []
  const removed = []
  const changed = []
  for (const [path, value] of after) {
    if (!before.has(path)) added.push(path)
    else if (before.get(path) !== value) changed.push(path)
  }
  for (const path of before.keys()) {
    if (!after.has(path)) removed.push(path)
  }
  return { added: added.sort(), removed: removed.sort(), changed: changed.sort() }
}

/** True when a diff found nothing at all. */
export function isClean(diff) {
  return diff.added.length === 0 && diff.removed.length === 0 && diff.changed.length === 0
}

/** A diff as an operator-readable block, naming every path. */
export function describeDiff(diff) {
  const lines = []
  for (const path of diff.added) lines.push(`  + ${path}`)
  for (const path of diff.removed) lines.push(`  - ${path}`)
  for (const path of diff.changed) lines.push(`  ~ ${path}`)
  return lines.join('\n')
}
