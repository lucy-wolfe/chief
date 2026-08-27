// Tonight's own #926 screening found 10 of 18 open children already
// landed — #926's real completion was 42/51, not 32/51, a 20-point
// undercount three engineers separately burned a first pass rediscovering
// by hand. The signal eng-3 actually used to find them was mechanical:
// A LANDED test or guard file that CITES AN ISSUE NUMBER which the
// tracker still shows open. A test naming the issue it closes is a fixed
// defect that forgot to close its own ticket.
//
//   #887 -> scripts/test/chiefd-workspace-membership.test.mjs (header cites #887)
//   #858 -> an assertion message: "...in a sibling test file (#858)"
//   #886 -> typecheck.sh comment: "#886: apps/cli joined this reference graph..."
//   #880 -> RowsContract.test.ts: "which is #880's regression case"
//   #881 -> StaffingTest.test.ts: "StaffingClient — DIRECT_CASES ... (#881)"
//
// This derives that same signal from the tree instead of a human re-doing
// it by hand the next time an epic's percentage needs checking.
//
// REPORTS ONLY -- NEVER CLOSES, NEVER FAILS A BUILD. A citation can mean
// "this test COVERS #NNN" as easily as "this test CLOSES #NNN, and simply
// forgot to say so" -- the difference needs a human, every time. This
// script's own output is SUSPECTED-DONE, not DONE, and it exits 0
// regardless of findings: an advisory signal that blocked CI would train
// engineers to silence it, the same failure mode a false alarm always
// produces.
//
// A REAL AMBIGUOUS CASE, FOUND WHILE BUILDING THIS, KEPT AS DOCUMENTATION
// OF WHY "SUSPECTED" IS THE RIGHT WORD: `tests/team-ui.test.ts` cites
// `#843` (confirmed OPEN) in a comment that explicitly says "not itself
// rewritten here" -- a citation that names an issue's CONTEXT while
// plainly disclaiming having closed it. This script still reports #843 as
// SUSPECTED (it cannot read the sentence), and that is correct: a human
// glancing at the citation resolves it in seconds, which is exactly the
// job this script hands off rather than gets wrong silently.
//
// SCOPE, STATED EXPLICITLY (never implied complete): this finds issues
// whose number appears in landed test/guard code as a `#NNN` citation. It
// CANNOT find the other class tonight's screening also caught -- #857,
// #875, #883, #885, #879, #511 were identified by READING the tree for
// behavior that matches an issue's description, never by a citation
// existing anywhere. A completeness claim here is a claim about a search
// for text, not a claim about which issues are actually done.
//
// EPICS: an open issue with sub-issues is cited by every child's test by
// construction, so it is excluded from SUSPECTED-DONE (reported separately,
// never silently dropped) rather than treated as a false completion signal.
// THIS EXCLUSION DOES NOT, AND STRUCTURALLY CANNOT, COVER AN ORPHAN EPIC --
// an issue that is in substance a broad initiative (cited defensibly by many
// unrelated files, the #450/#509 "programme-label" shape) but carries ZERO
// linked GitHub sub-issues. `subIssuesSummary.total > 0` is the only signal
// this script has for "epic"; an issue with `total === 0` is indistinguishable
// from an ordinary single-defect issue by that field alone, no matter how
// epic-shaped its citation footprint looks. Checked live against the real
// board while building this pass: the four issues actually labeled `epic`
// (#751/#796/#821/#832) all have children and are correctly excluded; no
// OPEN issue on the board right now is both epic-shaped and has zero
// children (checked via `label:epic` and via the `E<N>:` title-prefix
// convention). That is a fact about tonight's board, not a property of the
// code -- if such an issue is ever filed, this script will flag it as an
// ordinary SUSPECTED-DONE candidate, indistinguishable from a real one,
// and a human reading the RANK/citation-count/file-spread has to catch it.
//
// SELF-CITATION: this file and its test cite worked-example issue numbers
// in prose/comments; both are excluded from the file walk mechanically
// rather than by careful wording, since wording drifts and mechanics don't.
//
// TRACKING-REFERENCES (the #959 lesson, one layer below self-citation): a
// packet that files a follow-up issue and cites it in the SAME commit's
// comment ("its own story: #959") is structurally identical to this file
// citing its own worked examples -- a forward pointer to future work, not
// evidence the pointed-to issue is done. Any issue whose EVERY citation
// site matches a tracking-reference phrase ("tracked as", "filed as", "its
// own story/issue/ticket", "follow-up:") is excluded from SUSPECTED-DONE
// the same way an epic is: reported separately, never silently dropped. A
// mixed issue (one tracking-reference mention alongside a real behavior
// citation) stays in SUSPECTED-DONE -- the bookkeeping mention doesn't
// invalidate real evidence sitting next to it.
//
// IN-FLIGHT BRANCHES: this only scans the checked-out tree. A citation that
// exists only on an unlanded branch (e.g. active #828/#830/#820/#951 work)
// is invisible here -- it looks identical to "not yet cited anywhere" until
// that branch lands. Never treat an absence here as proof no one is close.
//
// PARKED CORPUS: per #937, every file directly under the top-level `tests/`
// runs (if at all) only via the bare `bun test tests` script -- no package's
// vitest.config.ts includes it, and turbo's `test:unit` task graph never
// reaches it. A citation there is a WRITTEN test, not a PASSING one: it
// proves intent, not behavior. Every citation site is tagged `executed:
// false` for a top-level `tests/...` path and `executed: true` otherwise
// (packages/*/test, apps/*/test, scripts/ -- all wired into a task that
// actually runs), and an issue whose ONLY citations are unexecuted is
// weaker evidence than one with at least one executed site. This is a path
// heuristic, not a live CI check -- it cannot tell a `tests/` file that
// happens to also be exercised by some other harness from one that never
// runs at all, so treat `executed: true` as "wired to run," never as "seen
// green."
//
// ASSERTION VS. MENTION (the #659 lesson): a citation being EXECUTED is
// necessary but not sufficient -- #659's own citation in
// `scripts/test/sql-only-state.test.mjs` executes, and merely DESCRIBES a
// write's rationale in a `why:` field ("#659's regenerated non-secret
// launch-contract projection...") rather than ASSERTING the behavior. The
// citation that actually asserts #659 (`tests/org-intercom.test.ts:693`,
// `test("#659: reload advances only immutable...")`) is under the parked
// `tests/` corpus and is UNEXECUTED. Every citation is tagged `kind`:
// 'assertion' (the line matches a `test(`/`it(`/`describe(`-style call
// whose own first string argument contains the issue number -- the
// strongest, cheaply-detectable shape), or 'mention' (everything else --
// a comment, a data field, a throw message, a doc header). This is a
// STRUCTURAL, not semantic, classification: it cannot tell a disclaiming
// comment ("not itself rewritten here", #843's own documented case) from
// an affirming one ("which is #880's regression case") -- both are
// 'mention'. Deliberately not attempted: doing that needs sentence
// parsing, which this script refuses the same way it refuses to resolve
// "covers" vs "closes" -- a human reads the citation TEXT already printed
// alongside every site. The only claim this axis makes is EXECUTED +
// ASSERTION is the strongest evidence shape; anything else is weaker,
// stated as such, never silently treated as equal.

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { extname, join, relative } from "node:path";

import { skipSet } from "./tree-walk-lib.mjs";

const SELF_EXCLUDED_PATHS = new Set(["scripts/suspected-done-issues.mjs", "scripts/test/suspected-done-issues.test.mjs"]);

const EXCLUDED_DIRS = skipSet();
const CITABLE_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts", ".mjs", ".sh"]);
// Guard/test files only -- a citation in a doc or in CHANGELOG.md
// entry is prose ABOUT an issue, not evidence that LANDED CODE closes it,
// and mixing the two would make "landed" indistinguishable from "discussed".
const CITABLE_PATH_PATTERN = /(^|\/)(test|tests)(\/|$)|\.test\.[mc]?ts$|(^|\/)scripts\//;

// A `#` immediately followed by 3-5 digits, with a non-digit (or nothing)
// on both sides, so a CSS hex color (#fff), a URL fragment, or a 1-2
// digit number too small to be a real issue in this tracker can never
// match. Never a broader `#\d+` -- that pattern already burned #939's own
// audit once (see turbo-env-audit.mjs's own history of false positives
// from under-scoped patterns).
const CITATION_PATTERN = /(?<![#\w])#(\d{3,5})(?![\da-zA-Z])/g;

// Top-level `tests/...` is not wired into any package's vitest.config.ts or
// turbo's test:unit graph (per #937) -- a citation there is written, not run.
const PARKED_PATH_PATTERN = /^tests\//;

// A `test(`/`it(`/`describe(`-style call (JS/TS) opening a quoted string on
// the SAME line as the citation -- the cheap, structural signal for "this
// citation sits in a test's own title," never proof the number appears
// INSIDE that string (checked separately, below).
const TEST_CALL_PATTERN = /\b(?:test|it|describe)(?:\.(?:only|skip|todo|each\([^)]*\)))?\s*\(\s*[`'"]/;

/** 'assertion' when the citing line opens a test/it/describe call and the
 *  issue number appears inside that call's own leading string argument;
 *  'mention' otherwise (comment, data field, throw message, doc header --
 *  a structural bucket, never a semantic one; see the header comment). */
function classifyCitationKind(line, numberText) {
  const match = TEST_CALL_PATTERN.exec(line);
  if (!match) return "mention";
  // The quoted string starts right after the match; ensure the issue
  // number's own text falls after the call opens (so a citation earlier
  // on the same line, e.g. in a preceding comment, is never miscredited
  // to a test call that follows it).
  const numberIndex = line.indexOf(numberText, match.index);
  return numberIndex >= match.index + match[0].length - 1 ? "assertion" : "mention";
}

// #959: a citation phrased as a forward pointer to a NOT-YET-DONE follow-up
// ("filed as #959", "tracked as #NNN", "its own story: #NNN", "follow-up:
// #NNN") is bookkeeping about the tracker, not evidence about behavior --
// the same class as this tool's own self-citation, one level down: a
// packet that files a follow-up issue and cites it in the same commit's
// comment is structurally identical to this file citing its own worked
// examples. Cheap and structural, same discipline as the assertion/mention
// axis: a phrase match, never an attempt to read whether the CITED issue
// is actually done.
const TRACKING_REFERENCE_PATTERN = /\b(?:tracked as|filed as|its own (?:story|issue|ticket)|follow-?up:?)\b.*#\d{3,5}/i;

function isTrackingReferenceLine(line) {
  return TRACKING_REFERENCE_PATTERN.test(line);
}

function walkFiles(root) {
  const out = [];
  (function walk(dir) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        if (EXCLUDED_DIRS.has(entry.name)) continue;
        walk(join(dir, entry.name));
        continue;
      }
      if (entry.isFile() && CITABLE_EXTENSIONS.has(extname(entry.name))) out.push(join(dir, entry.name));
    }
  })(root);
  return out;
}

/** Every `#NNN` citation in landed test/guard code, with file + line +
 *  the citing line's own text (so a human can judge "covers" vs "closes"
 *  without re-opening the file). Real per-line scanning, not a single
 *  whole-file regex exec, so line numbers are exact. */
export function findIssueCitations(root) {
  const citations = [];
  for (const file of walkFiles(root)) {
    const relPath = relative(root, file);
    if (SELF_EXCLUDED_PATHS.has(relPath)) continue;
    if (!CITABLE_PATH_PATTERN.test(relPath)) continue;
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, index) => {
      for (const match of line.matchAll(CITATION_PATTERN)) {
        citations.push({
          number: Number(match[1]),
          file: relPath,
          line: index + 1,
          text: line.trim(),
          executed: !PARKED_PATH_PATTERN.test(relPath),
          kind: classifyCitationKind(line, match[0]),
          trackingReference: isTrackingReferenceLine(line),
        });
      }
    });
  }
  return citations;
}

/** Every currently-open issue's number plus its sub-issue total, via `gh`.
 *  Never cached, never a snapshot committed to the repo -- an issue's
 *  open/closed state is exactly the kind of fact that goes stale the
 *  moment it's written down anywhere but the tracker itself. */
export function queryOpenIssueSummaries(repo = "tribes-protocol/chief") {
  const raw = execFileSync(
    "gh",
    ["issue", "list", "--repo", repo, "--state", "open", "--limit", "1000", "--json", "number,subIssuesSummary"],
    { encoding: "utf8" },
  );
  const issues = JSON.parse(raw);
  return issues.map((i) => ({ number: i.number, subIssueTotal: i.subIssuesSummary?.total ?? 0 }));
}

/** Back-compat-free convenience: just the open issue numbers, no epic
 *  distinction. Used only where the caller genuinely doesn't care. */
export function queryOpenIssueNumbers(repo = "tribes-protocol/chief") {
  return new Set(queryOpenIssueSummaries(repo).map((i) => i.number));
}

/** Evidence-strength rank for one issue's citation sites, highest first:
 *  3 = at least one EXECUTED + ASSERTION site (the #862/#699 shape --
 *      strongest available evidence);
 *  2 = at least one EXECUTED site, none of them an assertion (the #659
 *      shape -- runs, but only describes);
 *  1 = every site is unexecuted (parked-only, the #382/#436/#748 shape);
 *  Ties broken by total citation-site count, descending (more independent
 *  citations is weakly stronger, never a substitute for tier). */
function evidenceRank(sites) {
  const hasExecutedAssertion = sites.some((s) => s.executed && s.kind === "assertion");
  const hasExecuted = sites.some((s) => s.executed);
  if (hasExecutedAssertion) return 3;
  if (hasExecuted) return 2;
  return 1;
}

/** The intersection: every open, non-epic issue number cited by landed
 *  code, grouped, with every citation site kept (never collapsed to a
 *  count -- the citation TEXT is what lets a human resolve "covers" vs
 *  "closes"). Epics (subIssueTotal > 0) are reported separately in
 *  `epics`, since they're cited by every child's test by construction and
 *  would otherwise be a permanent false-positive stream. `suspected` is
 *  sorted by evidence rank (strongest first), not issue number -- see
 *  `evidenceRank`. */
export function findSuspectedDoneIssues(root, openIssueSummaries) {
  const summaries = openIssueSummaries instanceof Set ? [...openIssueSummaries].map((number) => ({ number, subIssueTotal: 0 })) : openIssueSummaries;
  const openNumbers = new Set(summaries.map((s) => s.number));
  const epicNumbers = new Set(summaries.filter((s) => s.subIssueTotal > 0).map((s) => s.number));
  const citations = findIssueCitations(root);
  const byNumber = new Map();
  const epicByNumber = new Map();
  for (const citation of citations) {
    if (!openNumbers.has(citation.number)) continue;
    const target = epicNumbers.has(citation.number) ? epicByNumber : byNumber;
    if (!target.has(citation.number)) target.set(citation.number, []);
    target.get(citation.number).push(citation);
  }
  // #959: an issue whose EVERY citation site is a tracking-reference
  // (a forward pointer like "filed as #959") is bookkeeping about the
  // tracker, not evidence -- moved to its own bucket, reported (never
  // silently dropped), same discipline as the epic exclusion. An issue
  // with a MIX of tracking-reference and real citations stays in
  // `suspected`: one bookkeeping mention elsewhere doesn't invalidate a
  // real behavior citation sitting alongside it.
  const trackingByNumber = new Map();
  for (const [number, sites] of [...byNumber.entries()]) {
    if (sites.every((s) => s.trackingReference)) {
      trackingByNumber.set(number, sites);
      byNumber.delete(number);
    }
  }
  const toRanked = (map) =>
    [...map.entries()]
      .map(([number, sites]) => ({ number, sites, allParked: sites.every((s) => !s.executed), rank: evidenceRank(sites) }))
      .sort((a, b) => b.rank - a.rank || b.sites.length - a.sites.length || a.number - b.number);
  const toSorted = (map) =>
    [...map.entries()]
      .map(([number, sites]) => ({ number, sites, allParked: sites.every((s) => !s.executed) }))
      .sort((a, b) => a.number - b.number);
  return { suspected: toRanked(byNumber), epics: toSorted(epicByNumber), trackingReferences: toSorted(trackingByNumber) };
}

const RANK_LABEL = { 3: "STRONG (executed + asserts)", 2: "WEAK (executed, only mentions)", 1: "PARKED-ONLY (never executed)" };

function main() {
  const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
  let openIssueSummaries;
  try {
    openIssueSummaries = queryOpenIssueSummaries();
  } catch (error) {
    console.error(`[suspected-done-issues] could not query open issues via gh: ${error instanceof Error ? error.message : error}`);
    console.error("REFUSING TO REPORT -- a stale or empty open-issue set would make every citation look either falsely suspected or falsely clean.");
    process.exit(2);
  }
  const { suspected, epics, trackingReferences } = findSuspectedDoneIssues(root, openIssueSummaries);
  console.log(`[suspected-done-issues] ${openIssueSummaries.length} open issues queried; ${suspected.length} cited by landed test/guard code.`);
  for (const { number, sites, allParked, rank } of suspected) {
    const weaknessTag = allParked ? " [ALL SITES UNEXECUTED -- parked tests/, written but never run]" : "";
    console.log(
      `\nSUSPECTED-DONE: #${number} [${RANK_LABEL[rank]}] (${sites.length} citation site(s))${weaknessTag} -- REPORTED ONLY, verify by hand before closing:`,
    );
    for (const site of sites) console.log(`  ${site.executed ? " " : "!"} ${site.kind === "assertion" ? "A" : "m"} ${site.file}:${site.line}  ${site.text}`);
  }
  if (epics.length > 0) {
    console.log(
      `\n${epics.length} epic(s) excluded from citation-matching (cited by every child's test by construction): ` +
        epics.map((e) => `#${e.number}`).join(", "),
    );
  }
  if (trackingReferences.length > 0) {
    console.log(
      `\n${trackingReferences.length} issue(s) excluded as tracking-references only (every citation is a forward pointer, e.g. "filed as #NNN", never a behavior claim): ` +
        trackingReferences.map((e) => `#${e.number}`).join(", "),
    );
  }
  console.log(
    "\nSCOPE: this finds issues whose number appears in landed test/guard code as a #NNN citation. It cannot find issues closed " +
      "without ever being cited (identified only by reading the tree for matching behavior) -- a completeness claim here is a claim " +
      "about a text search, not about which issues are actually done. It only scans the checked-out tree: a citation that exists " +
      "only on an unlanded branch is invisible here and must never be read as proof no one is close. A citation under top-level " +
      "tests/ is WRITTEN, not PASSING -- no package wires that directory into a running task (#937); such sites are marked " +
      "unexecuted. A citation can also RUN and merely MENTION an issue (a comment, a data field, a throw message) rather than " +
      "ASSERT its behavior (a test/it/describe title) -- ranked STRONG only when at least one site is both executed and an " +
      "assertion, WEAK when executed sites exist but none assert, PARKED-ONLY when every site is unexecuted. An issue whose " +
      "every citation is a tracking-reference (a forward pointer to a follow-up issue, e.g. this tool's own #959) is excluded " +
      "the same way an epic is -- reported separately, never silently dropped. The epic exclusion " +
      "(subIssuesSummary.total > 0) cannot see an ORPHAN epic -- a broad-initiative issue with zero linked sub-issues reads as an " +
      "ordinary issue to this script no matter how many unrelated files cite it; checked live and no such issue exists on the open " +
      "board right now (see header), but this is a fact about tonight's board, not a property of the code. Never auto-closes; " +
      "never fails this process (advisory only).",
  );
  process.exit(0);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
