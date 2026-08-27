// Chief never puppets a live agent: no product path types text or Enter into
// an agent's pane.
//
// # Why this is an ABSENCE pin, and why an absence needs one
//
// Every other rule in this repository is pinned by a test that watches
// something HAPPEN. This one is pinned by a test that watches something NOT
// happen, and an absence has no natural witness -- the code that would
// violate it does not exist yet, so there is nothing to assert about. Until
// now the rule lived in one doc comment (`actuate/resident.rs`), which is
// prose, and prose is what a future change reads past.
//
// The rule itself: a message is DELIVERED through the mailbox and the person
// is woken by a targeted reconcile. It is never delivered by typing it at
// their terminal. Typing at a live agent races its own input handling, cannot
// be observed to have landed, and produces the false-ownership state the
// whole desired-state model exists to avoid. The `send-keys` doorbells that
// once told every rail to re-read something were deleted for exactly this.
//
// # What makes this greppable rather than vague
//
// `tmux send-keys` has three non-typing forms, and only those are allowed in
// product code:
//
//   -M   forward a mouse event verbatim (the rail's click path)
//   -H   send hex bytes (the click bench's synthetic mouse input)
//   -X   drive copy-mode (`-X cancel`, which ends a selection)
//
// Anything else is KEYSTROKES. So the check is not "does the string
// `send-keys` appear" -- which would fire on every comment explaining this
// rule, including this one -- but "does a send-keys ARGUMENT LIST carry
// literal keys". That distinction is the whole guard.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const root = join(__dirname, '..', '..')

/** The tmux flags that make a `send-keys` something other than typing. */
const NON_TYPING_FLAGS = ['-M', '-H', '-X']

/**
 * Call sites that DO type keystrokes and are allowed to, each with the reason.
 *
 * IT IS EMPTY, AND THAT IS THE FINISHED STATE -- which was itself a finding.
 * This list opened with the founder bootstrap on the belief that it types into
 * the operator's outer terminal to launch a session. It does not: every
 * `send-keys` in `founder.rs` is inside `#[cfg(test)]`, and the production
 * half of that file contains none at all. So the rule holds without an
 * exception, and the honest statement is the stronger one -- NO product path
 * types at a pane.
 *
 * The row came out rather than being kept as harmless documentation, because
 * an allowlist entry with no subject is dead config, and dead config in an
 * allowlist is what hides a real finding. The liveness test below is what
 * caught it, which is the same test that will catch the next one.
 *
 * @type {ReadonlyArray<{file: string, why: string}>}
 */
export const ALLOWED_TYPING_SITES = []

/** Is this a source file the rule applies to -- product Rust, not a test? */
function isProductSource(path) {
  if (!path.endsWith('.rs')) return false
  if (path.includes('/tests/')) return false
  if (path.endsWith('tests.rs') || path.includes('/tests/')) return false
  return path.startsWith('apps/chiefd/crates/')
}

/**
 * Every product `send-keys` CALL SITE that carries literal keystrokes.
 *
 * Two things make this read the source rather than a single grep line, and
 * both were found by the guard being wrong first:
 *
 * 1. THE ARGV SPANS LINES. `"send-keys",` sits alone on one line and its
 *    `-M` two lines below, so a line-scoped check reports every correct
 *    mouse-forwarding call as typing. The window is read instead.
 * 2. `send-keys` ALSO APPEARS AS A BARE VERB NAME, in lists of tmux
 *    mutations that other guards maintain. Those are not call sites at all.
 *    A real call site names its target pane, so a window with no `-t` is
 *    skipped -- which is a property of the invocation, not a file exemption.
 */
export function typingCallSites(cwd = root) {
  let files
  try {
    files = execFileSync('git', ['grep', '-lI', 'send-keys', '--', 'apps/chiefd/crates'], {
      cwd,
      encoding: 'utf8',
    })
      .split('\n')
      .filter(Boolean)
  } catch (error) {
    if (error.status === 1) {
      throw new Error(
        'CANNOT CHECK: no `send-keys` occurrence found at all. This guard is derived from the real ' +
          'call sites, and finding none means the search is wrong, not that the rule is satisfied -- ' +
          'silence is never the green.'
      )
    }
    throw error
  }
  const offences = []
  for (const path of files) {
    if (!isProductSource(path)) continue
    // `#[cfg(test)]` bodies are cut first: test code mints its own throwaway
    // panes, and typing into a pane you just created is not puppeting anyone.
    // Same idiom the Rust-side source guards in `tmux.rs` already use.
    const source = readFileSync(join(cwd, path), 'utf8').split('#[cfg(test)]')[0]
    const lines = source.split('\n')
    for (const [index, line] of lines.entries()) {
      if (!line.includes('send-keys')) continue
      const window = lines.slice(index, index + 8).join('\n')
      // Not an invocation: a bare verb name in a list of tmux mutations.
      if (!window.includes('"-t"') && !window.includes("'-t'") && !window.includes('"-t",')) continue
      if (NON_TYPING_FLAGS.some((flag) => window.includes(`"${flag}"`))) continue
      offences.push({ path, line: `${path}:${index + 1}:${line.trim()}` })
    }
  }
  return offences
}

test('no product path types keystrokes at a pane, except the named non-agent sites', () => {
  const offences = typingCallSites(root)
  const unexplained = offences.filter(
    (o) => !ALLOWED_TYPING_SITES.some((allowed) => o.path === allowed.file)
  )
  assert.deepEqual(
    unexplained.map((o) => o.line),
    [],
    'A product path is typing at a pane. A message is DELIVERED through the mailbox and the person is ' +
      'woken by a targeted reconcile -- never by typing at their terminal, which races their own input ' +
      'handling and cannot be observed to have landed. If the pane genuinely does not belong to a live ' +
      'agent, add it to ALLOWED_TYPING_SITES with the reason.'
  )
})

test('every allowed typing site still exists, and still types', () => {
  // The liveness half: an allowlist row whose subject has gone is dead
  // config, and dead config in an allowlist is what hides a real finding.
  const offences = typingCallSites(root)
  for (const allowed of ALLOWED_TYPING_SITES) {
    assert.ok(
      offences.some((o) => o.path === allowed.file),
      `${allowed.file} is allowed to type but no longer does -- remove the row. Ruling was: ${allowed.why}`
    )
    assert.ok(allowed.why.length > 30, `${allowed.file} has no recorded reason`)
  }
})

test('NON-VACUITY: the mouse and copy-mode forms are NOT reported, and a bare key IS', () => {
  // A grep for `send-keys` alone would fire on every comment explaining this
  // rule, including this file's own header. The discriminator is the flag.
  const forwarded = '            "send-keys", "-t", pane, "-M",'
  const typed = '            "send-keys", "-t", pane, "Enter",'
  assert.ok(NON_TYPING_FLAGS.some((flag) => forwarded.includes(`"${flag}"`)), 'a forwarded mouse event must be allowed')
  assert.ok(!NON_TYPING_FLAGS.some((flag) => typed.includes(`"${flag}"`)), 'a literal key must be reported')
})
