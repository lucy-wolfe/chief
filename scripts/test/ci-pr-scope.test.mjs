import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { after, before, test } from 'node:test'

import { classifyCiScope } from '../ci-pr-scope.mjs'

let root

function git(...args) {
  return execFileSync(
    'git',
    ['-c', 'user.name=CI scope test', '-c', 'user.email=ci-scope@example.invalid', ...args],
    {
      cwd: root,
      encoding: 'utf8',
    },
  ).trim()
}

function commit(message) {
  git('add', '-A')
  git('commit', '--allow-empty', '-q', '-m', message)
  return git('rev-parse', 'HEAD')
}

before(() => {
  root = mkdtempSync(join(tmpdir(), 'chief-ci-pr-scope-'))
  git('init', '-q')
})

after(() => rmSync(root, { recursive: true, force: true }))

test('a final documentation commit cannot hide an earlier code change in the pull request', () => {
  const base = commit('base')
  mkdirSync(join(root, 'src'), { recursive: true })
  writeFileSync(join(root, 'src', 'change.ts'), 'export const changed = true\n')
  const codeHead = commit('code')
  writeFileSync(join(root, 'CHANGELOG.md'), '# Change\n')
  const head = commit('docs')

  assert.deepEqual(git('diff', '--name-only', codeHead, head).split('\n'), ['CHANGELOG.md'])
  assert.deepEqual(
    classifyCiScope({ eventName: 'pull_request', baseSha: base, headSha: head, cwd: root }),
    {
      docsOnly: false,
      changed: ['CHANGELOG.md', 'src/change.ts'],
      reason: 'pull request commits touch non-documentation files',
    },
  )
})

test('a code file that is changed and then reverted still triggers the full matrix', () => {
  const base = git('rev-parse', 'HEAD')
  writeFileSync(join(root, 'src', 'reverted.ts'), 'export const temporary = true\n')
  commit('temporary code')
  rmSync(join(root, 'src', 'reverted.ts'))
  const head = commit('revert code')

  assert.equal(git('diff', '--name-only', base, head), '')
  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: base,
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, false)
  assert.deepEqual(result.changed, ['src/reverted.ts'])
})

test('a documentation-only pull request can skip the full matrix', () => {
  const base = git('rev-parse', 'HEAD')
  mkdirSync(join(root, 'docs'), { recursive: true })
  writeFileSync(join(root, 'docs', 'ci.md'), '# Notes\n')
  const head = commit('a doc')

  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: base,
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, true)
  assert.deepEqual(result.changed, ['docs/ci.md'])
})

test('a base-only code commit is not classified as a pull request commit', () => {
  const common = git('rev-parse', 'HEAD')
  git('checkout', '-q', '-b', 'scope-base')
  writeFileSync(join(root, 'src', 'base-only.ts'), 'export const baseOnly = true\n')
  const base = commit('base advances')
  git('checkout', '-q', '-b', 'scope-pr', common)
  writeFileSync(join(root, 'PR.md'), '# Pull request\n')
  const head = commit('pull request docs')

  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: base,
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, true)
  assert.deepEqual(result.changed, ['PR.md'])
})

// DELETED WITH THEIR SUBJECT: two tests pinned the rule that any path under
// `plans/` counted as documentation, so that a non-`.md` plan asset (a
// diagram) skipped the matrix and a leading-space lookalike did not. `plans/`
// is git-ignored since the open-source release -- a plan is a LOCAL working
// document -- so no such path can appear in a diff and neither test could
// exercise the branch it named. The clause came out of `ci-pr-scope.mjs` and
// these came out with it, rather than being weakened to pass.
//
// What they were REALLY protecting survives below and above: an image outside
// the documentation set still runs the full matrix (a `.png` fails
// `endsWith('.md')`), and a leading space still cannot dress a path up as
// documentation.
test('an image is never documentation, whatever directory it sits in', () => {
  const base = git('rev-parse', 'HEAD')
  mkdirSync(join(root, 'docs'), { recursive: true })
  writeFileSync(join(root, 'docs', 'diagram.png'), 'documentation image\n')
  const head = commit('documentation image')
  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: base,
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, false)
  assert.deepEqual(result.changed, ['docs/diagram.png'])
})

test('a leading space is preserved, so a path cannot be laundered by whitespace', () => {
  // The surviving half of the deleted pair. git quotes a path with a leading
  // space, and an unquoting bug that trimmed it would let ` src/x.ts` be read
  // as `src/x.ts` -- or the reverse. The classifier must see the path git
  // actually reports, whitespace and all.
  const base = git('rev-parse', 'HEAD')
  mkdirSync(join(root, ' odd'), { recursive: true })
  writeFileSync(join(root, ' odd', 'code.ts'), 'export const odd = true\n')
  const head = commit('leading-space path')

  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: base,
    headSha: head,
    cwd: root,
  })
  assert.deepEqual(result.changed, [' odd/code.ts'])
  assert.equal(result.docsOnly, false, 'a non-markdown file is never documentation')
})

test('an empty pull request diff fails closed and runs the full matrix', () => {
  const head = git('rev-parse', 'HEAD')
  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: head,
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, false)
  assert.equal(result.reason, 'empty pull-request diff')
})

test('an unresolvable pull request endpoint fails closed and runs the full matrix', () => {
  const head = git('rev-parse', 'HEAD')
  const result = classifyCiScope({
    eventName: 'pull_request',
    baseSha: '0000000000000000000000000000000000000001',
    headSha: head,
    cwd: root,
  })
  assert.equal(result.docsOnly, false)
  assert.equal(result.reason, 'unresolved pull-request endpoint')
})

test('a non-pull-request event always runs the full matrix', () => {
  const result = classifyCiScope({ eventName: 'push', baseSha: '', headSha: '', cwd: root })
  assert.deepEqual(result, { docsOnly: false, changed: [], reason: 'non-pull-request event' })
})

test('a pull request touching only CHANGELOG.md or DECISIONS.md runs the full matrix, because a guard watches exactly those two files', () => {
  // THE GAP THIS CLOSES. `doc-append-only.test.mjs` protects CHANGELOG.md and
  // DECISIONS.md from being reordered, reworded or truncated — #890 caught both
  // committed at ZERO LINES. Both are `.md`, so a pull request touching only
  // them was docs-only, every job skipped, the repo-guards shard among them:
  // the guard could not run on the one change class it exists for, and fired
  // only on pull requests that happened to also touch code.
  for (const file of ['CHANGELOG.md', 'DECISIONS.md']) {
    const base = git('rev-parse', 'HEAD')
    writeFileSync(join(root, file), `- an appended entry for ${file}\n`)
    const head = commit(`append to ${file}`)

    const result = classifyCiScope({
      eventName: 'pull_request',
      baseSha: base,
      headSha: head,
      cwd: root,
    })
    assert.equal(result.docsOnly, false, `${file} must not be classified docs-only`)
    assert.deepEqual(result.changed, [file])
  }
})

test('ORDINARY prose still skips the matrix, including alongside a guarded file', () => {
  // NON-VACUITY for the rule above, both directions. The fix must not be
  // "`.md` no longer counts as documentation" — README and plans genuinely do
  // not need a Rust build, and that skip is worth real minutes on every one.
  const base = git('rev-parse', 'HEAD')
  writeFileSync(join(root, 'README.md'), '# Readme\n')
  const proseHead = commit('prose only')
  assert.equal(
    classifyCiScope({
      eventName: 'pull_request',
      baseSha: base,
      headSha: proseHead,
      cwd: root,
    }).docsOnly,
    true,
    'ordinary prose still skips',
  )

  // And ONE guarded file among many unguarded ones is enough to run everything.
  writeFileSync(join(root, 'DECISIONS.md'), '- another appended entry\n')
  const mixedHead = commit('prose plus a guarded file')
  assert.equal(
    classifyCiScope({
      eventName: 'pull_request',
      baseSha: base,
      headSha: mixedHead,
      cwd: root,
    }).docsOnly,
    false,
    'a guarded file anywhere in the diff runs the matrix',
  )
})
