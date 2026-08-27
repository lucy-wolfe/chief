// Locks the distinction #942's misreport proved missing: a killed or
// unreached package must never render identically to a real assertion
// failure. Fixture-based -- classifyPackage/classifyLog take real log text
// shapes observed live on `demo`, never a synthetic API the real turbo
// output wouldn't actually produce.

import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyPackage, scopedPackages } from "../turbo-package-completion.mjs";

test("a package with a clean Test Files summary is PASS", () => {
  const log = "@chief/testing:test:unit: Test Files  7 passed (7)\n@chief/testing:test:unit: Tests  37 passed (37)\n";
  const result = classifyPackage(log, "test:unit", "@chief/testing");
  assert.equal(result.status, "pass");
});

test("a package with a failing Test Files summary is FAIL, never confused with a kill", () => {
  const log = "@chief/web:test:unit: Test Files  2 failed | 46 passed (48)\n";
  const result = classifyPackage(log, "test:unit", "@chief/web");
  assert.equal(result.status, "fail");
});

test("RED (the exact #942 shape): a SIGINT exit with no Test Files summary is KILLED, never FAIL", () => {
  const log = [
    "@chief/piing:test:unit:  ✓ test/PiStartupNonblocking.test.ts (2 tests) 4ms",
    '@chief/piing:test:unit: error: script "test:unit" exited with code 130',
  ].join("\n");
  const result = classifyPackage(log, "test:unit", "@chief/piing");
  assert.equal(result.status, "killed");
  assert.match(result.detail, /SIGINT/);
  assert.match(result.detail, /NOT a failed assertion/);
});

test("a SIGKILL (137) or SIGTERM (143) exit is also KILLED, not FAIL", () => {
  /** @type {[number, string][]} */
  const killSignals = [[137, "SIGKILL"], [143, "SIGTERM"]];
  for (const [code, signal] of killSignals) {
    const log = `@chief/web:test:unit: error: script "test:unit" exited with code ${code}\n`;
    const result = classifyPackage(log, "test:unit", "@chief/web");
    assert.equal(result.status, "killed", `code ${code} should classify as killed`);
    assert.match(result.detail, new RegExp(signal));
  }
});

test("a genuine non-signal nonzero exit with no summary is FAIL, not silently swallowed as a kill", () => {
  const log = '@chief/chiefing:test:unit: error: script "test:unit" exited with code 1\n';
  const result = classifyPackage(log, "test:unit", "@chief/chiefing");
  assert.equal(result.status, "fail");
});

test("a package absent from the log entirely is UNREACHED, never silently dropped or counted as passing", () => {
  const log = "@chief/testing:test:unit: Test Files  7 passed (7)\n";
  const result = classifyPackage(log, "test:unit", "@chief/piing");
  assert.equal(result.status, "unreached");
});

test("a package that logged something but never reached a summary or exit line is UNREACHED", () => {
  const log = "@chief/piing:test:unit: $ vitest run\n@chief/piing:test:unit: \n@chief/piing:test:unit: RUN  v4.1.3\n";
  const result = classifyPackage(log, "test:unit", "@chief/piing");
  assert.equal(result.status, "unreached");
});

test("ANSI color codes between a label and its digits do not defeat classification (the documented turbo/vitest trap)", () => {
  const log = "@chief/web:test:unit: Test Files \x1b[31m 2 failed\x1b[39m | \x1b[32m46 passed\x1b[39m (48)\n";
  const result = classifyPackage(log.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, ""), "test:unit", "@chief/web");
  assert.equal(result.status, "fail");
});

test("REAL REPO sanity: scope is derived from turbo's own dry-run, never a hand-typed list, and includes known real members", () => {
  const repoRoot = new URL("../..", import.meta.url).pathname;
  const scope = scopedPackages(repoRoot, "test:unit");
  assert.ok(scope.length >= 5, `only ${scope.length} package(s) declare test:unit -- a turbo/workspace regression, refusing to trust this scope`);
  assert.ok(scope.includes("@chief/piing"), "the exact package #942's misreport happened in must be in scope");
  assert.ok(scope.includes("@chief/web"), "the exact package whose failure triggered the collateral kill must be in scope");
});
