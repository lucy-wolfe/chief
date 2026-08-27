#!/usr/bin/env node

import { execFileSync } from 'node:child_process'
import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

function gitRaw(cwd, ...args) {
  return execFileSync('git', args, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  })
}

function git(cwd, ...args) {
  return gitRaw(cwd, ...args).trim()
}

function hasCommit(cwd, sha) {
  try {
    git(cwd, 'cat-file', '-e', `${sha}^{commit}`)
    return true
  } catch {
    return false
  }
}

function resolveCommit(cwd, sha) {
  if (hasCommit(cwd, sha)) return true
  try {
    git(cwd, 'fetch', '--no-tags', '--depth=1', 'origin', sha)
  } catch {
    return false
  }
  return hasCommit(cwd, sha)
}

function findMergeBase(cwd, baseSha, headSha) {
  const readMergeBase = () => {
    try {
      return git(cwd, 'merge-base', baseSha, headSha)
    } catch {
      return ''
    }
  }

  let mergeBase = readMergeBase()
  if (mergeBase) return mergeBase
  for (const deepenBy of [32, 128, 512]) {
    try {
      git(cwd, 'fetch', '--no-tags', `--deepen=${deepenBy}`, 'origin', baseSha, headSha)
    } catch {
      return ''
    }
    mergeBase = readMergeBase()
    if (mergeBase) return mergeBase
  }
  return ''
}

export function classifyCiScope({ eventName, baseSha, headSha, cwd = process.cwd() }) {
  if (eventName !== 'pull_request') {
    return { docsOnly: false, changed: [], reason: 'non-pull-request event' }
  }
  if (!baseSha || !headSha) {
    return { docsOnly: false, changed: [], reason: 'missing pull-request endpoint' }
  }
  if (!resolveCommit(cwd, baseSha) || !resolveCommit(cwd, headSha)) {
    return { docsOnly: false, changed: [], reason: 'unresolved pull-request endpoint' }
  }

  const mergeBase = findMergeBase(cwd, baseSha, headSha)
  if (!mergeBase) {
    return { docsOnly: false, changed: [], reason: 'unresolved pull-request history' }
  }

  const changed = [
    ...new Set(
      gitRaw(cwd, 'log', '-m', '-z', '--format=', '--name-only', '--no-renames', `${mergeBase}..${headSha}`)
        .split('\0')
        .filter(Boolean),
    ),
  ].sort()
  if (changed.length === 0) {
    return { docsOnly: false, changed, reason: 'empty pull-request diff' }
  }

  // THE TWO FILES A GUARD ACTUALLY WATCHES ARE NOT "DOCS" FOR THIS PURPOSE.
  //
  // `doc-append-only.test.mjs` exists to protect CHANGELOG.md and DECISIONS.md
  // from being reordered, reworded or truncated — #890 caught both committed at
  // ZERO LINES. But those files are `.md`, so a pull request that touched ONLY
  // them was classified docs-only and skipped every job, the repo-guards shard
  // among them. **The guard could not run on the one change class it is about.**
  // It fired only on pull requests that happened to also touch code, which is
  // the same shape as every other defect this repo has named today: a check
  // that is correct, wired, and structurally unable to see its own subject.
  //
  // Named explicitly rather than by dropping `.md` from the rule. Prose files —
  // README, this repo's many design notes — genuinely do not need a Rust
  // build, and the skip is worth real minutes on every one of them.
  //
  // The rule used to admit any path under `plans/` as documentation, so that a
  // non-`.md` plan asset counted too. `plans/` is git-ignored now: a plan is a
  // LOCAL working document, so no such path can appear in a diff, and the
  // clause could never fire again. Removed with its fixtures rather than kept
  // as a harmless-looking condition nobody could trigger.
  const GUARDED_DOCS = ['CHANGELOG.md', 'DECISIONS.md']
  const docsOnly = changed.every(
    (path) => !GUARDED_DOCS.includes(path) && path.endsWith('.md'),
  )
  return {
    docsOnly,
    changed,
    reason: docsOnly
      ? 'pull request commits touch only documentation'
      : 'pull request commits touch non-documentation files',
  }
}

function main() {
  const result = classifyCiScope({
    eventName: process.env.CI_SCOPE_EVENT_NAME ?? '',
    baseSha: process.env.CI_SCOPE_BASE_SHA ?? '',
    headSha: process.env.CI_SCOPE_HEAD_SHA ?? '',
  })
  console.log(`reason=${result.reason}`)
  console.log('changed files:')
  console.log(result.changed.join('\n'))
  const output = `docs-only=${result.docsOnly}\n`
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, output)
  else process.stdout.write(output)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main()
