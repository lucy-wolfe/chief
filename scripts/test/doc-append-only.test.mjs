// #890: two merges committed CHANGELOG.md and DECISIONS.md at ZERO LINES —
// a Python conflict-resolution helper's `open(p, 'w').writelines([... for l
// in open(p) ...])` truncated the file before the comprehension that reads
// it ever ran, so every "restored" merge silently wrote back nothing.
// Wiped 4784 and 4706 lines across two commits. Every other gate reported
// green: typecheck, build, lint, knip, nine unit tasks, eight repo guards,
// clippy, cargo fmt --check, 2430 cargo tests. NOTHING READS EITHER FILE.
// It surfaced only because a later merge's both-sides check printed
// `ours: +0 -4784` and a human read the number.
//
// THE DESIGN POINT: a "file must be non-empty" assertion catches this
// specific incident and is cheap, but the real failure mode is not
// emptiness -- it is UNEXPLAINED DELETION. A merge that silently drops 400
// lines while adding 20 passes a non-empty check and loses just as much.
// Both files are APPEND-MOSTLY: entries are added, essentially never
// removed. That property is checkable and much stronger than a size floor
// -- a strict-superset check against each parent commit, exactly the
// verification `8b6d2b33`'s recovery performed by hand (sorted both
// versions, confirmed zero pre-damage lines absent).
//
// WHAT "ENTRY" MEANS: both files are flat bullet logs -- every top-level
// entry starts at column 0 with `- ` (dated CHANGELOG/DECISIONS lines);
// continuation lines (including occasional nested `  - ` sub-bullets
// within one entry's body) are indented and are NOT separate entries. An
// entry's FINGERPRINT is its first line, trimmed -- stable across a
// correction to the entry's later lines (the escape hatch below), but
// unique enough in practice that no two distinct entries share one (dated,
// named, or issue-numbered openings).
//
// ESCAPE HATCH: a legitimate edit -- correcting a typo in an EXISTING
// entry's first line -- would otherwise register as "the old fingerprint
// vanished." DOCUMENTED_FIRST_LINE_EDITS below is the explicit, reviewed
// registry for exactly that case (mirrors sql-only-state.test.mjs's own
// ALLOWLIST idiom): each entry names the file, the exact OLD first line
// that legitimately disappeared, and why. A missing fingerprint not listed
// here fails, naming the fingerprint and the file.
//
// CONSUMED, NOT DELETED-ON-A-TIMER (the #921 lesson): an entry is only
// "used" in the ONE commit whose parent still carries its `oldFirstLine`.
// From the very next commit onward, that fingerprint is gone from every
// parent by construction (the fix already landed and propagated) -- so an
// entry that DID exactly its one job goes "stale" (unused) on every future
// run forever, indistinguishable from an entry nobody ever wired up
// correctly. Three real #921 exceptions hit this: they were used in the
// commit documenting the fix, then reds every commit after because the
// registry has no way to say "this already did its job." `consumed: true`
// on an entry means exactly that -- it is excluded from the staleness scan
// entirely (see `checkAppendOnly` below), kept in the file as a permanent,
// reviewable record of what happened and why, rather than deleted the
// instant it stops matching. An entry WITHOUT `consumed: true` that never
// matches is still a real staleness violation -- the check that catches
// "this exception rotted and nobody noticed" is preserved for exactly the
// entries that haven't been marked done.
//
// COMPARISON BASIS: every parent of the commit under test (one for an
// ordinary commit, two for a merge commit -- exactly the shape that broke
// here). A parent that lacks the file entirely contributes no fingerprints
// (never a reason to fail). Every fingerprint present in ANY parent must
// still be present in the commit under test, or be a listed exception.
//
// NON-VACUOUSNESS (#848's lesson, applied here): a parent's fingerprint
// set that comes back EMPTY when the file demonstrably has content at that
// ref is refused, not silently treated as "nothing to check" -- the exact
// failure-mode shape (git-show returning nothing) that produced #890 in
// the first place.
//
// #921: THE GLUED-ENTRY CLASS. entryFingerprints() finds a new entry only
// at the start of a `\n`-delimited line -- so a whole second entry squashed
// onto its predecessor's line, with the separating newline dropped (a
// squashed/rebased merge's own defect, same family as #890's write
// truncation), is completely invisible to it: no fingerprint, no
// append-only coverage, indistinguishable from prose inside the first
// entry's body. Three real instances sat in CHANGELOG.md this way. Same
// shape as the vitest exclude-list parser fixed elsewhere tonight -- a
// parse that silently swallows everything after an unexpected character.
// `gluedEntryViolations` closes it: any `- **` bullet-open preceded by a
// non-whitespace, non-newline character is a glue point -- CHECKED FOR
// CHANGELOG.md ONLY (`GLUED_ENTRY_FILES` below). CHANGELOG.md's own
// convention makes this sound: every one of its bold-headline entries
// legitimately starts `- **`, and `- **` never otherwise appears mid-line
// there (confirmed empirically: zero matches on the real file once its
// three glued entries are fixed), so ANY mid-line occurrence is
// definitionally a glue. DECISIONS.md has NO such invariant -- its entries
// open `- YYYY-MM-DD ...` and routinely use an inline `-- **bold clause**`
// as ordinary prose WITHIN one entry's own body (real example found while
// building this: `- 2026-07-25 (#474 ...) -- **A test helper that
// overwrites...`), so the identical pattern there is a false positive, not
// a defect -- checked and rejected before shipping this, not assumed.
import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

// #890's own scope: CHANGELOG.md and DECISIONS.md, the two files actually
// destroyed. plans/*.md is NOT extended to here -- considered per the
// issue's own ask, and deliberately deferred: each plan file is a single
// story's own document (not one shared append-mostly log every story adds
// an entry to), so "a previously-present LINE disappearing" is a much
// weaker signal there -- a plan file's own author routinely rewrites its
// checklist/scope sections as a story proceeds (see this very story's own
// plan file, edited repeatedly as facts changed, per CLAUDE.md's own
// instruction to do so). Extending this exact mechanism to plans/*.md
// would flag ordinary, sanctioned plan-file editing as a violation on
// every story. A different, plan-specific guard (e.g. "a plan file is
// never fully DELETED without a completion marker") would fit that
// property better; out of scope for #890, worth a follow-up issue rather
// than silently declining to mention it.
const TRACKED_FILES = ['CHANGELOG.md', 'DECISIONS.md']

// #921: files where a bare `- **` bullet-open reliably means "new
// top-level entry" -- see the header comment above for why DECISIONS.md is
// excluded (its own convention uses `-- **bold**` legitimately mid-entry).
const GLUED_ENTRY_FILES = new Set(['CHANGELOG.md'])

/**
 * THE ONE-TIME PUBLIC-LEDGER RESET, and why it is a separate concept from the
 * exception registry below rather than ten thousand rows in it.
 *
 * On 2026-08-25 both ledgers were replaced with fresh files for the
 * open-source release. The private ledger is archived, not published: about
 * 16,800 lines of internal narrative naming hosts, incidents and colleagues.
 * That deletes every fingerprint the append-only check compares against, which
 * is exactly the shape this guard exists to refuse -- correctly, because the
 * guard cannot tell a reviewed replacement from the truncation that produced
 * #890.
 *
 * DOCUMENTED_FIRST_LINE_EDITS is NOT the mechanism for this. It is per-entry
 * and reviewed per-entry, and filling it with thousands of rows would say, to
 * every future reader, that an append-only file may be reworded whenever the
 * reason seems good enough. That is a repeal of the rule wearing the rule's
 * clothes. The reset is therefore named as its own thing, with its own
 * narrower shape.
 *
 * DETECTED BY CONTENT, NEVER BY A COMMIT SHA. Each fresh file opens with a
 * preamble entry whose first line begins with the sentinel below. A parent's
 * fingerprints are skipped for one file when the file at HEAD carries that
 * sentinel and the parent does NOT -- that parent is the pre-reset ledger, and
 * the commit under test is the reset. A SHA would not survive the squash merge
 * this lands through; the content condition is re-evaluated against whatever
 * parent the branch actually meets.
 *
 * SELF-LIMITING, WHICH IS THE POINT. From the next commit onward every parent
 * also carries the sentinel, the condition is false, and ordinary append-only
 * owns both files again with no residue -- no exception row to go stale, and
 * nothing to remember to remove. A SECOND reset cannot ride on this one: it
 * would need somebody to edit this constant with a new sentinel and a reason,
 * in a reviewed diff, which is the visibility a decision of that size should
 * cost.
 *
 * NOT A WEAKENING OF THE OTHER CHECKS. Malformed prefixes, glued entries and
 * the non-vacuity refusal all still run against HEAD and against the parent.
 * The reset skips exactly one comparison: "was every parent entry carried
 * forward", which is the one question a deliberate replacement answers "no" to
 * on purpose.
 */
/**
 * WHAT THIS GUARD CAN SEE, said plainly so nobody mistakes silence for a pass.
 *
 * On a pull request, CI checks out the SYNTHETIC MERGE COMMIT, whose parents
 * are `main` and the branch head. Intermediate branch commits are never any
 * checked commit's parent, so this guard never compares them against anything.
 *
 * The consequence: a first-line edit made BETWEEN two branch pushes is
 * structurally invisible here. Not missed -- invisible, by the shape of what
 * CI hands the guard. Widening the comparison logic will not change it,
 * because the commits in question are not in the graph this guard is given.
 *
 * That is an OBSERVATION WINDOW, not a defect, and the honest thing is to name
 * it rather than let a green be read as a statement about branch-internal
 * ledger history. What protects that history is REVIEW -- reading the file at
 * the pushed SHA -- and not this file. A reviewer who assumes otherwise is
 * trusting a check that was never asked the question.
 */
const PUBLIC_LEDGER_RESET = {
  date: '2026-08-25',
  reason:
    'The open-source release. The private ledger is archived, not published -- see the first entry of DECISIONS.md.',
  /** First-line PREFIXES, not whole fingerprints: the preamble entries are one
   *  long line each, and a prefix keeps this constant readable without letting
   *  it drift from the file it names. */
  sentinelPrefixes: {
    'CHANGELOG.md': '- **The public changelog starts here.**',
    'DECISIONS.md': '- 2026-08-25 — CHANGELOG.md and DECISIONS.md start FRESH in the public tree',
  },
}

/** Every fingerprint in `fingerprints` that opens with `file`'s reset
 *  sentinel. Returned as a LIST, not a boolean, because the count is the
 *  thing the caller has to reason about: see `resetSentinelState`. */
function resetSentinelMatches(file, fingerprints) {
  const prefix = PUBLIC_LEDGER_RESET.sentinelPrefixes[file]
  if (prefix === undefined) return []
  return [...fingerprints].filter((fingerprint) => fingerprint.startsWith(prefix))
}

/** Is `fingerprints` a ledger that has already been reset -- i.e. does it
 *  carry the sentinel entry for `file`? */
function carriesResetSentinel(file, fingerprints) {
  return resetSentinelMatches(file, fingerprints).length > 0
}

/**
 * What the sentinel says about one (file, ref) pair, or a REFUSAL.
 *
 * `carriesResetSentinel` answers "any fingerprint starts with the prefix",
 * and that is a decision made on an input the design does not cover. The
 * reset is defined as ONE preamble entry; two entries carrying the sentinel
 * is a state nobody designed, and silently interpreting it as "reset" would
 * grant the skip on a file somebody had appended a second sentinel-shaped
 * entry to. This repository's standing convention is that a guard refuses in
 * words when it cannot understand its input, and never passes quietly on one.
 *
 * Returns `{ present: boolean }`, or `{ refusal: string }` naming what was
 * seen.
 */
function resetSentinelState(file, fingerprints, ref) {
  const matches = resetSentinelMatches(file, fingerprints)
  if (matches.length > 1) {
    return {
      refusal:
        `${file} at ${ref} carries ${matches.length} entries opening with the public-ledger reset ` +
        'sentinel, and the reset is defined as exactly ONE preamble entry. Refusing to guess which ' +
        'is the reset:\n' +
        matches.map((m) => `  ${JSON.stringify(m)}`).join('\n'),
    }
  }
  return { present: matches.length === 1 }
}

// The reviewed exception registry. Each entry is consumed exactly once per
// guard run (see NON-VACUOUSNESS check on unused entries) so a stale
// exception -- one whose "old" fingerprint no longer matches anything --
// is itself flagged rather than silently kept forever.
/** @type {Array<{file: string, oldFirstLine: string, reason: string, consumed?: boolean}>} */
const DOCUMENTED_FIRST_LINE_EDITS = [
  {
    file: 'CHANGELOG.md',
    oldFirstLine: "- **fix(identity): a concurrent mint can no longer orphan a person's enrolled identity key.** On a real company (`4cc439341aa9`, 2026-08-20T00:23Z) six of twenty-one people could not start, each behind the card *\"a different identity key is already enrolled for this person; rotation is explicit and has not been performed\"*. `ensure_identity_key` tested `path.exists()` and then wrote through `publish_atomically`, which publishes by `rename(2)` and therefore REPLACES. Four provisioning passes ran at once inside one daemon, so two of them each minted a key for the same person: the first enrolled its key in `identities`, and the second published its own key over the file 3 ms later. Rotation is deliberate and the trust table is never re-pointed, so the disagreement was permanent and the person was withheld from every launch after. The mint now goes through `files::create_exclusively` \u2014 a synced sibling temp plus `link(2)`, which fails `EEXIST` in one syscall instead of replacing \u2014 so the FIRST writer owns the anchor and a losing minter reports `false` and keeps the winner's key. That holds against two tasks in one process and two daemons on one directory alike, which is why it is the write and not a lock. `link(2)` rather than `renameat2(RENAME_NOREPLACE)` because the latter is Linux-only. The refusal keeps its subject: a key genuinely swapped underneath an enrolled person is still withheld by name, pinned by `a_swapped_key_still_withholds_that_person_from_launch`. Red-first: `a_late_minter_never_orphans_the_key_that_was_already_enrolled` reproduces the operator's sentence word for word by holding one minter inside its mint while the other enrols, and `two_minters_that_race_leave_one_anchor_and_one_creator` reported two creators of one file. The reported cause \u2014 a stale key left by a previous company in the same reused directory \u2014 was measured FALSE on the box: every key file was created during this company's run. That path was already correct and is now pinned too (`a_key_left_by_a_previous_company_is_adopted_and_starts_its_person`): a surviving key with no `identities` row is ADOPTED as the new company's anchor. TEST_SUITE Case 41 carries the live form.",
    reason:
      'PR #1186. The entry cited TEST_SUITE Case 41; merging origin/main brought its OWN Case 41 (fix/wake-fence-swept, #1185), so this case renumbered to 42 and the citation had to follow it or point a reader at an unrelated case. Body otherwise unchanged.',
    consumed: true,
  },
  // A genuine typo fix to an existing entry's own FIRST line legitimately
  // changes its fingerprint. Add an entry here, reviewed, naming the file,
  // the exact old first line, and why; mark it consumed after that commit.
  // { file: 'CHANGELOG.md', oldFirstLine: '- guard (#XXX): mis-speled headline', reason: 'PR #YYYY, typo fix, entry body unchanged' }
  {
    file: 'CHANGELOG.md',
    oldFirstLine: '- **perf (CI): Rust verification now runs in bounded parallel lanes.** The workspace test gate runs six package groups on separate runners, with an exact derived test and suite count for every group; this prevents the previous all-workspace compile from exhausting the runner disk before tests started. Clippy, the macOS cross-target check, and the release-profile check also run as independent lanes behind their existing stable required status. The repo guard worktree creator now creates child worktrees serially before starting parallel guard processes, which removes the incomplete-worktree race found on GitHub.',
    reason: 'PR #1041, CI guard race clarification, entry body unchanged',
    consumed: true,
  },
  {
    file: 'CHANGELOG.md',
    oldFirstLine: '- **fix(sidebar): tmux viewport hooks now use the exact event client.** The hook admits only its own `hook_client`, then increments one server-global numeric generation and stores that request with the exact organization and owner before a silent background callback revalidates the client. Later control, ignore-size, detached, switched, wrong-session, blank-size, stale, and same-name recreated-session callbacks cannot replace or clear a valid ordinary request, publish geometry, or put a rail in tmux view mode. Publication and refusal cleanup require the expected organization, generation, and owner in one atomic guard; manual geometry and human sidebar preferences are unchanged.',
    reason: '2026-08-17 integration repair: restore the prior exact-client entry and append the server-generation correction as a separate newest entry',
    consumed: true,
  },
  {
    file: 'CHANGELOG.md',
    oldFirstLine: '- **fix(colors): every product card now keeps readable text in Light and Dark.** Role chips use truecolor ink and a hue-preserving dark identity ground, so the live Test Engineer periwinkle now renders white at 5.04:1 instead of terminal-black at 3.04:1. Pi status, detail, Markdown, selected, user, custom-message, and tool cards resolve every text/background pair above WCAG AA; exact red status glyphs keep their separate graphical-contrast rule. Existing homes refresh only Chief-owned organization theme files during normal materialization, so the fix reaches running companies without replacing agent settings or content.',
    reason: '2026-08-18 integration repair: restore the shipped secure-refresh wording after an attempted append-only repair restored an obsolete first line',
    consumed: true,
  },
  {
    file: 'DECISIONS.md',
    oldFirstLine: '2026-08-17 — CARD COLORS ARE PROVED FROM THEIR RESOLVED RGB PAIRS. Every Pi text token must have at least 4.5:1 contrast on every product card surface in Light and Dark, and selected sidebar card text has the same floor; status glyphs use the 3:1 graphical floor and sleeping stays exact `#ff0000`. Raw mid-luminance identity accents are stable inputs, while role-chip grounds keep their hue at L<=0.16 and choose truecolor black or white from the final ground. Chief-owned organization theme files refresh in existing homes on normal materialization, while agent settings and all other home content remain create-once.',
    reason: '2026-08-18 integration repair: remove the duplicate older card-color decision appended during rebase; the secure refresh decision and later correction remain',
    consumed: true,
  },
  {
    file: 'DECISIONS.md',
    oldFirstLine: "2026-08-17 — A COLD FOCUS CLICK PUBLISHES ONE FINAL WAKING BODY IN THE FIRST TMUX FRAME. Parked furniture is respawned in place; an occupied focus atomically returns every live occupant to its resolved home and creates the clicked person's final WAKING pane in the vacated cell; a rail-only focus creates that final body directly. Unknown homes, stray panes, and mixed ownership fail closed before writes, and no current person's process is killed for a view handoff.",
    reason: '2026-08-18 integration repair: remove the superseded cold-focus decision after absent-home placement shipped; the later complete decision remains',
    consumed: true,
  },
  {
    file: 'CHANGELOG.md',
    oldFirstLine: "- **fix(sidebar): a cold person click now uses one final focus pane from its first frame.** Parked focus furniture immediately shows the clicked person's startup message and role border. If another person owns focus, one atomic tmux handoff keeps that process alive, returns it home, and publishes only the clicked person's final startup body; a rail-only transition publishes that final body directly. The actuator starts Pi in the same WAKING pane, and an absent empty return-home window no longer blocks converge or causes a later second pane.",
    reason: '2026-08-18 integration repair: remove the superseded cold-focus changelog draft after absent-home placement shipped; the newer complete entry remains',
    consumed: true,
  },
  {
    file: 'CHANGELOG.md',
    oldFirstLine: '- **feat(sidebar): sleeping people now open a wake card before Chief starts them.** Selecting a sleeping person shows their exact name, company role, and backend-resolved effective Pi model in the permanent focus body without sending a wake request. The model resolver follows only the active epoch-filtered transcript branch and uses bounded, contained global/project settings reads only when that branch has no model pair. Mouse, Enter, or Space activation of `Wake Up` validates the exact company/session/pane, changes that same pane to an animated `Waking up…` state, and sends one idempotent wake; the actuator revalidates the pane before it replaces the card with Pi in place, with no generic frame, raw title, extra pane, or rail-width change.',
    reason: '2026-08-18 sleeping-card integration repair: remove the superseded pre-cache feature entry; the complete cached-authority entry remains',
    consumed: true,
  },
  {
    file: 'CHANGELOG.md',
    oldFirstLine: '- **feat(sidebar): sleeping people now open a wake card before Chief starts them.** Selecting a sleeping person shows their exact name, company role, and effective Pi model in the permanent focus body without sending a wake request. Mouse, Enter, or Space activation of `Wake Up` changes that same pane to an animated `Waking up…` state and sends one idempotent wake; the actuator later replaces the card with Pi in place, with no generic frame, raw title, extra pane, or rail-width change.',
    reason: '2026-08-18 sleeping-card integration repair: remove the superseded settings-era feature entry; the complete backend-authority entry remains',
    consumed: true,
  },
  {
    file: 'DECISIONS.md',
    oldFirstLine: "- 2026-08-18 — SELECTING A SLEEPING PERSON AND WAKING THEM ARE TWO DIFFERENT OPERATOR ACTIONS. The row click reserves the permanent focus body for a Chief-owned card and uses a backend-owned typed model projection from the epoch-filtered active Pi transcript chain, with bounded contained settings reads only when no transcript pair exists; it changes no launch intent. Only the card's focused `Wake Up` button may atomically validate its exact company/session/pane, change the pane-local marker from sleeping to waking, and ask ChiefD once; the actuator revalidates and respawns that same pane into Pi.",
    reason: '2026-08-18 sleeping-card integration repair: remove the superseded pre-cache decision; the complete cached-authority decision remains',
    consumed: true,
  },
  {
    file: 'DECISIONS.md',
    oldFirstLine: "- 2026-08-18 — SELECTING A SLEEPING PERSON AND WAKING THEM ARE TWO DIFFERENT OPERATOR ACTIONS. The row click reserves the permanent focus body for a Chief-owned card and reads the exact role from the roster plus the effective provider/model from that person's Pi settings, but it changes no launch intent. Only the card's focused `Wake Up` button may change the pane-local marker from sleeping to waking and ask ChiefD once; the actuator then claims and respawns that same pane into Pi.",
    reason: '2026-08-18 sleeping-card integration repair: remove the superseded settings-era decision; the complete backend-authority decision remains',
    consumed: true,
  },
]

// #890's own real CHANGELOG.md is 1.8MB -- comfortably past Node's DEFAULT
// execFileSync maxBuffer (1MB). A too-small buffer throws ENOBUFS, which a
// naive catch-and-treat-as-absent (the very first version of this
// function) silently misread as "the file doesn't exist at this ref" --
// exactly the class of silent failure this whole guard exists to catch,
// caught here by demonstrating the guard against the repo's OWN real
// files rather than fixtures alone. 64MB is comfortably past any size
// these two files could plausibly reach for a long time.
const GIT_SHOW_MAX_BUFFER = 64 * 1024 * 1024

function gitShow(cwd, ref, file) {
  try {
    // stderr left on the default `pipe` (captured into the error object on
    // failure, never inherited to this process's own stderr) -- NOT
    // `ignore`. An earlier version of this function set `stdio: [...,
    // 'ignore']` to quiet the expected "path does not exist" case, which
    // silently discarded the very text the catch block below needs to
    // classify the failure -- everything fell through to "unexpected,"
    // caught only by running this guard against the real, large
    // CHANGELOG.md/DECISIONS.md rather than fixtures alone.
    return execFileSync('git', ['show', `${ref}:${file}`], {
      cwd,
      encoding: 'utf8',
      maxBuffer: GIT_SHOW_MAX_BUFFER
    })
  } catch (error) {
    // Only "the path does not exist at this ref" (git's own fatal message,
    // captured in `.stderr` -- `execFileSync` does not fold it into
    // `.message`) is treated as absence. Anything else -- ENOBUFS, a
    // permissions error, git itself crashing -- is a genuine failure this
    // guard must not silently launder into "nothing to check here."
    const stderrText =
      error && typeof error === 'object' && 'stderr' in error && typeof error.stderr === 'string'
        ? error.stderr
        : ''
    const message = error instanceof Error ? error.message : String(error)
    const combined = `${stderrText}\n${message}`
    if (/does not exist in/.test(combined) || /exists on disk, but not in/.test(combined)) {
      return undefined
    }
    throw new Error(`git show ${ref}:${file} failed unexpectedly (not "path absent"): ${combined}`)
  }
}

function parentsOf(cwd, ref) {
  const out = execFileSync('git', ['rev-list', '--parents', '-n', '1', ref], {
    cwd,
    encoding: 'utf8'
  }).trim()
  const shas = out.split(/\s+/)
  return shas.slice(1) // drop the commit itself, keep its parent(s)
}

/** Top-level entries only: a line starting at column 0 with `- ` opens a
 * new entry; every other line (including indented `  - ` sub-bullets) is a
 * continuation of the current entry. Returns each entry's first line,
 * trimmed, as its fingerprint. */
function entryFingerprints(content) {
  const fingerprints = []
  for (const line of content.split('\n')) {
    if (line.startsWith('- ')) fingerprints.push(line.trim())
  }
  return fingerprints
}

/** A line beginning `+-` at column 0 is neither a continuation nor a valid
 * top-level entry. Treat it as a format violation instead of silently
 * dropping it from the fingerprint scan. */
function malformedTopLevelPrefixViolations(content, file, ref) {
  const violations = []
  for (const [index, line] of content.split('\n').entries()) {
    if (line.startsWith('+-')) {
      violations.push(
        `${file}:${index + 1} at ${ref} has malformed top-level entry prefix "+-"; expected "- ".`
      )
    }
  }
  return violations
}

/** #921: a `- **` bullet-open with NO whitespace or newline before it is a
 * second entry glued onto its predecessor's line with the separating
 * newline dropped -- exactly the shape a squashed/rebased merge produced
 * three times in this file (a whole prior entry's closing `.` or `)`
 * immediately followed by the next entry's `- **`, on one physical line).
 * `entryFingerprints` only recognizes a NEW entry at the start of a `\n`-
 * delimited line, so a glued second entry is invisible to it: it never
 * gets its own fingerprint, and a later append-only check can't tell it
 * apart from prose inside the first entry's body. Preceding whitespace
 * (a newline OR a space, e.g. this file's own documented nested `  - `
 * sub-bullets) is never flagged -- only zero separation is a glue. */
function gluedEntryViolations(content, file, ref) {
  const violations = []
  const pattern = /[^\s\n](- \*\*)/g
  for (const match of content.matchAll(pattern)) {
    const upToMatch = content.slice(0, match.index + 1)
    const line = upToMatch.split('\n').length
    violations.push(
      `${file}:${line} at ${ref} has a second entry glued onto the previous line with no separating newline ` +
        `(found "${content.slice(match.index, match.index + 20)}…"). This entry is invisible to entryFingerprints()'s ` +
        `line-based scan until the newline is restored.`
    )
  }
  return violations
}

/**
 * Compare `headRef`'s tracked files against every parent of `headRef`.
 * Returns `{ violations, checked }` -- `violations` is `[]` when clean;
 * `checked` records exactly what was compared (per the issue's own
 * acceptance criterion: "the check reports what it compared").
 *
 * #925: `exceptions` is an explicit parameter, defaulting to the real,
 * module-level `DOCUMENTED_FIRST_LINE_EDITS` registry -- it used to be read
 * directly from that module-level array with no parameter at all, which
 * meant the stale-exception check below iterated the WHOLE registry on
 * EVERY call, including this file's own fixture-repo self-tests. A real,
 * permanent entry (e.g. a genuine escape-hatch use for CHANGELOG.md/
 * DECISIONS.md) would then be flagged "stale" inside every unrelated
 * fixture check, because a synthetic fixture's history obviously never
 * contains that entry's `oldFirstLine` -- so the escape hatch could never
 * be used without breaking three of this file's own self-tests. Passing an
 * explicit `exceptions` array per call (each fixture test below passes its
 * own scoped array, usually `[]`) makes each call's staleness check mean
 * "did the exceptions THIS CALL is responsible for get used", not "did
 * every exception this repo has ever documented get used in this one run"
 * -- the real production test (below) is the only caller that keeps the
 * default, so it is the only one that actually exercises the real
 * registry.
 */
function checkAppendOnly(cwd, headRef, files = TRACKED_FILES, exceptions = DOCUMENTED_FIRST_LINE_EDITS) {
  const parents = parentsOf(cwd, headRef)
  const violations = []
  const headContents = new Map()
  for (const file of files) {
    const content = gitShow(cwd, headRef, file)
    if (content === undefined) {
      headContents.set(file, new Set())
      continue
    }
    violations.push(...malformedTopLevelPrefixViolations(content, file, headRef))
    if (GLUED_ENTRY_FILES.has(file)) violations.push(...gluedEntryViolations(content, file, headRef))
    headContents.set(file, new Set(entryFingerprints(content)))
  }

  const checked = { headRef, parents, files, parentEntryCounts: {} }
  const usedExceptions = new Set()

  for (const parent of parents) {
    for (const file of files) {
      const parentContent = gitShow(cwd, parent, file)
      if (parentContent === undefined) continue // file didn't exist at this parent
      const parentFingerprints = entryFingerprints(parentContent)
      checked.parentEntryCounts[`${parent}:${file}`] = parentFingerprints.length
      // NON-VACUOUSNESS: the file has real content at this ref (verified by
      // gitShow succeeding) but zero recognized entries would mean the
      // entry-boundary parser itself is broken, or the file's shape
      // changed out from under this guard -- either way, silently treating
      // that as "nothing to check" is exactly the #890/#848 failure shape.
      if (parentFingerprints.length === 0) {
        violations.push(
          `${file} at parent ${parent.slice(0, 8)} has content but zero recognized "- " entries -- ` +
            'the entry-boundary parser or the file format changed; refusing to treat this as "nothing to check."'
        )
        continue
      }
      const currentSet = headContents.get(file) ?? new Set()
      // The one-time public-ledger reset (see PUBLIC_LEDGER_RESET above):
      // HEAD carries the sentinel and this parent does not, so this parent is
      // the pre-reset ledger and the commit under test is the reset itself.
      //
      // NOTE THE ORDER. The non-vacuity refusal above runs BEFORE this, and
      // must keep running before it: a truncated read of the parent produces
      // zero fingerprints, and zero fingerprints carry no sentinel, so a
      // parent that could not be read would otherwise launder itself into
      // "this parent predates the reset" and be skipped entirely. That is the
      // #890 shape wearing the reset's clothes. Non-vacuity first, always.
      const headSentinel = resetSentinelState(file, currentSet, headRef)
      const parentSentinel = resetSentinelState(file, parentFingerprints, parent)
      if (headSentinel.refusal !== undefined || parentSentinel.refusal !== undefined) {
        violations.push(headSentinel.refusal ?? parentSentinel.refusal)
        continue
      }
      if (headSentinel.present && !parentSentinel.present) {
        // A RESET IS A REPLACEMENT, NOT A LICENCE TO EDIT.
        //
        // Skipping the comparison alone would make the reset commit a
        // one-commit window in which most of the old ledger could be kept
        // while a few shipped entries were quietly reworded -- which is
        // precisely, and continuously, what this guard exists to stop,
        // reintroduced once. So the skip is CONDITIONAL on the replacement
        // being total: not one entry of the pre-reset ledger may survive into
        // the new file. A decision that still binds is RESTATED as a new
        // dated entry, never copied, and a restatement has a different first
        // line by construction.
        const survivors = parentFingerprints.filter((fingerprint) => currentSet.has(fingerprint))
        if (survivors.length > 0) {
          violations.push(
            `${file}: the public-ledger reset at ${headRef} is not a replacement -- ${survivors.length} ` +
              `entr(y/ies) from parent ${parent.slice(0, 8)} survived into the new file. A reset REPLACES ` +
              'the ledger; a decision that still binds is RESTATED as a new dated entry, never copied, so ' +
              'a survivor means either the reset is really an edit or an old entry was pasted forward:\n' +
              survivors.slice(0, 5).map((f) => `  ${JSON.stringify(f)}`).join('\n') +
              (survivors.length > 5 ? `\n  ... +${survivors.length - 5} more` : '')
          )
          continue
        }
        checked.ledgerResets ??= []
        checked.ledgerResets.push({
          file,
          parent,
          skippedParentEntries: parentFingerprints.length,
          sharedWithParent: 0,
          reason: PUBLIC_LEDGER_RESET.reason,
        })
        continue
      }
      for (const fingerprint of parentFingerprints) {
        if (currentSet.has(fingerprint)) continue
        const exception = exceptions.find(
          (e) => e.file === file && e.oldFirstLine === fingerprint
        )
        if (exception) {
          usedExceptions.add(exception)
          continue
        }
        violations.push(
          `${file}: entry present at parent ${parent.slice(0, 8)} is missing from ${headRef} and not in ` +
            `DOCUMENTED_FIRST_LINE_EDITS: ${JSON.stringify(fingerprint)}`
        )
      }
    }
  }

  // A stale exception (never matched anything this run) means the manifest
  // no longer reflects reality -- the same "manifest must track reality,
  // not just gate it" symmetry #877's own guard-wiring check applies. Only
  // THIS CALL's own `exceptions` array is checked (see the function doc
  // comment above) -- never the raw module-level registry, so a real entry
  // relevant to a DIFFERENT call (or to production, from inside a fixture
  // test) is never in scope to be flagged stale here.
  //
  // #921: `consumed: true` entries are EXCLUDED from this scan entirely.
  // An exception does its one job in the single commit whose parent still
  // carries its `oldFirstLine`, then goes permanently unused by
  // construction -- flagging that as staleness forever would make correct,
  // one-time use indistinguishable from an exception nobody ever wired up.
  // An unconsumed entry that never matches is still flagged: that is the
  // real rot case (a registered exception whose `oldFirstLine` was wrong
  // from the start, or that got orphaned before ever doing its job).
  for (const exception of exceptions) {
    if (exception.consumed) continue
    if (!usedExceptions.has(exception)) {
      violations.push(
        `Stale exception in DOCUMENTED_FIRST_LINE_EDITS: ${JSON.stringify(exception)} matched nothing -- remove it, fix its oldFirstLine, or mark it consumed: true if it already did its job.`
      )
    }
  }

  return { violations, checked }
}

// ---------------------------------------------------------------------------
// The real guard: HEAD against its actual parent(s) in this repo.
// ---------------------------------------------------------------------------
test('CHANGELOG.md and DECISIONS.md are append-mostly across HEAD vs. its parent(s)', () => {
  const { violations, checked } = checkAppendOnly(repoRoot, 'HEAD')
  assert.equal(
    violations.length,
    0,
    `${violations.length} append-only violation(s):\n${violations.join('\n')}\n\nChecked: ${JSON.stringify(checked, null, 2)}`
  )
})

// ---------------------------------------------------------------------------
// Tamper proofs: real on-disk fixture repos, real `git init`/commits/merges
// -- not hand-rolled data structures standing in for git history. Each
// proves the guard fails for the RIGHT reason, then that the same content
// restored (or a legitimate escape hatch used) passes.
// ---------------------------------------------------------------------------

function initFixtureRepo() {
  const dir = mkdtempSync(join(tmpdir(), 'doc-append-only-fixture-'))
  execFileSync('git', ['init', '-q'], { cwd: dir })
  execFileSync('git', ['config', 'user.email', 'test@example.com'], { cwd: dir })
  execFileSync('git', ['config', 'user.name', 'Test'], { cwd: dir })
  return dir
}

function commitFile(dir, file, content, message) {
  writeFileSync(join(dir, file), content)
  execFileSync('git', ['add', file], { cwd: dir })
  execFileSync('git', ['commit', '-q', '--allow-empty', '-m', message], { cwd: dir })
  return execFileSync('git', ['rev-parse', 'HEAD'], { cwd: dir, encoding: 'utf8' }).trim()
}

/** A `--no-ff --no-commit` merge conflicts (real per-line conflict, exit
 * code 1) whenever both sides genuinely diverge on the same file -- exactly
 * the shape these tamper proofs need to exercise. execFileSync throws on a
 * nonzero exit; the conflicted merge state (markers written, `MERGE_HEAD`
 * present) is exactly what the caller wants regardless of whether git
 * itself calls that outcome a success. */
function mergeAllowingConflict(dir, branch) {
  try {
    execFileSync('git', ['merge', '-q', '--no-ff', '--no-commit', branch], { cwd: dir })
  } catch {
    // Expected for a real conflict -- the caller overwrites the file with
    // its own resolved content and commits next.
  }
}

test('tamper proof: a commit that truncates CHANGELOG.md to empty is caught, naming the file', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — first entry\n  continuation line\n- 2026-01-02 — second entry\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', '', 'the #890 shape: silently truncated to zero')
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(violations.length >= 2, 'expected both entries to be reported missing')
    assert.ok(violations.some((v) => v.includes('first entry')))
    assert.ok(violations.some((v) => v.includes('second entry')))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('tamper proof: a merge that silently drops 400 lines while adding 20 is caught (not just a size check)', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n- 2026-01-02 — entry B\n- 2026-01-03 — entry C\n', 'seed')
    execFileSync('git', ['checkout', '-q', '-b', 'feature'], { cwd: dir })
    // The feature branch legitimately adds a new entry.
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n- 2026-01-02 — entry B\n- 2026-01-03 — entry C\n- 2026-01-04 — entry D (new, legitimate)\n', 'feature: add entry D')
    execFileSync('git', ['checkout', '-q', 'master'], { cwd: dir })
    // The "merge" -- built by hand as a real 2-parent commit -- drops
    // entry B while adding D: net +1 line if you count files this way,
    // which is exactly what a naive size/line-count check would miss.
    mergeAllowingConflict(dir, 'feature')
    writeFileSync(join(dir, 'CHANGELOG.md'), '- 2026-01-01 — entry A\n- 2026-01-03 — entry C\n- 2026-01-04 — entry D (new, legitimate)\n')
    execFileSync('git', ['add', 'CHANGELOG.md'], { cwd: dir })
    execFileSync('git', ['commit', '-q', '-m', 'merge feature (silently drops entry B)'], { cwd: dir })
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(
      violations.some((v) => v.includes('entry B')),
      `expected entry B's disappearance to be caught; violations: ${JSON.stringify(violations)}`
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a merge that keeps every parent entry (real superset, both sides) passes clean', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n', 'seed')
    execFileSync('git', ['checkout', '-q', '-b', 'feature'], { cwd: dir })
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n- 2026-01-02 — entry B (feature)\n', 'feature adds B')
    execFileSync('git', ['checkout', '-q', 'master'], { cwd: dir })
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n- 2026-01-03 — entry C (master)\n', 'master adds C')
    mergeAllowingConflict(dir, 'feature')
    writeFileSync(
      join(dir, 'CHANGELOG.md'),
      '- 2026-01-01 — entry A\n- 2026-01-02 — entry B (feature)\n- 2026-01-03 — entry C (master)\n'
    )
    execFileSync('git', ['add', 'CHANGELOG.md'], { cwd: dir })
    execFileSync('git', ['commit', '-q', '-m', 'merge feature (real superset of both sides)'], { cwd: dir })
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.deepEqual(violations, [])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a correcting edit to an entry BODY (not its first line) is not flagged -- typo fixes are not deletions', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n  with a typo in this line\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n  with the typo fixed in this line\n', 'fix a typo in the body')
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.deepEqual(violations, [])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('escape hatch: a documented first-line edit is allowed; an undocumented one is not', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry with a typo in its headline\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry with the typo fixed\n', 'fix headline typo')
    const undocumented = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(undocumented.violations.length > 0, 'an undocumented first-line edit must be flagged')

    // #925: the exception is passed as this call's own scoped `exceptions`
    // argument, not pushed onto the shared module-level
    // DOCUMENTED_FIRST_LINE_EDITS registry. Mutating that shared array (the
    // guard's original design) meant every OTHER call to checkAppendOnly
    // running anywhere in this process -- including the real top-level
    // production test above, and every other fixture test in this file --
    // would see the mutation too, for as long as it was live. Passing it as
    // a parameter scopes it to exactly this one call, which is both the fix
    // for #925 and a cleaner test in its own right (no shared mutable state,
    // no try/finally reset to forget).
    const withException = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [
      {
        file: 'CHANGELOG.md',
        oldFirstLine: '- 2026-01-01 — entry with a typo in its headline',
        reason: 'test-only: proves the escape hatch clears a documented first-line edit'
      }
    ])
    assert.deepEqual(withException.violations, [])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('a stale exception (matches nothing) is itself flagged', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n', 'no-op edit')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [
      {
        file: 'CHANGELOG.md',
        oldFirstLine: '- 2026-01-01 — an entry that never existed',
        reason: 'test-only: proves a stale exception is caught'
      }
    ])
    assert.ok(violations.some((v) => v.includes('Stale exception')))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// #921: an entry marked consumed did its ONE job in some earlier commit and
// will never match again by construction (the very next commit's parent no
// longer carries the old fingerprint) -- flagging that as staleness forever
// is exactly what broke three real CHANGELOG.md exceptions tonight.
test('#921: an exception marked consumed: true that matches nothing is NOT flagged stale -- it already did its job', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — entry A\n', 'no-op edit')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [
      {
        file: 'CHANGELOG.md',
        oldFirstLine: '- 2026-01-01 — an entry consumed by a commit no longer in this fixture\'s history',
        reason: 'test-only: proves a CONSUMED exception is never flagged stale',
        consumed: true
      }
    ])
    assert.deepEqual(violations, [], `a consumed exception must never be flagged stale; got: ${JSON.stringify(violations)}`)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// #921: arm AND control. The `consumed` mechanism could silently disable
// staleness checking altogether by accident -- proving it still catches a
// genuinely unconsumed, never-matched exception (arm) is not the same as
// proving a REAL glued entry is still caught by the production defaults
// after the satisfied #921 exceptions were deleted (control).
// Both directions, against the real repoRoot's own real production check.
test('#921 ARM-AND-CONTROL: after deleting the three now-satisfied exceptions, the production check (a) still passes clean on real HEAD and (b) still catches a freshly introduced glued entry', () => {
  // (a) ARM: the real production registry produces zero violations against
  // the real repo's actual history.
  const real = checkAppendOnly(repoRoot, 'HEAD')
  assert.equal(
    real.violations.length,
    0,
    `expected zero violations against real HEAD now that the stale exceptions are gone; got: ${JSON.stringify(real.violations)}`
  )

  // (b) CONTROL: a fresh fixture repo, seeded with the real CHANGELOG.md's
  // own current tail (no exceptions registered), still flags a NEW glued
  // entry introduced in a follow-up commit -- proving the deletion did not
  // also blunt the glued-entry detector it happened to share a file with.
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- **Real entry, unrelated to any exception.** Body text.\n', 'seed')
    commitFile(
      dir,
      'CHANGELOG.md',
      '- **Real entry, unrelated to any exception.** Body text.- **A freshly glued second entry, never registered anywhere.** More text.\n',
      'introduce a fresh glued entry, unrelated to the three deleted exceptions'
    )
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.ok(
      violations.some((v) => v.includes('glued onto the previous line')),
      `expected the fresh glued entry to still be caught with zero exceptions registered; got: ${JSON.stringify(violations)}`
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// #925: the defect itself, reproduced directly. Before the fix, the
// stale-exception check iterated the WHOLE module-level
// DOCUMENTED_FIRST_LINE_EDITS array on every call regardless of which repo
// or files that call was checking -- so a real, permanent entry relevant
// only to the real repoRoot's CHANGELOG.md would be flagged "stale" inside
// this totally unrelated fixture-repo call, which never touches that entry
// at all. Proves the fix by exercising the exact cross-contamination shape:
// a real-looking exception for the real repoRoot is passed to
// checkAppendOnly for an unrelated FIXTURE repo whose history could never
// contain it -- with the old design this failed with "Stale exception";
// with the fix, only exceptions actually passed to a given call are ever
// evaluated for that call, but this call passes none, so nothing is
// evaluated and the run is clean.
test('#925: an exception scoped to one call does not leak into an unrelated call and get flagged stale there', () => {
  const productionShapedException = {
    file: 'CHANGELOG.md',
    oldFirstLine: '- 2026-01-01 — a real entry only ever checked against repoRoot, never this fixture',
    reason: 'test-only: simulates a real DOCUMENTED_FIRST_LINE_EDITS entry unrelated to this fixture'
  }
  // First, prove the entry is legitimately usable on its OWN scoped call
  // (same shape as the escape-hatch test above) -- this is not a dead
  // exception, it is simply irrelevant to the fixture call that follows.
  const ownDir = initFixtureRepo()
  try {
    commitFile(ownDir, 'CHANGELOG.md', `${productionShapedException.oldFirstLine}\n`, 'seed')
    commitFile(ownDir, 'CHANGELOG.md', '- 2026-01-01 — the corrected headline\n', 'fix headline')
    const ownResult = checkAppendOnly(ownDir, 'HEAD', ['CHANGELOG.md'], [productionShapedException])
    assert.deepEqual(ownResult.violations, [], 'the exception must clear its own matching call')
  } finally {
    rmSync(ownDir, { recursive: true, force: true })
  }

  // Then, an entirely unrelated fixture repo -- checked WITHOUT that
  // exception in its own `exceptions` array (as every real caller of this
  // guard for an unrelated repo/file set would) -- must never see it.
  const unrelatedDir = initFixtureRepo()
  try {
    commitFile(unrelatedDir, 'CHANGELOG.md', '- 2026-01-01 — entry unrelated to the fixture above\n', 'seed')
    commitFile(unrelatedDir, 'CHANGELOG.md', '- 2026-01-01 — entry unrelated to the fixture above\n', 'no-op edit')
    const unrelatedResult = checkAppendOnly(unrelatedDir, 'HEAD', ['CHANGELOG.md'], [])
    assert.deepEqual(
      unrelatedResult.violations,
      [],
      'an exception scoped to a different call must never surface as "stale" in an unrelated one'
    )
  } finally {
    rmSync(unrelatedDir, { recursive: true, force: true })
  }
})

test('non-vacuousness: a parent with real content but zero recognized entries is refused, not silently skipped', () => {
  const dir = initFixtureRepo()
  try {
    // No "- " prefixed lines at all -- the entry-boundary parser would
    // recognize zero entries even though the file plainly has content.
    commitFile(dir, 'CHANGELOG.md', 'just some prose, no bullet entries at all\n', 'seed (malformed for this format)')
    commitFile(dir, 'CHANGELOG.md', 'just some prose, no bullet entries at all\nstill no bullets\n', 'edit')
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(violations.some((v) => v.includes('zero recognized')))
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('tamper proof: a malformed `+-` top-level entry prefix is refused', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — valid entry\n', 'seed')
    commitFile(
      dir,
      'CHANGELOG.md',
      '- 2026-01-01 — valid entry\n+- 2026-01-02 — malformed entry\n',
      'add a malformed top-level prefix'
    )
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(
      violations.some((violation) => violation.includes('malformed top-level entry prefix')),
      `expected the malformed prefix to be refused; violations: ${JSON.stringify(violations)}`
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// ---------------------------------------------------------------------------
// #921: the glued-entry class -- a whole second entry squashed onto its
// predecessor's line with the separating newline dropped. Demonstrated red
// first (entryFingerprints alone sees ONE entry, not two -- the exact
// invisibility that let three real instances sit undetected in
// CHANGELOG.md), then green (gluedEntryViolations catches it and
// checkAppendOnly surfaces it).
// ---------------------------------------------------------------------------

const GLUED_FIXTURE =
  '- **First entry, complete.** Some body text ending in a period.- **Second entry, glued onto the first with zero separation.** More body text.\n'

test('THE DEMONSTRATED RED: entryFingerprints() alone treats a glued two-entry line as ONE entry -- the second entry is invisible to it', () => {
  const fingerprints = entryFingerprints(GLUED_FIXTURE)
  assert.equal(fingerprints.length, 1, `expected exactly 1 fingerprint (the glued line reads as one entry); got ${fingerprints.length}`)
  assert.ok(
    fingerprints[0].includes('Second entry, glued onto the first'),
    'the second entry is not a distinct fingerprint -- it is swallowed into the first entry\'s own fingerprint text, exactly the #921 blind spot'
  )
})

test('THE DEMONSTRATED GREEN: gluedEntryViolations() catches the same fixture the fingerprint scan is blind to', () => {
  const violations = gluedEntryViolations(GLUED_FIXTURE, 'CHANGELOG.md', 'HEAD')
  assert.equal(violations.length, 1, `expected exactly 1 glued-entry violation; got ${JSON.stringify(violations)}`)
  assert.match(violations[0], /glued onto the previous line/)
})

test('gluedEntryViolations never flags a legitimate nested sub-bullet (indented "  - **", real whitespace before it)', () => {
  const nested = '- **Top-level entry.** Body text.\n  - **Nested sub-bullet, indented, legitimate.** More text.\n'
  assert.deepEqual(gluedEntryViolations(nested, 'CHANGELOG.md', 'HEAD'), [])
})

test('gluedEntryViolations never flags an ordinary two-entry file with a real newline between them', () => {
  const clean = '- **First entry.** Body.\n- **Second entry.** Body.\n'
  assert.deepEqual(gluedEntryViolations(clean, 'CHANGELOG.md', 'HEAD'), [])
})

test('checkAppendOnly surfaces a glued entry in the real HEAD-vs-parent check, naming the file and line', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', '- **First entry, complete.** Body.\n', 'seed')
    commitFile(dir, 'CHANGELOG.md', GLUED_FIXTURE, 'introduce a glued second entry')
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.ok(
      violations.some((v) => v.includes('glued onto the previous line')),
      `expected a glued-entry violation; got ${JSON.stringify(violations)}`
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

// #921's own real-world confirmation: the three actual instances this fix
// removed from CHANGELOG.md, kept here as fixed strings so a regression
// (the newline getting dropped again by some future merge) is caught by
// name rather than only by the generic pattern.
test('REAL REPO: CHANGELOG.md at HEAD has zero glued entries', () => {
  for (const file of GLUED_ENTRY_FILES) {
    const content = gitShow(repoRoot, 'HEAD', file)
    if (content === undefined) continue
    const violations = gluedEntryViolations(content, file, 'HEAD')
    assert.deepEqual(violations, [], `${file} has ${violations.length} glued entr(y/ies) at HEAD:\n${violations.join('\n')}`)
  }
})

// The false positive found while building this, kept as a permanent
// negative control: DECISIONS.md legitimately uses `-- **bold**` mid-entry
// and must never be scanned by this pattern.
//
// The evidence used to be the LIVE DECISIONS.md, with its own comment saying
// that an empty result meant "re-check the reasoning, do not trust it
// silently." The 2026-08-25 public-ledger reset emptied that file, so the
// live leg came back empty and the instruction was followed: the reasoning
// was re-checked and it still holds -- the convention is unchanged and the
// pattern would still fire on it -- but the live file can no longer witness
// it. The witness is now a frozen fixture reproducing the exact shape, which
// is a better instrument anyway: it proves the false positive is real
// whatever the ledger happens to contain today, and it cannot go quietly
// vacuous the way a live-content check can.
const DECISIONS_MID_ENTRY_BOLD_FIXTURE =
  '- 2026-08-25 \u2014 A decision whose body cites a named rule -- **the wake lease** -- mid-sentence.\n'

test('DECISIONS.md is deliberately excluded from GLUED_ENTRY_FILES -- its own convention uses "-- **bold**" legitimately mid-entry', () => {
  assert.equal(GLUED_ENTRY_FILES.has('DECISIONS.md'), false)
  // The false positive is real: the glued-entry pattern DOES fire on this
  // legitimate shape, which is precisely why DECISIONS.md is excluded. An
  // assertion that the pattern matched nothing would prove the opposite of
  // what this control is for.
  assert.ok(
    /[^\s\n](- \*\*)/.test(DECISIONS_MID_ENTRY_BOLD_FIXTURE),
    'the mid-entry "-- **" shape must still trip the pattern; if it no longer does, the exclusion has lost its subject and should be removed rather than kept'
  )
  assert.equal(
    gluedEntryViolations(DECISIONS_MID_ENTRY_BOLD_FIXTURE, 'CHANGELOG.md', 'HEAD').length,
    1,
    'scanned AS IF it were CHANGELOG.md, this legitimate DECISIONS.md entry is reported as glued -- the concrete cost of removing the exclusion'
  )
})

// ---------------------------------------------------------------------------
// The one-time public-ledger reset (PUBLIC_LEDGER_RESET), proved on real
// fixture repos: the reset passes, and NEITHER of the two things it could be
// confused with does.
// ---------------------------------------------------------------------------

const RESET_CHANGELOG = '- **The public changelog starts here.** The private ledger is archived.\n'
const PRIVATE_CHANGELOG = '- **entry one.** body\n- **entry two.** body\n'

test('LEDGER RESET: replacing the whole ledger with the sentinel preamble passes, and the skip is RECORDED rather than silent', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', PRIVATE_CHANGELOG, 'the private ledger')
    commitFile(dir, 'CHANGELOG.md', RESET_CHANGELOG, 'the public ledger starts here')
    const { violations, checked } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.deepEqual(violations, [], `the reset must pass; got: ${JSON.stringify(violations)}`)
    assert.equal(checked.ledgerResets?.length, 1, 'the reset must be reported in `checked` -- a check that declines to check must say so')
    assert.equal(checked.ledgerResets[0].file, 'CHANGELOG.md')
    assert.equal(checked.ledgerResets[0].skippedParentEntries, 2, 'the report must name HOW MANY entries were not carried forward')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET is SELF-LIMITING: once the sentinel is on both sides, dropping an entry is a violation again', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', PRIVATE_CHANGELOG, 'the private ledger')
    commitFile(dir, 'CHANGELOG.md', RESET_CHANGELOG, 'the public ledger starts here')
    commitFile(dir, 'CHANGELOG.md', `${RESET_CHANGELOG}- **a public entry.** body\n`, 'a first public entry')
    commitFile(dir, 'CHANGELOG.md', RESET_CHANGELOG, 'silently drop that public entry')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.equal(violations.length, 1, `expected exactly one violation; got: ${JSON.stringify(violations)}`)
    assert.match(violations[0], /a public entry/)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET is not a wipe: emptying the ledger WITHOUT the sentinel is still the #890 failure', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', PRIVATE_CHANGELOG, 'the private ledger')
    commitFile(dir, 'CHANGELOG.md', '- **something else entirely.** body\n', 'wipe it, no sentinel')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.equal(violations.length, 2, `both dropped entries must be reported; got: ${JSON.stringify(violations)}`)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET: a reset that carries one old entry forward is refused, naming that entry', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', PRIVATE_CHANGELOG, 'the private ledger')
    // The dangerous shape: a "reset" that keeps most of the old file. If the
    // skip were unconditional this would pass, and any of the surviving
    // entries could have been reworded in the same commit invisibly.
    commitFile(dir, 'CHANGELOG.md', `${RESET_CHANGELOG}- **entry two.** body\n`, 'reset, but keep one old entry')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.equal(violations.length, 1, `expected exactly one violation; got: ${JSON.stringify(violations)}`)
    assert.match(violations[0], /not a replacement/)
    assert.match(violations[0], /entry two/, 'the surviving entry must be NAMED, or the reader cannot act on it')
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET: two sentinel entries in one file is an undesigned state and is REFUSED in words', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'CHANGELOG.md', PRIVATE_CHANGELOG, 'the private ledger')
    // `carriesResetSentinel` is "any fingerprint starts with the prefix", so a
    // second sentinel-shaped entry would otherwise be interpreted silently.
    const doubled = RESET_CHANGELOG + '- **The public changelog starts here.** a second one, somehow.\n'
    commitFile(dir, 'CHANGELOG.md', doubled, 'two sentinels')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.equal(violations.length, 1, `expected a refusal; got: ${JSON.stringify(violations)}`)
    assert.match(violations[0], /carries 2 entries opening with the public-ledger reset sentinel/)
    assert.match(violations[0], /Refusing to guess/)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET: a parent carrying two sentinels is refused too, not read as "already reset"', () => {
  const dir = initFixtureRepo()
  try {
    const doubled = RESET_CHANGELOG + '- **The public changelog starts here.** a second one, somehow.\n'
    commitFile(dir, 'CHANGELOG.md', doubled, 'a parent with two sentinels')
    commitFile(dir, 'CHANGELOG.md', `${doubled}- **a new entry.** body\n`, 'an ordinary append on top')
    const { violations } = checkAppendOnly(dir, 'HEAD', ['CHANGELOG.md'], [])
    assert.ok(
      violations.some((v) => /carries 2 entries opening with the public-ledger reset sentinel/.test(v)),
      `the PARENT's undesigned state must refuse too; got: ${JSON.stringify(violations)}`
    )
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test('LEDGER RESET sentinels match the real files, so the constant cannot drift from what it names', () => {
  // THIS TEST IS THE LANDING LOCK, and it is load-bearing well beyond
  // documentation. Do not demote it to a warning and do not delete it as
  // "just checking a constant".
  //
  // What it locks: because the sentinel is detected by CONTENT, the guard
  // change and the fresh ledgers must land in ONE commit. If the constant
  // could name a sentinel the files do not yet carry, there would be a window
  // in which somebody appending a sentinel-prefixed entry to the OLD ledger
  // would be granted a full reset skip on their next commit. Requiring the
  // real files to carry the sentinel AT HEAD closes that window: the constant
  // and its subject cannot exist apart, in any commit.
  //
  // The ordinary reading also applies -- a sentinel that stopped prefixing
  // the real preamble entry would silently stop matching, and the failure
  // would land on some future unrelated commit, which is the "stale
  // allowlist" shape this repo keeps getting bitten by.
  for (const [file, prefix] of Object.entries(PUBLIC_LEDGER_RESET.sentinelPrefixes)) {
    const content = gitShow(repoRoot, 'HEAD', file)
    assert.ok(content !== undefined, `${file} must exist at HEAD`)
    assert.ok(
      carriesResetSentinel(file, entryFingerprints(content)),
      `${file} at HEAD does not carry its reset sentinel ${JSON.stringify(prefix)} -- update PUBLIC_LEDGER_RESET or the file, deliberately`
    )
  }
})

test('a file absent at the parent contributes no fingerprints (a brand-new tracked file is never a violation)', () => {
  const dir = initFixtureRepo()
  try {
    commitFile(dir, 'README.md', 'placeholder\n', 'seed, no CHANGELOG.md yet')
    commitFile(dir, 'CHANGELOG.md', '- 2026-01-01 — first ever entry\n', 'add CHANGELOG.md for the first time')
    const { violations } = checkAppendOnly(dir, 'HEAD', TRACKED_FILES, [])
    assert.deepEqual(violations, [])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
