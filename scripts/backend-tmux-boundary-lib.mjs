// P0 ARCHITECTURE MANDATE: chiefd is the backend, and the backend is
// CLIENT-AGNOSTIC. It must not know about tmux, and it must not know about
// the web. `apps/cli` owns every tmux concern; `apps/web` owns every web
// concern; both are frontends over one client-agnostic chiefd API. The
// full workstream is the design record; this module
// is packet P1's derivation half.
//
// Today that mandate is false in 105 files, which is why it has been a
// sentence and not a test. This module ENUMERATES the violations rather
// than returning a boolean, so the guard's failure message is the work-list
// and `scripts/fixtures/backend-tmux-register.mjs` is a mechanical work
// assignment (`packet` names which packet drains each row) rather than a
// number somebody has to interpret.
//
// FILE GRANULARITY, deliberately — not per-line, and not a count.
//   * Per-line goes red every time an unrelated agent touches a line inside
//     an already-listed file. Several agents are live in these crates right
//     now; a constantly-red guard is a guard somebody disables, which is
//     strictly worse than no guard.
//   * A COUNT ratchet is worse still: it says "not-worse" and nothing about
//     correct, and it cannot name the file that regressed.
//   Per-file is stable under unrelated edits, and still forces the row to be
//   DELETED when the file is genuinely finished.
//
// SEVEN RULES. 2 and 3 are the ones that stop the register rotting; 6 is the
// one that stops the SCOPE rotting; 5 and 7 are one boundary read from both
// sides.
//   1. unregisteredFiles           — a `.rs` file under a scan root matching
//                                    /tmux/i with no register row.
//                                    (catches GROWTH)
//   2. staleRegisterRows           — a registered path that no longer
//                                    contains tmux. You cannot fix a file
//                                    and leave its row behind.
//                                    (catches UNRECORDED PROGRESS)
//   3. missingRegisterRows         — a registered path that does not exist.
//                                    A file move orphans a row and this goes
//                                    red the same day, not six weeks later —
//                                    this repo lost weeks to exactly that
//                                    shape (#963: a stale allowlist row a
//                                    file move orphaned, invisible until
//                                    batch assembly, then misattributed to
//                                    an unrelated pin). (catches ROT)
//   4. duplicateRegisterRows       — one file, one row.
//   5. backendCratesDependingOnCli — no scan-root crate names the CLI /
//                                    operator crate in ANY dependency table.
//                                    A text scan cannot see a boundary that
//                                    is crossed through a Cargo edge.
//   6. scanRootsDrift              — every chiefd crate that carries tmux is
//                                    scanned, declared a CLIENT crate, or
//                                    explicitly declared out of scope by the
//                                    register's SCOPE ROW, with a reason and
//                                    an owning packet. (catches a guard whose
//                                    SUBJECT silently shrank)
//   7. clientCratesDependingOnBackend
//                                  — rule 5's MIRROR: no client crate names
//                                    a backend crate in ANY dependency table.
//                                    A boundary protected in one direction
//                                    only is not a boundary: the cheap way to
//                                    make `chief-cli` compile is to reach for
//                                    `chiefd_core::…`, and that single edge
//                                    would end the client-agnostic API before
//                                    anyone noticed it had started.
// Rules 2 and 3 together mean the register can only ever be CORRECT, never
// merely not-worse. There is no "add a row and move on" escape.
//
// THE SCAN READS FULL TEXT, INCLUDING COMMENTS — on purpose. A comment in
// chiefd-core saying "the tmux pane this respawns" is evidence the code
// still serves tmux; the noun survives in the model even when it has been
// renamed out of the identifiers. An exemption rule for comments would be a
// second classifier with its own rot, and the obvious cheap version of it
// (`organization-revision-tripwire.test.mjs`'s `mod tests` split heuristic)
// would silently exempt roughly 350 lines of fixture code. Comment-only
// files are still real rows — they are simply the cheap ones, and the
// derived `why` says which is which.
//
// THE SCOPE GAP IS CLOSED (P6). Scoping to the three backend crates used to
// leave the `chiefd` BINARY crate uncovered: 20 files, ~233 tmux code lines,
// including the whole 576-line `lifecycle/tmux.rs`. That was CORRECT for the
// operator half of that crate — under the mandate tmux belongs to the
// operator — and WRONG for its daemon half (`run.rs`, `docstore_only.rs`),
// which lived in the same crate. "No tmux here" was not a statement that
// could be made about it at all, so the gap was carried as the register's
// SCOPE ROW and enforced by rule 6 rather than as a comment, because a
// comment does not fail.
//
// P6 split that crate into `chiefd-daemon` (backend) and `chief-cli` (the
// operator client, installed as `chiefd`). The distinction is now
// expressible, so `chiefd-daemon` joined `SCAN_ROOTS` and the scope row was
// DELETED — in one commit, because rule 6 refuses either one without the
// other. Its remaining tmux files became ordinary register rows owned by P9,
// exactly like the three library crates', which is what "scanned" means.
//
// REGENERATION: `node scripts/backend-tmux-boundary-lib.mjs --write`
// rewrites the register from the current tree, PRESERVING the `why` and
// `packet` of every surviving row and the scope row verbatim. A large
// fraction of the 105 files will be deleted or moved by sibling packets;
// without regeneration the register is unmaintainable by hand and somebody
// deletes the guard instead of maintaining it. The write path is behind an
// executed-directly check and is therefore unreachable from `node --test`
// and from importing this module.

import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))

/** Where every chiefd crate lives, and therefore what rule 6 sweeps. */
export const CHIEFD_CRATES_DIR = 'apps/chiefd/crates'

/**
 * The crates the boundary is asserted over: every crate on the BACKEND side of
 * the mandate.
 *
 * `chiefd-daemon` joined the three library crates in P6, when the split made
 * "this crate must not know about tmux" a statement that could be made about
 * it. Growing this list is half of an atomic pair — see rule 6 and
 * `scanRootsDrift`, which refuses a scope row that disagrees with it.
 */
export const SCAN_ROOTS = [
  `${CHIEFD_CRATES_DIR}/chiefd-core`,
  `${CHIEFD_CRATES_DIR}/chiefd-host`,
  `${CHIEFD_CRATES_DIR}/chiefd-api`,
  `${CHIEFD_CRATES_DIR}/chiefd-daemon`,
]


/**
 * The crates on the CLIENT side of the mandate, where tmux BELONGS.
 *
 * `chief-cli` (P2) is the operator client: it owns sessions, windows, panes,
 * placement and attach, and it reaches chiefd only over HTTP. Its tmux is not
 * a violation and never becomes one, so it is neither scanned (rule 1 would
 * report every line of it) nor carried as a register row (rule 2 would demand
 * the row be deleted, which would be a claim that the work is done rather than
 * that it is correct). It is a THIRD disposition, declared here beside
 * `SCAN_ROOTS`, and rule 6 holds all three sets disjoint so a crate cannot be
 * quietly both.
 *
 * `chiefd-daemon` is deliberately NOT here. Before P6 the two were one crate
 * and "tmux belongs here" could not be said about it either way, which is what
 * the register's scope row carried; the split put the operator half in this
 * list and the daemon half in `SCAN_ROOTS`, and rule 6 holds the two disjoint.
 */
export const CLIENT_CRATES = [`${CHIEFD_CRATES_DIR}/chief-cli`]

/**
 * Crate names a scan-root crate may never depend on — derived from
 * `CLIENT_CRATES` rather than retyped, so a new client crate is covered by
 * rule 5 the moment it is declared a client and never by a second edit
 * somebody forgets.
 *
 * It listed `chiefd` as well until P6. That name was the binary crate that was
 * CLI and daemon in one; the split retired it, and what remains under it is
 * `chiefd-daemon`, which is a SCAN ROOT rather than a client. Matching is on
 * the EXACT dependency key, so `chiefd-core`/`chiefd-host`/`chiefd-api`/
 * `chiefd-daemon` are never false positives for a client name.
 */
export const CLI_CRATE_NAMES = CLIENT_CRATES.map((crate) => crate.split('/').pop())

/**
 * Crate names a CLIENT crate may never depend on — derived from `SCAN_ROOTS`
 * rather than retyped, so the two rules can never disagree about what
 * "backend" means.
 *
 * It carried a hand-written `'chiefd'` alongside the derivation until P6,
 * because the binary crate of that name was the daemon and was not scanned.
 * It is scanned now, under its real name, so the derivation covers it and the
 * extra literal would be a second place to edit.
 */
export const BACKEND_CRATE_NAMES = SCAN_ROOTS.map((root) => root.split('/').pop())

/** The one thing that makes a file a violation. Case-insensitive: `Tmux`,
 * `TMUX_SOCKET` and `tmux` are the same fact about the same boundary. */
export const TMUX_PATTERN = /tmux/i

/**
 * The anti-vacuity floor, INVERTED by #751/P10 and this is the whole subtlety
 * of making the guard unconditional.
 *
 * While the register was draining, the floor counted VIOLATIONS: 107 files
 * matched, and a sudden collapse to a handful meant the scan had gone blind
 * rather than that the work was done. That number cannot survive the drain —
 * the passing state is now ZERO matches, so a floor on matches would either be
 * permanently red or be set to 0, which is a floor that can never fail.
 *
 * So the floor moves to the thing that is still supposed to be large: the
 * number of tracked `.rs` files each scan root RESOLVES. A moved crate
 * directory, a `git ls-files` returning nothing, or a pathspec that no longer
 * matches makes a root resolve zero files — and zero scanned files with zero
 * matches is indistinguishable from verified-clean unless somebody asserts the
 * subject was there (#848, exactly). Every root is checked individually,
 * because a single total would let one dead root hide behind three live ones.
 *
 * RE-ANCHORING: lower it only when a crate genuinely shrinks, in the same
 * commit, with the measured date.
 */
export const MINIMUM_TRACKED_FILES_PER_ROOT = 10

/**
 * Tracked `.rs` files under each scan root — the SUBJECT of the scan, whether
 * or not any of them names tmux.
 */
export function trackedRustFilesByRoot(repoRoot, roots = SCAN_ROOTS) {
  return Object.fromEntries(
    roots.map((root) => [
      root,
      gitLsFiles(repoRoot, [root])
        .filter((file) => file.endsWith('.rs'))
        .filter((file) => existsSync(join(repoRoot, file))).length,
    ]),
  )
}

/**
 * Scan roots that resolve too few tracked files to be trusted. Empty is the
 * passing state; a named root here means the guard cannot see its subject and
 * must refuse rather than report clean.
 */
export function blindScanRoots(repoRoot, roots = SCAN_ROOTS) {
  return Object.entries(trackedRustFilesByRoot(repoRoot, roots))
    .filter(([, count]) => count < MINIMUM_TRACKED_FILES_PER_ROOT)
    .map(([root, count]) => `${root} resolves only ${count} tracked .rs file(s) — the scan is blind`)
    .sort()
}

function gitLsFiles(repoRoot, pathspecs) {
  const stdout = execFileSync(
    'git',
    ['ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', ...pathspecs],
    { cwd: repoRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
  )
  return stdout.split('\0').filter((entry) => entry.length > 0)
}

/** True if a source line's tmux mention sits inside a `//`/`///`/`/*` comment.
 * Used ONLY to derive a human-readable `why`/`packet` for a NEW register row
 * — never to exempt anything from the scan. */
function isCommentLine(line) {
  const trimmed = line.trim()
  return (
    trimmed.startsWith('//') ||
    trimmed.startsWith('*') ||
    trimmed.startsWith('/*') ||
    // A SQL comment inside a Rust string literal. `COMPANY_SCHEMA_SQL` in
    // `chiefd-core/src/schema.rs` is one long raw string, and the tombstones
    // explaining retired columns live in it as `--` lines. Those are prose by
    // every meaning of the word, and treating them as code made the guard fail
    // on documentation — the same way the schema's own column guard did before
    // it learned to strip them.
    //
    // Safe against false negatives: Rust has no prefix `--` operator, so a line
    // of real Rust cannot begin with one.
    trimmed.startsWith('--')
  )
}

/** The crate directory (`apps/chiefd/crates/<name>`) a repo-relative path
 * belongs to, or null if it is not under the chiefd crates tree. */
function crateDirOf(file) {
  if (!file.startsWith(`${CHIEFD_CRATES_DIR}/`)) return null
  const name = file.slice(CHIEFD_CRATES_DIR.length + 1).split('/')[0]
  return name ? `${CHIEFD_CRATES_DIR}/${name}` : null
}

/**
 * Every `.rs` file under the given roots whose FULL TEXT mentions tmux, with
 * the evidence that makes it a violation.
 *
 * Enumerated with `git ls-files --cached --others --exclude-standard` — the
 * index PLUS untracked-but-not-ignored files, because a never-git-added file
 * is exactly the state a fresh violation arrives in (#882) — and then
 * `existsSync`-filtered, because `--cached` reports index entries whose
 * worktree file has been deleted.
 */
export function scanTmuxFiles(repoRoot, roots = SCAN_ROOTS) {
  if (roots.length === 0) return []
  return gitLsFiles(repoRoot, roots)
    .filter((file) => file.endsWith('.rs'))
    .filter((file) => existsSync(join(repoRoot, file)))
    .map((file) => {
      const matches = readFileSync(join(repoRoot, file), 'utf8')
        .split('\n')
        .filter((line) => TMUX_PATTERN.test(line))
      return { file, matches }
    })
    .filter((entry) => entry.matches.length > 0)
    .map((entry) => {
      const code = entry.matches.filter((line) => !isCommentLine(line))
      return {
        file: entry.file,
        crate: crateDirOf(entry.file) ?? dirname(entry.file),
        hits: entry.matches.length,
        codeHits: code.length,
        commentOnly: code.length === 0,
        // The most useful single line of evidence: the first line of real
        // CODE naming tmux, falling back to the first comment when the
        // file's mentions are comments only.
        firstMatch: (code[0] ?? entry.matches[0]).trim(),
      }
    })
    .sort((a, b) => a.file.localeCompare(b.file))
}

/** Whether a repo-relative path exists and still mentions tmux. */
export function fileMentionsTmux(repoRoot, file) {
  const absolute = join(repoRoot, file)
  if (!existsSync(absolute)) return false
  return TMUX_PATTERN.test(readFileSync(absolute, 'utf8'))
}

/**
 * Every chiefd crate directory that carries at least one tmux-mentioning
 * `.rs` file — the universe rule 6 measures `SCAN_ROOTS` against. A crate
 * with no tmux at all (`beacond` today) is not in the universe: it needs
 * neither scanning nor an exemption, and demanding either would make the
 * scope row a list of every crate that ever existed.
 */
export function chiefdCratesWithTmux(repoRoot) {
  const crates = new Set()
  for (const entry of scanTmuxFiles(repoRoot, [CHIEFD_CRATES_DIR])) {
    const crate = crateDirOf(entry.file)
    if (crate) crates.add(crate)
  }
  return [...crates].sort()
}

/**
 * Every dependency key declared by a Cargo.toml, tagged with the table it was
 * declared in.
 *
 * Hand-rolled, in the same idiom (and for the same reason) as
 * `scripts/chiefd-workspace-membership-lib.mjs`'s `parseTomlStringArray`:
 * this workspace has no TOML dependency wired at the root, and a dependency
 * table in these manifests is a flat `name = <value>` list under a
 * `[...dependencies]` header. `#`-to-end-of-line comments are stripped
 * first, so a commented-out `# chiefd = { … }` is never read as a live edge
 * — these manifests carry paragraph-long `#` rationale blocks.
 *
 * TARGET-CONDITIONAL TABLES COUNT. All three scan roots already carry
 * `[target.'cfg(not(target_os = "macos"))'.dependencies]`, so a header is
 * matched by SUFFIX — any table whose name ends in `dependencies`,
 * `dev-dependencies` or `build-dependencies`. A boundary crossed only on
 * Linux is still crossed.
 */
export function parseTomlDependencyKeys(manifestText) {
  const keys = []
  let table = null
  for (const rawLine of manifestText.split('\n')) {
    const hashIndex = rawLine.indexOf('#')
    const line = (hashIndex === -1 ? rawLine : rawLine.slice(0, hashIndex)).trimEnd()
    const header = /^\s*\[([^\]]+)\]\s*$/.exec(line)
    if (header) {
      table = header[1].trim()
      continue
    }
    if (table === null) continue
    if (!/(^|\.)(dependencies|dev-dependencies|build-dependencies)$/.test(table)) continue
    const entry = /^\s*([A-Za-z0-9_-]+)\s*=/.exec(line)
    if (entry) keys.push({ table, name: entry[1] })
  }
  return keys
}

/**
 * Every dependency edge from a crate under `roots` to a crate named in
 * `forbidden`, as readable `"<crate> [<table>] -> <dependency>"` strings so
 * the assertion's diff names the edge, the table and both ends.
 *
 * One implementation for both directions: rules 5 and 7 differ ONLY in which
 * set of crates is being read and which set of names is forbidden, and a
 * second copy of this walk is a second place for the two to drift apart.
 */
function forbiddenCargoEdges(repoRoot, roots, forbidden) {
  return roots
    .flatMap((root) => {
      const manifestPath = join(repoRoot, root, 'Cargo.toml')
      if (!existsSync(manifestPath)) return []
      const crate = root.split('/').pop()
      return parseTomlDependencyKeys(readFileSync(manifestPath, 'utf8'))
        .filter((entry) => forbidden.includes(entry.name))
        .map((entry) => `${crate} [${entry.table}] -> ${entry.name}`)
    })
    .sort()
}

/**
 * Rule 5. Scan-root (backend) crates that name a CLI/operator crate in any
 * dependency table.
 */
export function backendCratesDependingOnCli(repoRoot, roots = SCAN_ROOTS) {
  return forbiddenCargoEdges(repoRoot, roots, CLI_CRATE_NAMES)
}

/**
 * Rule 7 — rule 5's MIRROR. Client crates that name a backend crate in any
 * dependency table.
 *
 * This is the direction that will actually be crossed. Nobody is tempted to
 * make `chiefd-core` depend on the CLI; everybody is tempted to make the CLI
 * depend on `chiefd-core` the first time it needs a type the daemon already
 * has — and one such edge makes the API stop being the contract, silently,
 * with a green build. `chief-cli` re-derives the composite document key from
 * the same public facts every other client uses precisely because of this
 * rule.
 */
export function clientCratesDependingOnBackend(repoRoot, clients = CLIENT_CRATES) {
  return forbiddenCargoEdges(repoRoot, clients, BACKEND_CRATE_NAMES)
}



/**
 * Rule 6. Every chiefd crate carrying tmux is either SCANNED or DECLARED OUT
 * OF SCOPE by the scope row, and the scope row agrees with `SCAN_ROOTS`.
 *
 * This is what makes the P6 handoff atomic:
 *   - delete the scope row alone -> `crates/chiefd` carries tmux, is not
 *     scanned and is no longer excluded -> RED.
 *   - grow SCAN_ROOTS alone -> the scope row still declares the old set and
 *     still excludes a directory that is now scanned -> RED.
 *   - both, in one commit -> green.
 * And, independently of P6: a NEW chiefd crate that arrives carrying tmux is
 * red until somebody decides whether it is backend or client. A guard whose
 * subject can shrink without anyone noticing is the failure this repo keeps
 * paying for.
 */
export function cratesOutOfScope(repoRoot, clients = CLIENT_CRATES) {
  const scanned = new Set(SCAN_ROOTS)
  const declaredClients = new Set(clients)
  const drift = []
  for (const crate of chiefdCratesWithTmux(repoRoot)) {
    if (scanned.has(crate)) continue
    if (declaredClients.has(crate)) continue
    drift.push(
      `${crate} carries tmux but is neither in SCAN_ROOTS nor a declared CLIENT crate`,
    )
  }
  return drift.sort()
}

/**
 * The whole check, as an ENUMERATION. Five named arrays; all five empty is the
 * passing state, and the arrays themselves are the report.
 *
 * #751/P10 collapsed the register. Rules 1-4 were `unregisteredFiles`,
 * `staleRegisterRows`, `missingRegisterRows` and `duplicateRegisterRows` — four
 * ways of asking whether a hand-maintained work-list still described the tree.
 * The work-list is finished, so the question is simply **does any file under a
 * scan root name tmux at all**, and there is no row to add, refile or forget.
 * An allowlist can only get less wrong; no allowlist cannot get wrong.
 *
 * The three rules that were never about the register are unchanged: neither
 * side of the client boundary may depend on the other, and a chiefd crate that
 * carries tmux must be a declared CLIENT crate.
 *
 * `scanned` is returned alongside so a caller can prove the scan was not
 * vacuous — a check that only reports pass/fail cannot be told apart from one
 * that silently enumerated nothing (#848).
 */
export function deriveBoundaryReport(repoRoot) {
  const scanned = scanTmuxFiles(repoRoot)
  // #751/P10 follow-up: rule 1 is about CODE. `scanTmuxFiles` has always
  // computed `codeHits`/`commentOnly`; this is the report finally using them.
  //
  // The original rule counted comments too, and its stated reason was good: a
  // comment in `chiefd-core` saying "the tmux pane this respawns" is evidence
  // the code still serves tmux, and a per-file exemption list would be a second
  // classifier that rots. The first half of that is still true. The second half
  // is why this is a uniform lexical rule and NOT a register — there are no
  // rows to add, refile or forget, which is exactly what P10 deleted.
  //
  // What forced the change is that the migration finished. The backend now
  // carries tombstones explaining why tmux left — including one recording that
  // `TMUX_PANE` is set by tmux itself and must never be renamed, which is the
  // exact knowledge whose absence let P9 rewrite it to `RUNTIME_PANE` and break
  // every spawn. A rule that forbids writing that sentence down is a rule that
  // deletes the reason the incident cannot recur.
  //
  // The protection lost here was never the real one. P9 renamed `Tmux*` to
  // `Runtime*` in place and this scan went green while the entire pane machine
  // was still in the backend — proving a text match was never what held the
  // boundary. Rules 2 and 3 are, because a Cargo dependency edge cannot be
  // renamed away.
  const filesNamingTmux = scanned
    .filter((entry) => !entry.commentOnly)
    .map((entry) => `${entry.file}  (${entry.codeHits} tmux code line(s), first: ${entry.firstMatch})`)

  return {
    scanned,
    filesNamingTmux,
    /** Files whose only tmux mentions are comments — reported, never failed. */
    filesNamingTmuxInCommentsOnly: scanned
      .filter((entry) => entry.commentOnly)
      .map((entry) => entry.file),
    backendCratesDependingOnCli: backendCratesDependingOnCli(repoRoot),
    clientCratesDependingOnBackend: clientCratesDependingOnBackend(repoRoot),
    cratesOutOfScope: cratesOutOfScope(repoRoot),
    blindScanRoots: blindScanRoots(repoRoot),
  }
}

/** Number of scanned violation files per scan root. */
export function scannedCountsByCrate(scanned) {
  return Object.fromEntries(SCAN_ROOTS.map((root) => [root.split('/').pop(), scanned.filter((entry) => entry.crate === root).length]))
}

// ---------------------------------------------------------------------------
// #751/P10: THE REGENERATION PATH IS GONE, WITH THE FILE IT WROTE
//
// `--write` existed to rewrite `scripts/fixtures/backend-tmux-register.mjs`
// while ~105 rows were draining, preserving each row's `why` and `packet`. The
// register reached zero in P9 and is deleted, so there is nothing to
// regenerate and no `scopeRowOf`/`fileRowsOf` to read. A guard with no
// hand-maintained input cannot drift from the tree it checks.
// ---------------------------------------------------------------------------
