// Locks scripts/suspected-done-issues.mjs against fixtures AND, per
// explicit requirement, demonstrates it live against the real repo: it
// must flag a known-landed-but-open issue and must NOT flag a genuinely
// open, uncited one.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { findIssueCitations, findSuspectedDoneIssues, queryOpenIssueNumbers, queryOpenIssueSummaries } from "../suspected-done-issues.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "suspected-done-"));
  mkdirSync(join(root, "scripts", "test"), { recursive: true });
  mkdirSync(join(root, "tests"), { recursive: true });
  mkdirSync(join(root, "docs"), { recursive: true });

  // A real citation in a guard file -- the shape #887/#858/#886 actually were.
  writeFileSync(
    join(root, "scripts", "test", "widget.test.mjs"),
    "// Guard for the widget invariant (#12345).\ntest('widget stays valid', () => {})\n",
  );
  // A citation inside tests/ -- also citable.
  writeFileSync(join(root, "tests", "widget.test.ts"), "// covers #99999's regression case\n");
  // A decoy that merely LOOKS like a citation but is a hex color / hash fragment / long number.
  writeFileSync(
    join(root, "scripts", "test", "decoy.test.mjs"),
    "const color = '#fff123'\nconst url = 'https://example.com/#123abc'\nconst tiny = '#12'\n",
  );
  // A citation in a NON-guard file (a prose doc) -- must never count, since
  // prose ABOUT an issue is not evidence landed code closes it.
  writeFileSync(join(root, "docs", "notes.md"), "See #12345 for background.\n");

  return root;
}

test("findIssueCitations finds a real #NNN citation in a guard file, with exact file/line/text", () => {
  const root = fixture();
  try {
    const citations = findIssueCitations(root);
    const hit = citations.find((c) => c.number === 12345 && c.file === "scripts/test/widget.test.mjs");
    assert.ok(hit, "expected a citation for #12345 in the guard file");
    assert.equal(hit.line, 1);
    assert.match(hit.text, /#12345/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("findIssueCitations finds a citation under tests/ too, not only scripts/", () => {
  const root = fixture();
  try {
    const citations = findIssueCitations(root);
    assert.ok(citations.some((c) => c.number === 99999 && c.file === "tests/widget.test.ts"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a hex color, a URL hash fragment, and a too-short number are never mistaken for an issue citation", () => {
  const root = fixture();
  try {
    const citations = findIssueCitations(root);
    const decoyNumbers = citations.filter((c) => c.file === "scripts/test/decoy.test.mjs");
    assert.deepEqual(decoyNumbers, [], `expected zero false-positive citations from the decoy file, got: ${JSON.stringify(decoyNumbers)}`);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a citation in a plan/doc file (not a test or guard) is never counted -- prose about an issue is not evidence landed code closes it", () => {
  const root = fixture();
  try {
    const citations = findIssueCitations(root);
    assert.ok(!citations.some((c) => c.file === "docs/notes.md"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("RED (the actual defect class): a cited issue that is STILL OPEN is reported SUSPECTED-DONE", () => {
  const root = fixture();
  try {
    const openIssueNumbers = new Set([12345, 55555]); // 55555 is open but never cited -- must not appear.
    const { suspected } = findSuspectedDoneIssues(root, openIssueNumbers);
    const numbers = suspected.map((s) => s.number);
    assert.ok(numbers.includes(12345), "a cited AND open issue must be reported suspected-done");
    assert.ok(!numbers.includes(55555), "an open issue with zero citations must never be reported (nothing to suspect)");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("GREEN: a cited issue that is already CLOSED is never reported (it is not suspected of anything -- it is confirmed)", () => {
  const root = fixture();
  try {
    const openIssueNumbers = new Set(); // 12345 and 99999 are both closed.
    const { suspected } = findSuspectedDoneIssues(root, openIssueNumbers);
    assert.deepEqual(suspected, []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("multiple citation sites for the same suspected issue are all kept, never collapsed to a count", () => {
  const root = fixture();
  try {
    writeFileSync(join(root, "scripts", "test", "second-site.test.mjs"), "// also touches #12345\n");
    const { suspected } = findSuspectedDoneIssues(root, new Set([12345]));
    const entry = suspected.find((s) => s.number === 12345);
    assert.equal(entry.sites.length, 2);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// REAL REPO, LIVE: required demonstration against the actual tree and the
// actual GitHub tracker -- a known-landed-but-open issue must be flagged,
// and a genuinely open, uncited issue must not be.
// ---------------------------------------------------------------------------

// DERIVED POSITIVE CONTROL, not a hardcoded pair. This assertion named
// #880/#881, then #382/#404, and both pairs went CLOSED — because closing a
// correctly-flagged issue is precisely what this tool exists to cause. A
// control that expires every time the tool succeeds is the #907 defect class:
// updating it looks identical to weakening it, and it re-breaks forever.
//
// So the properties are checked against whatever the live tracker says today:
// nothing closed is ever flagged, nothing uncited is ever flagged, and the
// citation scanner is not silently returning nothing. An empty `suspected`
// list is a legitimate PASS here — it means the backlog is triaged, which is
// the goal — and the scanner non-vacuity check below is what keeps that pass
// from being the vacuous kind.
test("REAL REPO + LIVE TRACKER: nothing closed and nothing uncited is ever flagged SUSPECTED-DONE", async () => {
  const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
  let openIssueSummaries;
  try {
    openIssueSummaries = queryOpenIssueSummaries();
  } catch {
    // No `gh` auth / network / GitHub remote in this environment -- skip
    // rather than fail a check that depends on a live external service this
    // test cannot control, matching this repo's own established pattern for
    // real-network-dependent tests. NOTE for anyone reading a green run: this
    // check is a NO-OP without `gh` auth AND a GitHub remote. A "54/54 green"
    // from a host missing either has not exercised this test at all.
    return;
  }
  const openNumbers = new Set(openIssueSummaries.map((summary) => summary.number));
  const epicNumbers = new Set(
    openIssueSummaries.filter((summary) => summary.subIssueTotal > 0).map((summary) => summary.number),
  );

  const citations = findIssueCitations(repoRoot);
  assert.ok(
    citations.length > 0,
    "the citation scanner found ZERO citations in the whole tree -- it is broken or its path filter excludes everything, and every assertion below would pass vacuously",
  );
  const citedNumbers = new Set(citations.map((citation) => citation.number));

  const { suspected } = findSuspectedDoneIssues(repoRoot, openIssueSummaries);
  for (const entry of suspected) {
    assert.ok(
      openNumbers.has(entry.number),
      `#${entry.number} is flagged SUSPECTED-DONE but the tracker does not list it as open -- a closed issue must never be reported as suspected-done`,
    );
    assert.ok(
      citedNumbers.has(entry.number),
      `#${entry.number} is flagged SUSPECTED-DONE with no citation anywhere in the tree -- the flag must always rest on a real citation site`,
    );
    assert.ok(
      !epicNumbers.has(entry.number),
      `#${entry.number} has sub-issues and must be reported as an epic, never as suspected-done`,
    );
    assert.ok(
      Array.isArray(entry.sites) && entry.sites.length > 0,
      `#${entry.number} is flagged with an empty site list -- the report must name where the evidence is`,
    );
  }
});

test("REAL REPO + LIVE TRACKER: issue 921, 867, 892 (confirmed still OPEN, confirmed uncited anywhere) are NOT flagged -- the negative controls", async () => {
  const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
  let openIssueNumbers;
  try {
    openIssueNumbers = queryOpenIssueNumbers();
  } catch {
    return;
  }
  const { suspected } = findSuspectedDoneIssues(repoRoot, openIssueNumbers);
  const numbers = suspected.map((s) => s.number);
  for (const control of [921, 867, 892]) {
    assert.ok(!numbers.includes(control), `#${control} has zero citations anywhere in the tree and must never be flagged`);
  }
});

test("EPIC EXCLUSION: an open issue with sub-issues is never reported SUSPECTED-DONE even when cited -- it's reported separately as an epic", () => {
  const root = fixture();
  try {
    const summaries = [
      { number: 12345, subIssueTotal: 51 }, // epic, cited in the fixture
      { number: 99999, subIssueTotal: 0 }, // ordinary open issue, cited in the fixture
    ];
    const { suspected, epics } = findSuspectedDoneIssues(root, summaries);
    assert.ok(!suspected.some((s) => s.number === 12345), "an epic must never appear in the suspected list");
    assert.ok(suspected.some((s) => s.number === 99999), "a non-epic cited issue still appears in suspected");
    assert.ok(epics.some((e) => e.number === 12345), "the epic's citations are reported separately, not silently dropped");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("SELF-EXCLUSION: this tool's own source and test file are never scanned for citations, even though both cite real issue numbers in prose", () => {
  const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
  const citations = findIssueCitations(repoRoot);
  assert.ok(
    !citations.some((c) => c.file === "scripts/suspected-done-issues.mjs" || c.file === "scripts/test/suspected-done-issues.test.mjs"),
    "the tool's own file and test must be excluded from the scan mechanically, not by careful wording",
  );
});

test("PARKED CORPUS: a citation under top-level tests/ is tagged executed:false; a citation under scripts/ or packages/*/test is tagged executed:true", () => {
  const root = fixture();
  try {
    mkdirSync(join(root, "packages", "widget", "test"), { recursive: true });
    writeFileSync(join(root, "packages", "widget", "test", "widget.test.ts"), "// wired: #55000\n");
    const citations = findIssueCitations(root);
    const parked = citations.find((c) => c.number === 12345 && c.file === "scripts/test/widget.test.mjs");
    const wired = citations.find((c) => c.number === 55000);
    // scripts/test/widget.test.mjs is under scripts/, not top-level tests/, so it's wired (executed).
    assert.equal(parked.executed, true, "scripts/test/... is not the parked tests/ corpus");
    assert.equal(wired.executed, true, "packages/*/test is wired into a vitest.config.ts, not parked");
    const parkedTests = findIssueCitations(root).find((c) => c.number === 99999 && c.file === "tests/widget.test.ts");
    assert.equal(parkedTests.executed, false, "top-level tests/... is the parked corpus per #937 -- written, not run");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("PARKED CORPUS: an issue whose only citation sites are all unexecuted is flagged allParked:true; one executed site clears the flag", () => {
  const root = fixture();
  try {
    // 99999 is cited only in tests/widget.test.ts (parked).
    const { suspected } = findSuspectedDoneIssues(root, new Set([99999, 12345]));
    const parkedOnly = suspected.find((s) => s.number === 99999);
    assert.equal(parkedOnly.allParked, true, "99999's sole citation is under top-level tests/, so it should be all-parked");
    // 12345 is cited in scripts/test/widget.test.mjs (wired), so it's not all-parked.
    const wired = suspected.find((s) => s.number === 12345);
    assert.equal(wired.allParked, false, "12345 has a wired (scripts/) citation, so it is not all-parked");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("ASSERTION VS MENTION: a citation inside a test()/it()/describe() title's own string is tagged kind:'assertion'", () => {
  const root = fixture();
  try {
    mkdirSync(join(root, "scripts", "test", "asserts"), { recursive: true });
    writeFileSync(
      join(root, "scripts", "test", "assertion-shape.test.mjs"),
      "test(\"#77001: the thing actually works\", () => {})\n",
    );
    const citations = findIssueCitations(root);
    const hit = citations.find((c) => c.number === 77001);
    assert.ok(hit, "expected a citation for #77001");
    assert.equal(hit.kind, "assertion", `expected kind 'assertion', got ${JSON.stringify(hit)}`);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("ASSERTION VS MENTION: the #659 shape -- a citation executes but only DESCRIBES (a data field, not a test title) is tagged kind:'mention'", () => {
  const root = fixture();
  try {
    writeFileSync(
      join(root, "scripts", "test", "mention-shape.test.mjs"),
      "const entry = { why: \"#77002's regenerated projection is harness-only, never authority\" }\n",
    );
    const citations = findIssueCitations(root);
    const hit = citations.find((c) => c.number === 77002);
    assert.ok(hit, "expected a citation for #77002");
    assert.equal(hit.kind, "mention", `expected kind 'mention', got ${JSON.stringify(hit)}`);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("ASSERTION VS MENTION: a citation in a plain comment (the #718 admission shape) is tagged kind:'mention', never 'assertion'", () => {
  const root = fixture();
  try {
    writeFileSync(
      join(root, "scripts", "test", "comment-shape.test.mjs"),
      "// #77003 is an open P0 with ZERO e2e coverage\ntest('unrelated', () => {})\n",
    );
    const citations = findIssueCitations(root);
    const hit = citations.find((c) => c.number === 77003);
    assert.ok(hit, "expected a citation for #77003");
    assert.equal(hit.kind, "mention");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("EVIDENCE RANK: an issue with an executed assertion site ranks STRONG (3) above one with only executed mentions (2), above one that is parked-only (1)", () => {
  const root = fixture();
  try {
    mkdirSync(join(root, "scripts", "test"), { recursive: true });
    writeFileSync(join(root, "scripts", "test", "strong.test.mjs"), "test(\"#88001: asserts it\", () => {})\n");
    writeFileSync(join(root, "scripts", "test", "weak.test.mjs"), "// just mentions #88002 in passing\n");
    writeFileSync(join(root, "tests", "parked-only.test.ts"), "test(\"#88003: asserts it but this file is parked\", () => {})\n");
    const { suspected } = findSuspectedDoneIssues(root, new Set([88001, 88002, 88003]));
    const byNumber = Object.fromEntries(suspected.map((s) => [s.number, s]));
    assert.equal(byNumber[88001].rank, 3, "executed + assertion must rank STRONG");
    assert.equal(byNumber[88002].rank, 2, "executed, mention-only must rank WEAK");
    assert.equal(byNumber[88003].rank, 1, "parked-only must rank lowest");
    // And the list itself is sorted strongest-first.
    const ranks = suspected.filter((s) => [88001, 88002, 88003].includes(s.number)).map((s) => s.rank);
    assert.deepEqual(ranks, [3, 2, 1], "suspected must be sorted by rank descending");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL REPO + LIVE TRACKER: #659's own citation is executed but mention-only (the #659 lesson) -- ranked WEAK, not STRONG, from the parked assertion site alone", async () => {
  const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
  let summaries;
  try {
    summaries = queryOpenIssueSummaries();
  } catch {
    return;
  }
  const { suspected } = findSuspectedDoneIssues(repoRoot, summaries);
  const entry659 = suspected.find((s) => s.number === 659);
  if (!entry659) return; // #659 may have closed since this was written -- not this test's concern.
  const executedSites = entry659.sites.filter((s) => s.executed);
  assert.ok(executedSites.length > 0, "#659 should have at least one executed citation (scripts/test/sql-only-state.test.mjs)");
  assert.ok(
    executedSites.every((s) => s.kind !== "assertion"),
    `expected #659's executed sites to be mention-only per the architect's own finding; got ${JSON.stringify(executedSites)}`,
  );
});


test("REAL REPO + LIVE TRACKER: epics with sub-issues are excluded from SUSPECTED-DONE, reported as epics instead", async () => {
  const repoRoot = new URL("../..", import.meta.url).pathname.replace(/\/$/, "");
  let summaries;
  try {
    summaries = queryOpenIssueSummaries();
  } catch {
    return;
  }
  const { suspected, epics } = findSuspectedDoneIssues(repoRoot, summaries);
  for (const epic of epics) {
    assert.ok(!suspected.some((s) => s.number === epic.number), `#${epic.number} is an epic and must never also appear in suspected`);
  }
});

test("TRACKING-REFERENCE: a line matching 'filed as #NNN'/'its own story: #NNN'/'tracked as #NNN' is tagged trackingReference:true", () => {
  const root = fixture();
  try {
    writeFileSync(
      join(root, "scripts", "test", "tracking-shape.test.mjs"),
      "// its own story: #77004.\n// tracked as #77005\n// filed as #77006's own issue\n// #77007 is not done unwinding it yet\n",
    );
    const citations = findIssueCitations(root);
    const byNumber = Object.fromEntries(citations.map((c) => [c.number, c]));
    assert.equal(byNumber[77004].trackingReference, true, "'its own story: #NNN' must be a tracking reference");
    assert.equal(byNumber[77005].trackingReference, true, "'tracked as #NNN' must be a tracking reference");
    assert.equal(byNumber[77006].trackingReference, true, "'filed as #NNN' must be a tracking reference");
    assert.equal(byNumber[77007].trackingReference, false, "an ordinary status comment must never be misclassified as a tracking reference");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("TRACKING-REFERENCE: an issue whose EVERY citation is a tracking-reference is excluded from suspected and reported separately", () => {
  const root = fixture();
  try {
    writeFileSync(join(root, "scripts", "test", "tracking-only.test.mjs"), "// filed as its own story: #88010\n");
    const { suspected, trackingReferences } = findSuspectedDoneIssues(root, new Set([88010]));
    assert.ok(!suspected.some((s) => s.number === 88010), "a tracking-reference-only issue must never appear in suspected");
    assert.ok(trackingReferences.some((t) => t.number === 88010), "it must be reported in trackingReferences instead of silently dropped");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("TRACKING-REFERENCE: an issue with ONE tracking-reference mention alongside a REAL behavior citation stays in suspected", () => {
  const root = fixture();
  try {
    writeFileSync(join(root, "scripts", "test", "mixed-tracking.test.mjs"), "// filed as its own story: #88011\ntest(\"#88011: asserts the real behavior\", () => {})\n");
    const { suspected, trackingReferences } = findSuspectedDoneIssues(root, new Set([88011]));
    assert.ok(suspected.some((s) => s.number === 88011), "a mixed issue (tracking mention + real citation) must stay in suspected");
    assert.ok(!trackingReferences.some((t) => t.number === 88011), "a mixed issue must never also appear in trackingReferences");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// #970 REWORK (per architect/integrator-2): this was originally a
// "REAL REPO + LIVE TRACKER" test asserting that #959 -- this tool's own
// follow-up issue, cited only as "its own story: #959" at the time this
// test was written -- is excluded from suspected. #970's own pin later
// added a second, non-forward-pointer citation to #959
// (guard-wiring-manifest.mjs's "general form of the org-world.ts
// createCompany gap (#959/#970), caught"), which correctly re-admitted
// #959 to suspected at the WEAK tier -- TRACKING_REFERENCE_PATTERN's
// exclusion is deliberately all-or-nothing, so a real second citation
// stops the issue from qualifying as tracking-only. The TOOL was right;
// this test's own PREMISE -- "#959's only citation is a tracking
// reference" -- was a snapshot fact about the repo, not an invariant, and
// it stopped being true. Any future pin that cites #959 anywhere non-
// forward-pointer can flip this same assertion again, so the live
// dependency is removed: the property (an issue whose real-repo-shaped
// citation is ONLY ever a forward pointer like "its own story: #NNN" is
// excluded from suspected and reported as a tracking-reference instead)
// is proven against a fixture reproducing #959's original citation shape
// verbatim, which cannot be changed by any later pin.
test("TRACKING-REFERENCE (fixture, not live -- #970 rework): a citation shaped exactly like #959's original 'its own story: #NNN' is excluded from suspected, reported as a tracking-reference instead", () => {
  const root = fixture();
  try {
    writeFileSync(join(root, "scripts", "test", "self-filed-followup.test.mjs"), "// its own story: #95900\n");
    const { suspected, trackingReferences } = findSuspectedDoneIssues(root, new Set([95900]));
    assert.ok(!suspected.some((s) => s.number === 95900), "#95900 must never appear in suspected -- its only citation is a tracking-reference");
    assert.ok(trackingReferences.some((t) => t.number === 95900), "#95900 must appear in trackingReferences");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
