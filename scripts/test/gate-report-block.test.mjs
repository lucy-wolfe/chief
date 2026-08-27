// Guard for gate-matrix.sh's REPORT-READY BLOCK.
//
// Running a gate is one command; reporting it is a separate act performed
// later, by someone already busy fixing what they saw. That second step is the
// one that gets skipped — three finished runs went unreported in a single
// night — and from outside, a finished-and-unreported run and a still-running
// run are indistinguishable. The block exists so reporting is a copy rather
// than a composition.
//
// These tests assert the properties that were paid for, not the formatting:
// the block prints on BOTH exit paths, it names the HOST, it names the SHA
// *and* whether that SHA matches an origin ref, and it reports failures by
// IDENTITY rather than by package name.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const driver = readFileSync(join(repoRoot, "scripts", "gate-matrix.sh"), "utf8");

/** The block's body: everything between the opening banner and the closing rule. */
function reportBlock() {
  const start = driver.indexOf('echo "===== GATE REPORT ====="');
  const end = driver.indexOf('echo "======================="', start);
  assert.notEqual(start, -1, "no GATE REPORT banner in gate-matrix.sh");
  assert.notEqual(end, -1, "no closing rule after the GATE REPORT banner");
  return driver.slice(start, end);
}

test("the report block exists and is emitted before the driver exits", () => {
  const block = driver.indexOf('echo "===== GATE REPORT ====="');
  const exit = driver.lastIndexOf("exit $rc");
  assert.ok(block !== -1, "no report block");
  assert.ok(block < exit, "the report block must precede the final exit");
});

test("it prints on BOTH paths — it is not inside a success-only branch", () => {
  // A block that only appears on green is a block nobody sees at the moment it
  // matters most. The emit must not be guarded by an rc/success conditional.
  const before = driver.slice(0, driver.indexOf('echo "===== GATE REPORT ====="'));
  const lastIf = before.lastIndexOf("\nif ");
  const lastFi = before.lastIndexOf("\nfi");
  assert.ok(lastFi > lastIf, "the report block appears to sit inside an unclosed conditional");
});

test("it names the HOST — a status without one cannot be verified by anyone else", () => {
  assert.match(reportBlock(), /host:.*\$\(hostname\)/);
});

test("it names the SHA", () => {
  assert.match(reportBlock(), /sha:/);
  assert.match(driver, /GATE_SHA=\$\(git -C "\$ROOT" rev-parse HEAD/);
});

test("it states whether the SHA matches an origin ref — a report that does not say what it gated is a green about an unnamed tree", () => {
  assert.match(driver, /GATE_ORIGIN_STATE/);
  assert.match(driver, /for-each-ref/, "origin-ref comparison must be derived, not asserted");
  assert.match(driver, /no matching origin ref/, "the no-match case must be stated, not left blank");
});

test("it carries the exit status and the duration", () => {
  const block = reportBlock();
  assert.match(block, /GATE_MATRIX_EXIT:\$rc/);
  assert.match(block, /seconds:/);
});

test("it reports failures BY IDENTITY, never by package name alone", () => {
  const block = reportBlock();
  assert.match(block, /by identity, never by package/);
  // A test-file name and a stack terminus are both identity; a package total is not.
  assert.match(block, /FAIL \+\[\^ \]\+\\\.test\\\.\(ts\|mjs\)/);
  assert.match(block, /at \[A-Za-z\]/, "must extract a stack terminus, not only a file name");
});

test("it says 'none' explicitly when there are no failures rather than printing nothing", () => {
  // An empty failures section and a missing failures section look identical;
  // that ambiguity is the whole class this program has been chasing.
  assert.match(reportBlock(), /echo "  none"/);
});

test("it prints the per-package census, and names the absence of the log rather than skipping it", () => {
  const block = reportBlock();
  assert.match(block, /packages:/);
  assert.match(block, /test:unit log absent/, "a missing log must be stated, not silently omitted");
});

test("the failures section is SCOPED to the failing package, not grepped over the whole log", () => {
  // The first version grepped the entire test:unit log for `at <fn> (<path>)`
  // and took the first three matches. On a real run that lifted `handleAction`
  // frames out of an unrelated PASSING test's log line and presented them as
  // the failure's stack — a plausible wrong answer in the field whose whole
  // purpose is identity. Frames are only identity if they belong to the failure.
  const block = reportBlock();
  assert.match(block, /failed_pkgs=/, "must first determine WHICH packages failed");
  assert.match(block, /pkg_lines=.*grep -F/, "must restrict extraction to that package's own lines");
  assert.match(block, /SCOPED to the FAILING PACKAGE/, "the reason must be recorded where the next reader will change it");
});

test("it says so explicitly when no package reported a failing count, rather than printing an empty section", () => {
  assert.match(reportBlock(), /no package reported a failing test count/);
});
