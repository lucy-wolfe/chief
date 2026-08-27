// #914: unit coverage for scripts/cargo-target-dir-agreement.mjs.
//
// The property under test is "a build and its consumer agree on
// CARGO_TARGET_DIR", exercised as pure functions against a real temp
// directory tree — no subprocess, no actual cargo build — mirroring
// cargo-test-floor.test.mjs's shape (`node --test`).

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  buildStamp,
  compareStamp,
  fingerprintBinaries,
  fingerprintBinary,
  readStamp,
  resolveTargetDir,
  stampPath,
  writeStamp,
} from "../cargo-target-dir-agreement.mjs";

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "cargo-target-dir-agreement-test-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function writeBinary(targetDir, name, contents) {
  const debugDir = join(targetDir, "debug");
  mkdirSync(debugDir, { recursive: true });
  writeFileSync(join(debugDir, name), contents);
}

test("resolveTargetDir honors CARGO_TARGET_DIR verbatim when set (trimmed)", () => {
  assert.equal(resolveTargetDir({ CARGO_TARGET_DIR: "  /custom/target  " }, "/repo"), "/custom/target");
});

test("resolveTargetDir falls back to <repoRoot>/apps/chiefd/target when unset", () => {
  assert.equal(resolveTargetDir({}, "/repo"), join("/repo", "apps", "chiefd", "target"));
});

test("resolveTargetDir falls back the same way when CARGO_TARGET_DIR is blank", () => {
  assert.equal(resolveTargetDir({ CARGO_TARGET_DIR: "   " }, "/repo"), join("/repo", "apps", "chiefd", "target"));
});

test("fingerprintBinary returns null for a binary that does not exist", () => {
  withTempDir((dir) => {
    assert.equal(fingerprintBinary(join(dir, "debug", "chiefd")), null);
  });
});

test("fingerprintBinary is content-addressed: identical bytes -> identical sha256", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "same-bytes");
    const first = fingerprintBinary(join(dir, "debug", "chiefd"));
    writeBinary(dir, "chiefd", "same-bytes");
    const second = fingerprintBinary(join(dir, "debug", "chiefd"));
    assert.equal(first.sha256, second.sha256);
  });
});

test("fingerprintBinaries only includes names whose binary actually exists", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "chiefd-bytes");
    const found = fingerprintBinaries(dir, ["chiefd", "beacond"]);
    assert.deepEqual(Object.keys(found), ["chiefd"]);
  });
});

test("writeStamp + readStamp round-trip through the resolved target dir", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "v1");
    const stamp = buildStamp({
      resolvedTargetDir: dir,
      gitSha: "abc123",
      gitDirty: false,
      binaries: fingerprintBinaries(dir, ["chiefd"]),
      stampedAtMs: 0,
    });
    writeStamp(dir, stamp);
    const read = readStamp(dir);
    assert.deepEqual(read, stamp);
  });
});

test("readStamp returns null when no stamp was ever written at this location — the #2 stale/abandoned-dir case", () => {
  withTempDir((dir) => {
    assert.equal(readStamp(dir), null);
    assert.equal(compareStamp(readStamp(dir), fingerprintBinaries(dir, ["chiefd"])).ok, false);
  });
});

test("compareStamp: FAILS CLOSED when no stamp exists — refuses rather than silently passing", () => {
  const result = compareStamp(null, {});
  assert.equal(result.ok, false);
  assert.match(result.reasons[0], /no build stamp found/);
});

// This is the direct #914 regression case: a build step recorded one set of
// bytes, and by the time `verify` runs, the binary on disk at the SAME
// resolved path is different — exactly what happens when a stale target dir
// is reused across SHAs (incident #2) or two lanes race a rebuild (#3/#4).
test("compareStamp: REFUSES when a binary's bytes changed since record — the exact #914 regression", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "build-A-bytes");
    const stamp = buildStamp({
      resolvedTargetDir: dir,
      gitSha: "sha-A",
      gitDirty: false,
      binaries: fingerprintBinaries(dir, ["chiefd"]),
      stampedAtMs: 0,
    });
    writeStamp(dir, stamp);

    // A different build silently replaces the binary without re-recording —
    // the exact "which build is the consumer about to trust?" hazard.
    writeBinary(dir, "chiefd", "build-B-bytes-totally-different-and-longer");

    const result = compareStamp(readStamp(dir), fingerprintBinaries(dir, ["chiefd"]));
    assert.equal(result.ok, false);
    assert.match(result.reasons[0], /does not match the recorded build/);
  });
});

test("compareStamp: REFUSES when a recorded binary is now missing", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "build-A-bytes");
    const stamp = buildStamp({
      resolvedTargetDir: dir,
      gitSha: "sha-A",
      gitDirty: false,
      binaries: fingerprintBinaries(dir, ["chiefd"]),
      stampedAtMs: 0,
    });
    writeStamp(dir, stamp);

    rmSync(join(dir, "debug", "chiefd"));

    const result = compareStamp(readStamp(dir), fingerprintBinaries(dir, ["chiefd"]));
    assert.equal(result.ok, false);
    assert.match(result.reasons[0], /MISSING/);
  });
});

// The positive control: it must be able to say YES, not just refuse
// everything. A guard only ever observed failing is as much a claim as one
// only ever observed passing.
test("compareStamp: PASSES when the binary on disk is byte-identical to what was recorded", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "stable-bytes");
    const stamp = buildStamp({
      resolvedTargetDir: dir,
      gitSha: "sha-A",
      gitDirty: false,
      binaries: fingerprintBinaries(dir, ["chiefd"]),
      stampedAtMs: 0,
    });
    writeStamp(dir, stamp);

    const result = compareStamp(readStamp(dir), fingerprintBinaries(dir, ["chiefd"]));
    assert.deepEqual(result, { ok: true });
  });
});

// This is the property from #914's issue body directly: "a consumer that
// resolves a DIFFERENT directory than the builder did" — modeled here as two
// separate temp directories standing in for two different CARGO_TARGET_DIR
// resolutions. The consuming side must never fall back to reading the
// builder's stamp; it can only see what is at ITS OWN resolved location.
test("two different resolved target dirs never see each other's stamp", () => {
  withTempDir((buildDir) => {
    withTempDir((consumeDir) => {
      writeBinary(buildDir, "chiefd", "built-here");
      const stamp = buildStamp({
        resolvedTargetDir: buildDir,
        gitSha: "sha-A",
        gitDirty: false,
        binaries: fingerprintBinaries(buildDir, ["chiefd"]),
        stampedAtMs: 0,
      });
      writeStamp(buildDir, stamp);

      // The "consumer" resolved a completely different directory (e.g. a
      // fresh shell with CARGO_TARGET_DIR unset or pointed elsewhere).
      assert.equal(readStamp(consumeDir), null);
      const result = compareStamp(readStamp(consumeDir), fingerprintBinaries(consumeDir, ["chiefd"]));
      assert.equal(result.ok, false);
    });
  });
});

test("stampPath is inside the resolved target dir, not a shared/global location", () => {
  assert.equal(stampPath("/some/target"), join("/some/target", ".cargo-target-dir-agreement.json"));
});
