// The permanent SQL-only scan gate (Mandate 2: no organization decision
// state lives outside SQLite). Ported off the parked bun:test corpus
// (E9-S6, docs/testing/parked-suite-triage.json's row for
// tests/sql-only-state.test.ts) — that original scanned `src/organization`,
// which #787 (E4-S1) moved to `apps/cli/src/legacy/organization`, so the
// scan had enumerated ZERO write call sites and passed vacuously since the
// move, and it was wired into no gate at all: a Mandate 2 violation could
// have landed and nothing would have caught it. Run with
// `node --test scripts/test/sql-only-state.test.mjs`.
//
// HOW IT GATES (a real structural check, not a hardcoded filename list that
// goes stale): it enumerates — from source, so a NEW writer trips it — every
// byte-writing call site (`writeFileSync` / `appendFileSync` / `writeFile`,
// including the ones buried inside atomic-write helpers, since those bottom
// out in `writeFileSync`) under EVERY writer root of the org data tree — see
// `WRITER_ROOTS` below, which is the single list this prose deliberately does
// not duplicate (#751/G12 widened it from three roots to six, and a
// hand-copied second list in a header comment is precisely how the previous
// version of this sentence came to describe a smaller scan than the one that
// ran). Each found site
// must map to exactly one ALLOWLIST entry below. Any site not on the list =
// failure, named by file, line and source text. The reverse also fails: an
// allowlist entry that matches nothing means a blessed writer was deleted
// and its entry must be removed — so the list can never quietly drift from
// the code.
//
// THE #848 LESSON, APPLIED HERE: the exact defect that made the original
// gate go silent was a scan root that stopped existing and nobody noticed
// because an empty scan still returns a (vacuously) empty result. This port
// therefore asserts EVERY writer root is non-empty before trusting the
// scan at all — a root that matches zero tracked files fails loudly, the
// same way #848 says a zero-match scan must never look identical to a
// verified-clean one.
//
// WHOLE-TREE SCOPE (DECISIONS.md 2026-07-20, implementer C, memory-review
// family): the gate is NOT scoped to `state/`. It enumerates WRITERS by
// source, so a write to ANY subpath of the data tree is caught. Because we
// key on the write CALL SITE, no destination subtree can escape.
//
// SCOPE NOTE: this gate governs the CREATE side — new file-backed durable
// state. `existsSync`-as-refusal (a reader treating a file's presence as a
// decision) is a distinct reader-side concern and deliberately out of scope
// here. Likewise `renameSync` / `copyFileSync` / `cpSync` are commit/move/
// copy mechanics, not state-creating byte writes — every atomic write's real
// bytes land at its `writeFileSync(temporary, …)`, which IS caught here.
//
// PORT NOTE (E9-S6, this file): every entry below is the ORIGINAL
// bun:test file's allowlist with `file` repointed from `src/organization/*`
// to `apps/cli/src/legacy/organization/*` (the #787 move), then the extension
// entries moved into `packages/piing/extensions/*` by #785. Every entry was
// verified programmatically against the live tree before this port landed:
// each `file` exists and each `match` line is present verbatim at that
// path. The two narrower regression tests that lived alongside the
// structural guard in the original file (the intercom/team-ui normalized-
// route check, and the ack-spool-import check) are NOT ported here — they
// are not part of the Mandate 2 structural guard this file's disposition
// row targets; `tests/sql-only-state.test.ts` itself remains in place,
// frozen, as the parked-corpus record of them (D25).
//
// STANDING RULES honoured: no `.skip`, no soft-warn — a violation fails the
// run (never weaken a test). Mandate 1 (reactive-only) note: every check
// here is a single synchronous read — no polling, no interval, no sleep.
//
// #882 — TRACKED-VS-UNTRACKED, DECIDED: `git ls-files`'s default is
// tracked-only, so a planted unblessed `writeFileSync` sitting UNTRACKED did
// not trip this guard until it was `git add`ed. Found while falsifying this
// guard for #865. Decided, not left an accident: this guard's real job is
// "no unblessed writer exists in this tree" — a pre-commit sanity check a
// developer runs on code they are ABOUT to commit, which is exactly the
// window where it is still untracked. A guard whose local pass answers
// "committed code is clean" while its user reads it as "my new code is
// clean" is the same gap `git grep` and #848's typecheck legs already cost
// this repo twice. CI is unaffected either way — a CI checkout never has
// untracked files, since every file present there came from a commit. So
// the enumeration below is `git ls-files -z --cached --others
// --exclude-standard` (tracked + untracked-but-not-gitignored, under the
// SAME writer roots) rather than the old tracked-only default; `
// --exclude-standard` keeps `.gitignore`d scratch/build output (the thing
// option 2's "may surface scratch files" concern named) out of scope, so
// nothing legitimately excluded starts tripping this guard. The enumerated
// count is now printed alongside which roots and which scope it came from,
// per #848's own "print the count" precedent, so a green result states the
// question it answered rather than leaving it to be inferred or assumed.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFileSync, existsSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { tmpdir } from 'node:os'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

// #848: the scan root that broke silently (src/organization, moved to
// apps/cli/src/legacy/organization by #787) still exists as a STRING —
// nothing stops a future move from repeating the exact failure this port
// exists to fix. Naming the dead root here, alongside the live one, is what
// lets the "before/after" evidence test below demonstrate the difference
// rather than merely assert it.
const STALE_PRE_787_ROOT = 'src/organization'

// Byte-writing primitives. Atomic-write helpers (writeJsonAtomic/writeJsonSync)
// are not matched by name — they are caught at the `writeFileSync` inside
// them, which is the real place bytes hit disk, and that inner line is
// itself on the allowlist with the helper's purpose.
const WRITE_PRIMITIVE = /(?<![.\w])(writeFileSync|appendFileSync|writeFile)\s*\(/

// The source roots that emit the org data tree, live post-#787. The
// operator-lifecycle root that used to sit here is absent because it no longer
// exists: it was ported to Rust and deleted, and a scan root that cannot match
// is exactly the vacuous-pass failure this file's header exists to prevent.
//
// #751/G12 — THREE ROOTS ADDED, AND WHY THE OMISSION MATTERED. This list held
// only the first three entries below, which meant the gate could not see any
// of these live TypeScript byte-writers:
//   - apps/cli/src/legacy/foundation/launcher-setup.ts (2 sites) — the
//     operator's own pi-home settings, written on every `bun run cli setup`.
//     Two unblessed writes in the shipping boot path that this Mandate-2 gate
//     structurally could not reach.
//   - packages/chiefing/src/resources/Identity.ts (2 sites) — the P-256
//     identity key.
//   - packages/piing/src (4 sites) — the provider projection's atomic writes
//     plus E3's home-materialization writers.
// Each of those is a SANCTIONED class (the audit's own reading, and the
// allowlist below states each one's basis). The gap was never that the writes
// were wrong; it was that nothing blessed them, so the gate's green said
// nothing about them either way — a Mandate-2 instrument that cannot see two
// of its subject trees is reporting on a smaller repo than the one that
// ships. Widening the roots is what turns those writes from invisible into
// reviewed.
// #751/P0 deleted `apps/cli/src/legacy` outright, so its three roots
// (`organization`, `cli.ts`, `foundation`) go with it — a writer root naming
// a tree that no longer exists cannot be scanned, and the gate refuses on it
// rather than passing over an absent directory. `apps/cli/src` is not
// re-added in their place: the CLI is now the Bun entry point plus the
// Founder session and writes no state at all.
const WRITER_ROOTS = [
  'packages/piing/extensions',
  'packages/chiefing/src',
  'packages/piing/src',
]

// ===========================================================================
// THE ALLOWLIST — every blessed file writer, its destination, and why.
// Migrated verbatim from tests/sql-only-state.test.ts's ALLOWLIST; only the
// `file` field changed (src/organization/* -> apps/cli/src/legacy/organization/*).
// A reviewer reads this array to see exactly what is blessed and on what basis.
// ===========================================================================
const ALLOWLIST = [
  // =========================================================================
  // HARNESS — the Pi harness itself reads these; they cannot live behind SQL.
  // =========================================================================
  // #751/E4 — THE ROWS DELETED HERE, AND WHY THEY ARE NOT REPOINTED.
  // Eighteen entries used to sit in this array naming write sites under
  // `apps/cli/src/legacy/organization/`: org-materialize.ts (six),
  // org-reload-hard-contract.ts, org-loop-control.ts, org-person-contracts.ts,
  // org-learned-skills.ts, org-sync-transport.ts, org-log.ts,
  // org-supervision.ts, org-operator-escalation.ts, agent-identity.ts (two)
  // and durable-store-fetch-worker.ts (two). Every one of those FILES is
  // gone — the ~94-module port into chiefd took that directory down to
  // company-files.ts and managed-pane-observation.ts, neither of which
  // writes bytes. There is no surviving TypeScript site any of those rows
  // could be repointed at (the writes they blessed are now chiefd's, behind
  // SQL or behind chiefd's own materializer), and this guard's reverse
  // direction — "an allowlist entry that matches nothing means a blessed
  // writer was deleted and its entry must be removed" — is precisely the
  // rule that flagged them. Removing them is that rule being obeyed, not
  // the guard being loosened: the FORWARD direction is untouched, so any
  // write site reappearing under these roots still fails unblessed.

  // =========================================================================
  // BOOTSTRAP — how the database/org is FOUND, or how the store's own death is
  // attributed. Cannot live behind the database it locates.
  //
  // #751/E4: this class has ZERO live sites. Its sole row was
  // org-sync-transport.ts's store-death sentinel, and the synchronous
  // transport it belonged to was deleted outright (Mandate 1: no
  // Atomics.wait, no fetch Worker), so there is no dead store left to
  // attribute the death of. The heading stays because the CLASS is still a
  // legitimate one a future blessed writer could fall into; the row does
  // not, because it names a file that no longer exists.
  // =========================================================================

  // =========================================================================
  // LOG — append-only diagnostic stream no code reads back for a decision.
  // =========================================================================
  // #964 moved bus/events.jsonl's write site out of its callers and into the
  // shared bounded-append helper (callers now call appendBoundedJsonlLine, not
  // appendFileSync, for this file specifically) -- one allowlist entry for the
  // one real write site, not one per caller of it. Only organization-intercom.ts
  // still produces this stream; the second producer has since been deleted.
  { file: 'packages/piing/extensions/bus-events-bounded-append.ts', match: 'appendFileSync(path, payload, { encoding: "utf8", mode: 0o600 });', writes: '.chief/bus/events.jsonl (bounded, rotating; written through this one helper by organization-intercom.ts)', cls: 'LOG', why: 'plan §LOGS — .chief/bus/events.jsonl append stream; #964 gave it one shared, bounded writer instead of an unbounded append per caller.' },
  { file: 'packages/piing/extensions/organization-intercom.ts', match: 'appendFileSync(join(directory, "exceptions.jsonl"), `${JSON.stringify({', writes: '.chief/logs/exceptions.jsonl', cls: 'LOG', why: 'plan §LOGS — exceptions.jsonl diagnostic stream.' },
  { file: 'packages/piing/extensions/organization-intercom.ts', match: 'appendFileSync(join(directory, `${service}.jsonl`), `${JSON.stringify({', writes: '.chief/logs/<service>.jsonl', cls: 'LOG', why: 'per-service diagnostic append stream; LOG class.' },
  // The Pi half of the daemon observability stream. A `chiefd_launch_company`
  // measured at 143.2s spent 98.2% of it inside the extension with nothing
  // written anywhere, so the step could not be explained from disk; this is
  // the one write site that fixes that. Same class, and the same properties
  // that make the class safe: append-only, bounded by rotation, best-effort
  // (a failed write is counted and swallowed), and NEVER read back — no
  // roster, assignment, goal or supervision decision is taken from it.
  { file: 'packages/piing/src/extensionruntime/LaunchTrace.ts', match: "appendFileSync(path, line, { encoding: 'utf8', mode: 0o600 })", writes: '<dir>/.chief/log/founder-pi.jsonl, else $HOME/.chief/log/ for a box-wide process (bounded, rotating)', cls: 'LOG', why: 'plan §LOGS — the Pi-side launch trace, emitting `chiefd-log`\'s record so the two runtimes\' streams merge on timestamp. Diagnostic only. It writes NOWHERE when the environment names no directory, which is why the append is guarded rather than unconditional.' },

  // =========================================================================
  // HOST-MUTEX — host-scoped physical tmux mutex under /tmp, OUTSIDE every
  // data root; a per-instance db would let two checkouts both claim one tmux
  // session. REVISIT at one-daemon.
  // (#825: org-supervisor-host-owner.ts, the sole HOST-MUTEX row, is deleted
  // whole with the detached TS supervisor that took this mutex — the class
  // now has zero live sites.)
  // =========================================================================

  // =========================================================================
  // REVIEW / IPC — decision-state-to-file classified as bounded, transient IPC.
  // =========================================================================

  // FOUNDER LAUNCH's two blessed temp-file writers are GONE, not moved: the
  // `chiefd_launch_company` tool no longer serializes a company spec and a
  // Founder bootstrap into a temp directory for a CLI child to read back. It
  // POSTs one document to chiefd, which owns genesis, so the extension writes
  // nothing to disk at all and has no row to bless.

  // =========================================================================
  // NEW WRITERS (2026-07-26 #55 triage, ci-isolation): credential /
  // harness-marker / IPC / artifact collateral. NONE writes org DECISION
  // state (roster / assignments / goals / supervision) to a file.
  // =========================================================================
  // #751/E4: agent-identity.ts's two credential writes, org-materialize.ts's
  // `.launcher-owned` sentinel, and durable-store-fetch-worker.ts's two IPC
  // temps are all deleted with their files (the fetch worker with the
  // synchronous transport itself — Mandate 1 — and the identity/materialize
  // writes with the modules chiefd now owns). See the block comment at the
  // head of this array for why they are removed rather than repointed.

  // =========================================================================
  // PI-HOME — Mandate 5's sanctioned class: a Pi agent's own home directory.
  // The mandate is "only Pi's home on disk", so writing a Pi home is not an
  // exception to it, it IS it. These sites became visible to this gate for
  // the first time under #751/G12's WRITER_ROOTS widening; they have been
  // live and unreviewed by any instrument until now. None writes org DECISION
  // state (roster / assignments / goals / supervision) — the reverse
  // direction below is what keeps that true: if one of these lines changes,
  // its entry goes stale and a human re-reads it.
  // =========================================================================

  // =========================================================================
  // CREDENTIAL — a private key. Its bytes are the secret; there is no
  // SQL-shaped form of "the caller holds this key", and chiefd-host mints its
  // side the same way.
  // =========================================================================
  { file: 'packages/chiefing/src/resources/Identity.ts', match: 'writeFileSync(keyPath, keypair.privatePkcs8Pem, { mode: 0o600 })', writes: 'pi-home identity key (PKCS#8, 0600)', cls: 'CREDENTIAL', why: 'the P-256 caller-identity private key. Mode 0600, written once; the DERIVED public half is what chiefd stores. A key in a row is a key handed to every reader of the row.' },
  { file: 'packages/chiefing/src/resources/Identity.ts', match: 'writeFileSync(stageKeyPath, keypair.privatePkcs8Pem, { mode: 0o600 })', writes: 'staged pi-home identity key (PKCS#8, 0600)', cls: 'CREDENTIAL', why: 'same key, staged into a person pi-home being materialized. Same basis, same mode.' },

  // =========================================================================
  // PROVIDER PROJECTION — the models/credentials registry a Pi pane reads at
  // startup. Harness-read configuration under a pi-home, written atomically
  // (the blessed line is the `writeFileSync(temporary, …)` inside the
  // stage-then-rename, which is where bytes actually hit disk).
  // =========================================================================
]

function repoFile(...segments) {
  return join(repoRoot, ...segments)
}

// `cwd`/`roots` are parameters (not module-level constants) so the tamper
// proof below can point this SAME real pipeline at an isolated fixture
// repo instead of a hand-rolled stand-in for it.
//
// #882: `--cached --others --exclude-standard` (tracked + untracked, minus
// anything `.gitignore`/`.git/info/exclude`/the global gitignore excludes)
// rather than the old tracked-only default — see this file's top-of-file
// #882 note for why. `--exclude-standard` is what keeps this from also
// scanning `node_modules`, `dist`, build output, etc: those are excluded by
// the SAME mechanism a plain `git status` already uses to hide them, not a
// bespoke list this guard would have to maintain and could drift from.
function gitLsFiles(cwd, ...pathspecs) {
  return execFileSync(
    'git',
    ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', ...pathspecs],
    { cwd, encoding: 'utf8' },
  )
    .split('\0')
    .filter(Boolean)
}

const FIXTURE_GIT_IDENTITY = {
  name: 'SQL-only-state fixture',
  email: 'sql-only-state-fixture@example.invalid',
}

// Fixture commits must be reproducible on a host with no Git author identity.
// `user.useConfigOnly` disables Git's username/hostname fallback, so the
// following local configuration is not merely conventional: the test proves
// the temporary repository supplies its own identity and never consults or
// changes a developer's global configuration.
//
// #1041: `user.useConfigOnly` alone only defeats the SYNTHESIZED fallback. A
// real `user.name`/`user.email` in the developer's ~/.gitconfig (or in
// /etc/gitconfig) is still read, so on any box with a git identity — which
// is most of them — the fixture's commit SUCCEEDED and the
// `Author identity unknown` assertion below failed. That made the assertion
// a fact about the host rather than about the fixture, and CLAUDE.md carried
// a standing exemption telling every agent to ignore it. Neutralising the
// global and system config files makes the claim in the comment above
// literally true and the same on every host: the ONLY identity in play is
// the one this fixture writes into its own .git/config. The exemption is
// retired with this change, not preserved.
const FIXTURE_GIT_ENV = {
  ...process.env,
  GIT_CONFIG_GLOBAL: '/dev/null',
  GIT_CONFIG_SYSTEM: '/dev/null',
}

function fixtureGit(fixtureRoot, args) {
  return execFileSync('git', ['-c', 'user.useConfigOnly=true', ...args], {
    cwd: fixtureRoot,
    encoding: 'utf8',
    env: FIXTURE_GIT_ENV,
  })
}

function configureFixtureGitIdentity(fixtureRoot) {
  fixtureGit(fixtureRoot, ['config', '--local', 'user.name', FIXTURE_GIT_IDENTITY.name])
  fixtureGit(fixtureRoot, ['config', '--local', 'user.email', FIXTURE_GIT_IDENTITY.email])
}

// #882: renamed from `trackedTsFiles` — `gitLsFiles` now enumerates
// untracked-but-not-gitignored files too, so "tracked" would be a stale
// name for what this actually returns.
function scannableTsFiles(cwd, roots) {
  return gitLsFiles(cwd, ...roots)
    .filter((name) => name.endsWith('.ts') && !name.endsWith('.test.ts'))
    // #751/G12: `git ls-files --cached` reports the INDEX, and a file deleted
    // in the working tree but not yet staged is still in the index. Before
    // this filter, that shape crashed the whole guard with a bare
    // "ENOENT: ... open '<path>'" out of readFileSync — which is the worst
    // possible failure for an instrument, because it looks like a broken
    // checkout rather than like anything about the code, and it takes the
    // real forward check down with it. Hit for real the first time this
    // scan reached `packages/piing/src`, mid-deletion of two zero-consumer
    // modules. A path with no bytes on disk has no write sites; skipping it
    // is the same answer the scan would give after the deletion is staged,
    // reached without a crash in between. The FORWARD check is untouched:
    // every file that does exist is still enumerated and still must be
    // blessed.
    .filter((name) => existsSync(join(cwd, name)))
}

// Every byte-writing call site under `roots`, from source. Real recursion
// through the real filesystem via `git ls-files` — not a fixture list — so
// this function is exactly what both the real guard test and the tamper
// proof exercise.
function findWriteSites(cwd, roots) {
  const sites = []
  for (const file of scannableTsFiles(cwd, roots)) {
    const lines = readFileSync(join(cwd, file), 'utf8').split('\n')
    for (const [index, raw] of lines.entries()) {
      if (WRITE_PRIMITIVE.test(raw)) {
        sites.push({ file, line: index + 1, text: raw.trim() })
      }
    }
  }
  return sites
}

// Forward: every write site must be blessed. Reverse: every allowlist entry
// must still match a real site. Shared by the real guard test and the
// tamper proof so both exercise identical comparison logic.
function compareSitesToAllowlist(sites, allowlist) {
  const blessed = new Map()
  for (const entry of allowlist) {
    let set = blessed.get(entry.file)
    if (!set) {
      set = new Set()
      blessed.set(entry.file, set)
    }
    set.add(entry.match)
  }
  const unblessed = sites.filter((s) => !blessed.get(s.file)?.has(s.text))
  const seen = new Set(sites.map((s) => `${s.file}\x00${s.text}`))
  const stale = allowlist.filter((e) => !seen.has(`${e.file}\x00${e.match}`))
  return { unblessed, stale }
}

// #848: the real count found on b45d07c2's real count-vs-pattern lesson —
// a scan that "resolves" to a handful of stragglers is as dangerous as one
// that resolves to zero, since both look like "the gate ran and found
// nothing wrong." 41 real sites existed on the tree that ported this file,
// and the floor was set to 20 — well below that, as scripts/typecheck.sh's
// own floors are — so a few legitimate future writer deletions (W2-D-style
// migrations) wouldn't false-positive this gate, while a scan-root move or
// deletion (which collapses the count to 0) trips it immediately.
//
// #751/E4 RE-ANCHORED IT TO 4, and the reason matters because "lowering a
// floor" is normally the wrong move: the legacy TypeScript tree was not
// eroded by drift, it was DISSOLVED into Rust on purpose, taking every
// write site under `apps/cli/src/legacy/organization/` with it. The real
// count is now 7, all of them under `packages/piing/extensions/`. A floor of
// 20 against a real 7 does not protect anything — it makes this gate refuse
// to run at all, which is strictly worse than no floor, since the forward
// unblessed-writer check (the actual product invariant) then never executes.
// 4 keeps the same shape the floor always had: roughly half the real count,
// far enough above 0 that a scan root going missing still trips it loudly.
// Do NOT lower this again to accommodate a deletion — re-anchor only when
// the tree itself has legitimately shrunk past it, and say why here.
//
// #751/G12 RAISED IT TO 6, in the other direction and for the opposite
// reason: three writer roots were added (foundation, chiefing/src,
// piing/src), taking the real count from 7 to 13. A floor of 4 against a real
// 13 would tolerate two whole roots vanishing before it said anything, which
// is most of what a floor is for. 6 is again roughly half, and low enough
// that G5's scheduled deletion of the remaining zero-consumer piing writers
// cannot make this gate refuse to run.
const MINIMUM_WRITE_SITES = 6

// ---------------------------------------------------------------------------
// 0. The #848 lesson, applied literally: a writer root that matches zero
//    tracked files, or an enumeration that resolves far fewer real write
//    sites than expected, must REFUSE TO RUN and name the shortfall — not
//    silently produce a vacuously-passing scan. This is the exact defect
//    class that let the pre-#787 scan go unnoticed for however long it did.
// ---------------------------------------------------------------------------

for (const root of WRITER_ROOTS) {
  test(`writer root is non-empty and would actually be scanned: ${root}`, () => {
    assert.ok(existsSync(repoFile(root)), `writer root ${root} does not exist in the working tree at all`)
    const files = gitLsFiles(repoRoot, root)
    assert.ok(
      files.length > 0,
      `writer root "${root}" matched ZERO files (tracked + untracked, non-gitignored) — this is exactly the #787 defect class ` +
        '(a moved/renamed root silently scanning nothing while the gate still reports green). ' +
        'Fix WRITER_ROOTS, do not let this pass.',
    )
  })
}

test('negative self-test: a nonexistent writer root is caught, not silently scanned as empty-and-clean', () => {
  const bogusRoot = 'apps/cli/src/legacy/this-directory-does-not-exist-anywhere'
  assert.equal(existsSync(repoFile(bogusRoot)), false, 'fixture precondition: the bogus root must not exist')
  const files = gitLsFiles(repoRoot, bogusRoot)
  assert.equal(files.length, 0, 'fixture precondition: git ls-files must return nothing for the bogus root')
  // This is exactly the shape the real check above would have hit against
  // `src/organization` post-#787 had this non-emptiness guard existed then:
  // proving the assertion actually fires on the failure shape it exists to catch.
  assert.throws(() => {
    assert.ok(files.length > 0, `writer root "${bogusRoot}" matched ZERO files (tracked + untracked, non-gitignored)`)
  }, /matched ZERO files \(tracked \+ untracked, non-gitignored\)/)
})

test(`the write-site scan resolves at least ${MINIMUM_WRITE_SITES} real sites — a near-zero count refuses to run rather than passing quietly`, () => {
  const fileCount = WRITER_ROOTS.reduce((sum, root) => sum + scannableTsFiles(repoRoot, [root]).length, 0)
  const total = findWriteSites(repoRoot, WRITER_ROOTS).length
  // #882: state the question this run actually answered, not just that it
  // passed — "enumerated N files under these roots, scope tracked+untracked
  // minus .gitignore" is what makes a green result mean something specific
  // rather than "the scan ran," the same move #848 made for the typecheck
  // legacy leg's file count.
  console.log(
    `[sql-only-state] enumerated ${fileCount} file(s) (tracked + untracked, non-gitignored) under ` +
      `${WRITER_ROOTS.join(', ')} — ${total} real write call site(s) found (floor: ${MINIMUM_WRITE_SITES})`,
  )
  if (total < MINIMUM_WRITE_SITES) {
    throw new Error(
      `WRITER_ROOTS (${WRITER_ROOTS.join(', ')}) resolve only ${total} real write call site(s), ` +
        `below the expected floor of ${MINIMUM_WRITE_SITES} — a scan root probably no longer exists (#848). ` +
        'REFUSING TO TRUST THIS RESULT.',
    )
  }
})

test('BEFORE/AFTER evidence: the stale pre-#787 scan root resolves 0 sites; the live roots resolve real ones', () => {
  // "Before": the exact root the original tests/sql-only-state.test.ts used,
  // which #787 moved away from under it. This is what made the original
  // gate pass vacuously — not merely "unwired," genuinely blind.
  const before = gitLsFiles(repoRoot, STALE_PRE_787_ROOT)
  const beforeSites = before.length === 0 ? [] : findWriteSites(repoRoot, [STALE_PRE_787_ROOT])
  // "After": the live roots this port actually scans.
  const afterSites = findWriteSites(repoRoot, WRITER_ROOTS)
  console.log(
    `[sql-only-state] BEFORE (stale root "${STALE_PRE_787_ROOT}"): ${beforeSites.length} write site(s) found. ` +
      `AFTER (live roots "${WRITER_ROOTS.join(', ')}"): ${afterSites.length} write site(s) found.`,
  )
  assert.equal(beforeSites.length, 0, `expected the stale pre-#787 root to resolve 0 sites (proving it is gone), got ${beforeSites.length}`)
  assert.ok(
    afterSites.length >= MINIMUM_WRITE_SITES,
    `expected the live roots to resolve at least ${MINIMUM_WRITE_SITES} sites, got ${afterSites.length}`,
  )
})

// ---------------------------------------------------------------------------
// TAMPER PROOF: a guard that cannot fail is the same class of defect it was
// written to catch. This builds a REAL isolated git repo on disk (not a
// hand-rolled fixture array) containing exactly one unblessed write call,
// and runs the actual findWriteSites/compareSitesToAllowlist pipeline
// against it — proving end to end that a new file-backed org-decision-state
// write, added anywhere under a writer root, is caught, not merely that a
// synthetic comparison table can be doctored to look caught.
// ---------------------------------------------------------------------------

test('tamper proof: adding a real unblessed write site to a fixture repo is caught by the real scan pipeline', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'sql-only-state-tamper-'))
  try {
    execFileSync('git', ['init', '-q'], { cwd: fixtureRoot })
    const tamperedFile = join(fixtureRoot, 'org-tampered-writer.ts')
    writeFileSync(
      tamperedFile,
      'export function writeSomethingNobodyBlessed(path: string, value: unknown): void {\n' +
        '  writeFileSync(path, JSON.stringify(value));\n' +
        '}\n',
    )
    execFileSync('git', ['add', '-A'], { cwd: fixtureRoot })

    const sites = findWriteSites(fixtureRoot, ['.'])
    assert.equal(sites.length, 1, `expected exactly one write site in the fixture repo, found ${sites.length}`)

    // An EMPTY allowlist, exactly as if this new writer had never been
    // reviewed: the real comparison logic must flag it.
    const { unblessed, stale } = compareSitesToAllowlist(sites, [])
    assert.equal(unblessed.length, 1, 'the tampered write site must be reported unblessed by the real comparison logic')
    assert.equal(unblessed[0].file, 'org-tampered-writer.ts')
    assert.equal(stale.length, 0, 'an empty allowlist has nothing to go stale')

    // And the mirror case: an allowlist entry for a write that was since
    // deleted (reverse direction) is reported stale, using the SAME
    // fixture repo with the tampered file removed again. The deletion must
    // be RESTAGED (`git add -A` again) — `git ls-files` reports the index,
    // not the working tree, so a bare `rmSync` alone would leave the
    // now-deleted file still "tracked" and make this assert the wrong thing.
    rmSync(tamperedFile)
    execFileSync('git', ['add', '-A'], { cwd: fixtureRoot })
    const sitesAfterRemoval = findWriteSites(fixtureRoot, ['.'])
    const staleAllowlist = [{ file: 'org-tampered-writer.ts', match: 'writeFileSync(path, JSON.stringify(value));', writes: 'fixture', cls: 'DEAD', why: 'fixture' }]
    const reverse = compareSitesToAllowlist(sitesAfterRemoval, staleAllowlist)
    assert.equal(reverse.stale.length, 1, 'a since-deleted blessed writer must be reported as a stale allowlist entry')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// #882's own demonstration: this is the EXACT repro from the ticket — a
// planted unblessed writer that is never `git add`ed at all. Before this
// fix, `findWriteSites` (built on tracked-only `git ls-files`) returned
// ZERO sites here and this test would have failed to prove anything was
// caught; after it, the untracked file is found identically to a staged
// one. A sibling `.gitignore`d file proves the fix didn't overshoot into
// scanning things `--exclude-standard` is supposed to keep out.
test('#882: an UNTRACKED (never git-added) unblessed write site is caught — the exact repro that motivated this fix', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'sql-only-state-untracked-tamper-'))
  try {
    fixtureGit(fixtureRoot, ['init', '-q'])
    writeFileSync(join(fixtureRoot, '.gitignore'), 'ignored-writer.ts\n')
    fixtureGit(fixtureRoot, ['add', '.gitignore'])

    // RED: require a configured identity even on hosts where Git would
    // otherwise synthesize one from the username/hostname. This proves the
    // fixture itself, rather than a developer's machine, supplies the
    // identity its commit needs.
    assert.throws(
      () => fixtureGit(fixtureRoot, ['commit', '-q', '-m', 'fixture: add .gitignore']),
      /Author identity unknown/,
      'the disposable fixture must not rely on a host-level Git identity',
    )
    configureFixtureGitIdentity(fixtureRoot)
    assert.equal(
      fixtureGit(fixtureRoot, ['config', '--local', '--get', 'user.name']).trim(),
      FIXTURE_GIT_IDENTITY.name,
      'fixture identity must be stored only in its own .git/config',
    )
    assert.equal(
      fixtureGit(fixtureRoot, ['config', '--local', '--get', 'user.email']).trim(),
      FIXTURE_GIT_IDENTITY.email,
      'fixture identity must be stored only in its own .git/config',
    )
    fixtureGit(fixtureRoot, ['commit', '-q', '-m', 'fixture: add .gitignore'])

    // Deliberately NOT git-added — this is the whole point of the test.
    writeFileSync(
      join(fixtureRoot, 'untracked-writer.ts'),
      'export function writeSomethingNobodyBlessed(path: string, value: unknown): void {\n' +
        '  writeFileSync(path, JSON.stringify(value));\n' +
        '}\n',
    )
    // A gitignored sibling with an equally unblessed write, proving
    // `--exclude-standard` still keeps genuinely-excluded scratch files out
    // of scope rather than the fix overshooting into scanning everything.
    writeFileSync(
      join(fixtureRoot, 'ignored-writer.ts'),
      'export function alsoUnblessed(path: string, value: unknown): void {\n' +
        '  writeFileSync(path, JSON.stringify(value));\n' +
        '}\n',
    )

    const sites = findWriteSites(fixtureRoot, ['.'])
    assert.equal(
      sites.length,
      1,
      `expected exactly the untracked (non-gitignored) write site, found ${JSON.stringify(sites)}`,
    )
    assert.equal(sites[0].file, 'untracked-writer.ts')

    const { unblessed } = compareSitesToAllowlist(sites, [])
    assert.equal(
      unblessed.length,
      1,
      'an untracked, never git-added unblessed write site must be reported exactly like a staged one — ' +
        'this is #882: before this fix, an untracked writer was invisible until `git add`ed',
    )
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// ---------------------------------------------------------------------------
// 1. The structural guard itself: forward (every write site is allowlisted)
//    and reverse (every allowlist entry still matches a real site).
// ---------------------------------------------------------------------------

test('no org state is written to a file outside the SQL-only allowlist', () => {
  const sites = findWriteSites(repoRoot, WRITER_ROOTS)
  const { unblessed, stale } = compareSitesToAllowlist(sites, ALLOWLIST)

  assert.deepEqual(
    {
      unblessedWriteSites: unblessed.map((s) => `${s.file}:${s.line}  ${s.text}`),
      staleAllowlistEntries: stale.map((e) => `${e.file}  ${e.match}`),
    },
    { unblessedWriteSites: [], staleAllowlistEntries: [] },
  )
})

test('negative self-test: an unblessed write site and a stale allowlist entry are both caught', () => {
  // A fixture ALLOWLIST that is missing one real site's entry (proves the
  // forward direction fires) and carries one entry matching nothing real
  // (proves the reverse direction fires) — built from the real scan so this
  // stays true regardless of which sites exist today. The TAMPER PROOF test
  // above is the stronger, more literal version of this same claim (a real
  // fixture repo rather than a doctored in-memory list); this stays as a
  // second, cheaper angle on the real ALLOWLIST/scan pair.
  const sites = findWriteSites(repoRoot, WRITER_ROOTS)
  assert.ok(sites.length > 0, 'fixture precondition: the live scan must find at least one real write site')
  const missingOneEntry = ALLOWLIST.filter((e) => !(e.file === sites[0].file && e.match === sites[0].text))
  const withBogusEntry = [
    ...missingOneEntry,
    // Deliberately matches NOTHING in the tree — that is the point: it proves
    // the reverse direction (a blessed site that no longer exists is reported
    // stale) still fires. It names a synthetic path rather than a real-looking
    // one so it can never accidentally start matching; the previous version
    // pointed at `apps/cli/src/legacy/...`, which #751/P0 deleted, and was
    // swept up with the genuinely-stale rows it looked identical to.
    {
      file: 'packages/piing/src/does-not-exist-sql-only-state-control.ts',
      match: 'writeFileSync(neverRealPath, "control")',
      writes: '<nothing>',
      cls: 'PI-HOME',
      why: 'negative-control fixture row: exists only to be reported stale.',
    },
  ]

  const { unblessed, stale } = compareSitesToAllowlist(sites, withBogusEntry)

  assert.ok(unblessed.length > 0, 'expected the doctored allowlist to leave at least one real site unblessed')
  assert.ok(stale.length > 0, 'expected the doctored allowlist\'s bogus entry to be reported stale')
})

// #976: migrated from the unwired tests/sql-only-state.test.ts (deleted in
// the same change) -- both asserted something this file's own earlier
// tests do not, and were about to be lost silently along with the rest of
// that never-executing twin. Correspondence: each of these two tests here
// is a verbatim port of the like-named test in the deleted file, moved
// into the file `bun run test:sql-only-state` (package.json:46) actually
// runs.

test('copied agent authority readers use normalized Chiefd routes, never legacy org JSON projections', () => {
  const intercom = readFileSync(repoFile('packages/piing/extensions/organization-intercom.ts'), 'utf8')
  const teamUi = readFileSync(repoFile('packages/piing/extensions/team-ui.ts'), 'utf8')
  assert.ok(intercom.includes('"/v1/org/manifest/read"'), 'organization-intercom.ts must read the manifest through the normalized route')
  assert.doesNotMatch(intercom, /readFileSync\(join\(context\.organizationDir,\s*"org\.json"\)/, 'organization-intercom.ts must not read the legacy org.json projection directly')
  assert.doesNotMatch(intercom, /readFileSync\(join\(organizationDir,\s*"state",\s*"launcher\.json"\)/, 'organization-intercom.ts must not read the legacy state/launcher.json projection directly')
  assert.ok(teamUi.includes('"/v1/org/manifest/read"'), 'team-ui.ts must read the manifest through the normalized route')
  assert.doesNotMatch(teamUi, /readCachedJsonFile\(join\(organizationDir,\s*"org\.json"\)/, 'team-ui.ts must not read the legacy org.json projection directly')
})

test('the acknowledgement drain has no TypeScript writer at all', () => {
  // The drain moved into chiefd (chiefd-core/src/store/supervision_intake.rs).
  // The property this test used to assert about org-supervision-transport.ts —
  // "never reads from disk" — is now guaranteed by that file not existing, so
  // the check is that nothing reintroduces it.
  assert.ok(
    !existsSync(repoFile('apps/cli/src/legacy/organization/org-supervision-transport.ts')),
    'the acknowledgement drain must stay in chiefd; org-supervision-transport.ts is deleted'
  )
  assert.ok(
    !existsSync(repoFile('apps/cli/src/legacy/organization/org-acks-store.ts')),
    'the ACK queue must stay in chiefd; org-acks-store.ts is deleted'
  )
})
