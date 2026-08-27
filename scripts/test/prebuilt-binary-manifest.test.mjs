// #918: unit coverage for scripts/prebuilt-binary-manifest.mjs.
//
// The property under test is "downloaded CI binaries match the bytes a
// build actually produced", exercised as pure functions against a real temp
// directory tree — no subprocess, no actual cargo build — mirroring
// cargo-target-dir-agreement.test.mjs's shape (`node --test`).

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  buildManifest,
  compareManifest,
  fingerprintBinaries,
  fingerprintBinary,
  manifestPath,
  readManifest,
  writeManifest,
} from "../prebuilt-binary-manifest.mjs";

function withTempDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "prebuilt-binary-manifest-test-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function writeBinary(binaryDir, name, contents) {
  mkdirSync(binaryDir, { recursive: true });
  writeFileSync(join(binaryDir, name), contents);
}

test("fingerprintBinary returns null for a binary that does not exist", () => {
  withTempDir((dir) => {
    assert.equal(fingerprintBinary(join(dir, "chiefd")), null);
  });
});

test("fingerprintBinary is content-addressed: identical bytes -> identical sha256", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "same-bytes");
    const first = fingerprintBinary(join(dir, "chiefd"));
    writeBinary(dir, "chiefd", "same-bytes");
    const second = fingerprintBinary(join(dir, "chiefd"));
    assert.equal(first.sha256, second.sha256);
  });
});

test("fingerprintBinary is content-addressed: different bytes -> different sha256", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "version-a");
    const a = fingerprintBinary(join(dir, "chiefd"));
    writeBinary(dir, "chiefd", "version-b-longer");
    const b = fingerprintBinary(join(dir, "chiefd"));
    assert.notEqual(a.sha256, b.sha256);
  });
});

test("fingerprintBinaries only includes names whose binary actually exists", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "chiefd-bytes");
    const found = fingerprintBinaries(dir, ["chiefd", "beacond"]);
    assert.deepEqual(Object.keys(found), ["chiefd"]);
  });
});

test("writeManifest + readManifest round-trip through the release dir", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "v1");
    const manifest = buildManifest({
      gitSha: "abc123",
      binaries: fingerprintBinaries(dir, ["chiefd"]),
      writtenAtMs: 1000,
    });
    writeManifest(dir, manifest);
    const readBack = readManifest(dir);
    assert.deepEqual(readBack, manifest);
    assert.equal(manifestPath(dir), join(dir, "prebuilt-binary-manifest.json"));
  });
});

test("readManifest returns null (never throws) when no manifest exists", () => {
  withTempDir((dir) => {
    assert.equal(readManifest(dir), null);
  });
});

test("compareManifest fails closed when no manifest was found at all", () => {
  const result = compareManifest(null, { chiefd: { path: "/x", sha256: "a", size: 1 } });
  assert.equal(result.ok, false);
  assert.match(result.reasons[0], /no prebuilt-binary manifest found/);
});

test("compareManifest passes when on-disk bytes match the recorded manifest", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "matching-bytes");
    const current = fingerprintBinaries(dir, ["chiefd"]);
    const manifest = buildManifest({ gitSha: "sha", binaries: current, writtenAtMs: 1 });
    assert.deepEqual(compareManifest(manifest, current), { ok: true });
  });
});

test("compareManifest fails when a recorded binary is now missing", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "bytes");
    const recorded = fingerprintBinaries(dir, ["chiefd"]);
    const manifest = buildManifest({ gitSha: "sha", binaries: recorded, writtenAtMs: 1 });
    const result = compareManifest(manifest, {});
    assert.equal(result.ok, false);
    assert.match(result.reasons[0], /MISSING/);
  });
});

// This is the property #918 exists to catch: the artifact that landed is not
// the one the build job fingerprinted — the "wrong build" instance.
test("compareManifest fails when the on-disk binary's bytes differ from the manifest (wrong artifact)", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "build-A-bytes");
    const recordedA = fingerprintBinaries(dir, ["chiefd"]);
    const manifest = buildManifest({ gitSha: "sha-A", binaries: recordedA, writtenAtMs: 1 });

    writeBinary(dir, "chiefd", "build-B-bytes-entirely-different");
    const currentB = fingerprintBinaries(dir, ["chiefd"]);

    const result = compareManifest(manifest, currentB);
    assert.equal(result.ok, false);
    assert.match(result.reasons[0], /does not match the manifest/);
  });
});

// This is the property #918's design note calls out specifically: the SAME
// path can go stale after the manifest was written (rebuilt in place without
// re-writing the manifest) — not only "wrong path"/"wrong artifact".
test("compareManifest fails when the binary at the SAME path is silently replaced after the manifest was written (stale-after-record)", () => {
  withTempDir((dir) => {
    writeBinary(dir, "chiefd", "original-bytes");
    const recorded = fingerprintBinaries(dir, ["chiefd"]);
    const manifest = buildManifest({ gitSha: "sha", binaries: recorded, writtenAtMs: 1 });

    // Verify against the same tree passes first (restore-to-green arm).
    assert.deepEqual(compareManifest(manifest, fingerprintBinaries(dir, ["chiefd"])), { ok: true });

    // Now silently replace the binary in place, without re-recording.
    writeBinary(dir, "chiefd", "replaced-bytes-same-path");
    const result = compareManifest(manifest, fingerprintBinaries(dir, ["chiefd"]));
    assert.equal(result.ok, false);
    assert.match(result.reasons[0], /does not match the manifest/);
  });
});
