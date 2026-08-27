// #918: the CI e2e `CHIEFD_PREBUILT_BINARY` path has a build and a consumer
// with no shared filesystem between them — the build runs in `build-chiefd`,
// one job/runner, and the CI binary set travels to a different job/runner
// only via `actions/upload-artifact` / `actions/download-artifact`, which
// preserves neither a directory a stamp could live in (#914's mechanism)
// nor binary mtimes. This is the same defect class as #914 in its strongest
// form: nothing asserts the bytes a consumer is about to trust are the
// bytes a build actually produced.
//
// THE PROPERTY: a `write` step (run right after the CI debug build, same job
// as the upload) fingerprints the binaries and writes a
// sidecar manifest NEXT TO them, so it travels inside the same artifact
// upload. A `verify` step (run by the consumer, after the existing mtime
// freshness gate) recomputes the on-disk binaries' hashes and compares
// them against the manifest, refusing to report a result if the manifest
// is missing or the binary no longer matches what was recorded.
//
// Deliberately additive to, not a replacement for,
// `scripts/binary-freshness-gate.sh` — that script's mtime-ordering check
// stays exactly as it is (see the design record
// "Out of scope"); this manifest is a second, independent signal consulted
// only after the mtime gate already passed.
//
// NOTE on #914: `scripts/cargo-target-dir-agreement.mjs` fingerprints a
// binary the same way (sha256 of its bytes). This packet's base branch
// predates #914 landing on canonical, so importing its `fingerprintBinary`
// is not possible here — this file ships its own minimal hash helper below,
// deliberately small and commented as a duplicate to collapse once #914 is
// on canonical (flagged to the architect in the REVIEW REQUEST, not silently
// left as permanent duplication).

import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const MANIFEST_FILENAME = "prebuilt-binary-manifest.json";

/** The binaries this guard knows to fingerprint by default. */
// The binary pair became a triple in P6: `chief` (the operator
// client) and `chiefd` (the backend) are two binaries now, and an e2e
// consumer that downloaded only the client would fail the first time it
// started a company — the precise class of "the artifact travelled but the
// wrong one" this manifest exists to catch.
export const DEFAULT_BINARIES = ["chief", "chiefd", "beacond"];

/**
 * sha256 of a binary's bytes, plus its size. Returns `null` when the binary
 * does not exist — a missing binary is a legitimate state (nothing built,
 * or a partial `--bin` build) that `write`/`verify` turn into a loud
 * message, not a thrown error here.
 */
export function fingerprintBinary(binaryPath) {
  if (!existsSync(binaryPath)) return null;
  const stat = statSync(binaryPath);
  const bytes = readFileSync(binaryPath);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  return { path: binaryPath, sha256, size: stat.size };
}

/** Fingerprint every named binary under `<binaryDir>/<name>`. Names with no
 * binary present are simply absent from the returned map. */
export function fingerprintBinaries(binaryDir, names) {
  const result = {};
  for (const name of names) {
    const fp = fingerprintBinary(join(binaryDir, name));
    if (fp) result[name] = fp;
  }
  return result;
}

export function manifestPath(binaryDir) {
  return join(binaryDir, MANIFEST_FILENAME);
}

/** Build the JSON-serializable manifest a `write` call writes. `writtenAtMs`
 * is passed in rather than read from `Date.now()` so this stays pure and
 * testable with a fixed clock. */
export function buildManifest({ gitSha, binaries, writtenAtMs }) {
  return { gitSha, binaries, writtenAtMs };
}

export function writeManifest(binaryDir, manifest) {
  writeFileSync(manifestPath(binaryDir), JSON.stringify(manifest, null, 2) + "\n");
}

/** Returns `null` (never throws) when no manifest exists at this location —
 * a normal, expected state a caller turns into a refusal or a deliberate
 * carve-out, not a crash. */
export function readManifest(binaryDir) {
  const path = manifestPath(binaryDir);
  if (!existsSync(path)) return null;
  return JSON.parse(readFileSync(path, "utf8"));
}

/**
 * Compare a manifest against what is on disk RIGHT NOW in the same binary
 * directory. Returns `{ ok: true }` or `{ ok: false, reasons: string[] }` —
 * never throws, so a caller can print every disagreement found rather than
 * stopping at the first.
 */
export function compareManifest(manifest, currentBinaries) {
  if (!manifest) {
    return {
      ok: false,
      reasons: ["no prebuilt-binary manifest found next to the downloaded CI binaries"],
    };
  }
  const reasons = [];
  for (const [name, recorded] of Object.entries(manifest.binaries)) {
    const current = currentBinaries[name];
    if (!current) {
      reasons.push(`"${name}" was recorded in the manifest but is now MISSING at ${recorded.path}`);
      continue;
    }
    if (current.sha256 !== recorded.sha256) {
      reasons.push(
        `"${name}" at ${current.path} does not match the manifest — ` +
          `recorded sha256 ${recorded.sha256.slice(0, 12)}… (${recorded.size}B), ` +
          `found sha256 ${current.sha256.slice(0, 12)}… (${current.size}B). ` +
          "The binary changed since the manifest was written without a matching re-write."
      );
    }
  }
  return reasons.length > 0 ? { ok: false, reasons } : { ok: true };
}

// CLI: `node prebuilt-binary-manifest.mjs write|verify --binary-dir <dir> [--binaries a,b] [--root <repoRoot>]`
if (import.meta.url === `file://${process.argv[1]}`) {
  const { execFileSync } = await import("node:child_process");

  const args = process.argv.slice(2);
  const mode = args[0];
  if (mode !== "write" && mode !== "verify") {
    console.error("usage: prebuilt-binary-manifest.mjs write|verify --binary-dir <dir> [--binaries a,b] [--root <repoRoot>]");
    process.exit(2);
  }
  let binaryDir;
  let root = process.cwd();
  let binaryNames = DEFAULT_BINARIES;
  for (let i = 1; i < args.length; i++) {
    if (args[i] === "--binary-dir") binaryDir = args[++i];
    else if (args[i] === "--root") root = args[++i];
    else if (args[i] === "--binaries") binaryNames = args[++i].split(",").filter(Boolean);
  }
  if (!binaryDir) {
    console.error("prebuilt-binary-manifest.mjs: --binary-dir is required");
    process.exit(2);
  }

  const currentBinaries = fingerprintBinaries(binaryDir, binaryNames);

  console.log(`[prebuilt-binary-manifest] binary dir: ${binaryDir}`);
  console.log(
    `[prebuilt-binary-manifest] binaries examined: ${binaryNames.join(", ")} -> found ${
      Object.keys(currentBinaries).length
    }/${binaryNames.length}`
  );

  if (mode === "write") {
    if (Object.keys(currentBinaries).length === 0) {
      console.error(`[prebuilt-binary-manifest] REFUSING TO WRITE: no binaries found under ${binaryDir} — build first.`);
      process.exit(1);
    }
    let gitSha = "unknown";
    try {
      gitSha = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    } catch {
      // Not fatal — the fingerprint comparison in `verify` does not depend
      // on git state; gitSha is diagnostic context only.
    }
    const manifest = buildManifest({ gitSha, binaries: currentBinaries, writtenAtMs: Date.now() });
    writeManifest(binaryDir, manifest);
    console.log(`[prebuilt-binary-manifest] wrote ${manifestPath(binaryDir)} for gitSha=${gitSha}`);
    process.exit(0);
  }

  // verify
  const manifest = readManifest(binaryDir);
  const result = compareManifest(manifest, currentBinaries);
  if (!result.ok) {
    console.error("[prebuilt-binary-manifest] VERIFY FAILED:");
    for (const reason of result.reasons) console.error(`  - ${reason}`);
    process.exit(1);
  }
  console.log("[prebuilt-binary-manifest] VERIFY PASSED — binaries match the recorded manifest");
  process.exit(0);
}
