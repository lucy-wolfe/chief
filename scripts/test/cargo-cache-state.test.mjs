// #941: unit coverage for scripts/cargo-cache-state.mjs.
//
// The property under test is "a cargo leg that may or may not have compiled
// anything says which, and a caller can tell whether that claim is fresh or
// stale", exercised as pure functions against synthetic cargo JSON-lines
// output and a real temp directory — no subprocess, no actual cargo build —
// mirroring cargo-target-dir-agreement.test.mjs's shape.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import {
  assertCacheStateEmitted,
  buildStamp,
  formatCacheStateLine,
  readStamp,
  stampPath,
  summarizeCacheState,
  writeStamp,
} from "../cargo-cache-state.mjs";

const CLI_PATH = fileURLToPath(new URL("../cargo-cache-state.mjs", import.meta.url));

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "cargo-cache-state-test-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function artifactLine(fresh) {
  return JSON.stringify({ reason: "compiler-artifact", fresh, package_id: "pkg 1.0.0" });
}

test("summarizeCacheState counts fresh:false as compiled, fresh:true as cached", () => {
  const text = [artifactLine(false), artifactLine(true), artifactLine(true)].join("\n");
  assert.deepEqual(summarizeCacheState(text), { compiled: 1, fresh: 2, total: 3 });
});

test("summarizeCacheState ignores non-compiler-artifact messages", () => {
  const text = [
    JSON.stringify({ reason: "build-script-executed" }),
    artifactLine(false),
    JSON.stringify({ reason: "build-finished", success: true }),
  ].join("\n");
  assert.deepEqual(summarizeCacheState(text), { compiled: 1, fresh: 0, total: 1 });
});

test("summarizeCacheState skips malformed/non-JSON lines rather than throwing", () => {
  const text = ["not json at all", artifactLine(true), "", "   "].join("\n");
  assert.deepEqual(summarizeCacheState(text), { compiled: 0, fresh: 1, total: 1 });
});

test("summarizeCacheState on a fully cold build: every artifact fresh:false", () => {
  const text = Array.from({ length: 169 }, () => artifactLine(false)).join("\n");
  assert.deepEqual(summarizeCacheState(text), { compiled: 169, fresh: 0, total: 169 });
});

test("summarizeCacheState on a fully warm rebuild: every artifact fresh:true", () => {
  const text = Array.from({ length: 169 }, () => artifactLine(true)).join("\n");
  assert.deepEqual(summarizeCacheState(text), { compiled: 0, fresh: 169, total: 169 });
});

test("formatCacheStateLine renders compiled/cached/total and the target dir", () => {
  const line = formatCacheStateLine({ compiled: 3, fresh: 166, total: 169 }, "/shared/target");
  assert.match(line, /^\[cargo-cache-state\] \/shared\/target: 3 compiled, 166 cached, 169 total$/);
});

test("assertCacheStateEmitted refuses when no stamp exists", () => {
  const result = assertCacheStateEmitted(null, 1000);
  assert.equal(result.ok, false);
  assert.match(result.reason, /no cache-state stamp found/);
});

test("assertCacheStateEmitted refuses a stamp older than the gate run's start", () => {
  const stamp = buildStamp({
    summary: { compiled: 0, fresh: 169, total: 169 },
    resolvedTargetDir: "/shared/target",
    gitSha: "abc123",
    stampedAtMs: 500,
  });
  const result = assertCacheStateEmitted(stamp, 1000);
  assert.equal(result.ok, false);
  assert.match(result.reason, /stale/);
});

test("assertCacheStateEmitted passes a stamp written at or after the gate run's start", () => {
  const stamp = buildStamp({
    summary: { compiled: 5, fresh: 164, total: 169 },
    resolvedTargetDir: "/shared/target",
    gitSha: "abc123",
    stampedAtMs: 1500,
  });
  assert.equal(assertCacheStateEmitted(stamp, 1000).ok, true);
  assert.equal(assertCacheStateEmitted(stamp, 1500).ok, true);
});

test("assertCacheStateEmitted passes with no --since constraint (undefined)", () => {
  const stamp = buildStamp({
    summary: { compiled: 0, fresh: 1, total: 1 },
    resolvedTargetDir: "/shared/target",
    gitSha: "abc123",
    stampedAtMs: 42,
  });
  assert.equal(assertCacheStateEmitted(stamp, undefined).ok, true);
});

test("CLI `build` refuses to stamp when cargo itself fails (fail-closed, not an honest 0/0/0)", () => {
  withTempDir((dir) => {
    const targetDir = join(dir, "target");
    // A fake `cargo` on PATH that always fails, mimicking "no such command"
    // or any other total build failure with zero compiler-artifact messages.
    const binDir = join(dir, "fakebin");
    mkdirSync(binDir, { recursive: true });
    const fakeCargo = join(binDir, "cargo");
    writeFileSync(fakeCargo, "#!/bin/sh\necho 'error: no such command' >&2\nexit 101\n");
    chmodSync(fakeCargo, 0o755);

    const result = spawnSync(
      process.execPath,
      [CLI_PATH, "build", "--root", dir, "--", "build", "--release"],
      {
        encoding: "utf8",
        env: { ...process.env, PATH: `${binDir}:${process.env.PATH}`, CARGO_TARGET_DIR: targetDir },
      }
    );

    assert.equal(result.status, 101, "must propagate cargo's own exit code, not swallow it");
    assert.equal(readStamp(targetDir), null, "a failed cargo run must leave no stamp for `assert` to trust");
  });
});

test("writeStamp/readStamp round-trip through a real temp directory", () => {
  withTempDir((dir) => {
    assert.equal(readStamp(dir), null);
    const stamp = buildStamp({
      summary: { compiled: 2, fresh: 3, total: 5 },
      resolvedTargetDir: dir,
      gitSha: "deadbeef",
      stampedAtMs: 12345,
    });
    writeStamp(dir, stamp);
    assert.deepEqual(readStamp(dir), stamp);
    assert.equal(stampPath(dir), join(dir, ".cargo-cache-state.json"));
  });
});
