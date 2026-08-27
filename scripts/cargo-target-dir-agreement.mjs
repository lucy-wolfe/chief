// #914: a build and the process consuming its output must agree on
// CARGO_TARGET_DIR, and nothing checked that they did. Five same-day
// incidents (see #914's issue body) came from exactly this: a binary built
// under one resolved target dir, consumed under a different one — stale,
// missing, or from an abandoned SHA — with no signal beyond a confusing red
// or (twice) two agents reporting opposite results for the identical test.
//
// Mirrors cargo-test-floor-lib.mjs's shape: exported pure functions, a thin
// CLI entrypoint at the bottom, unit-tested directly
// (scripts/test/cargo-target-dir-agreement.test.mjs) rather than only
// exercised end-to-end.
//
// THE PROPERTY: a `record` step (run right after a build) fingerprints the
// binaries it just produced and stamps that fingerprint, keyed to the
// resolved target dir, INTO that same directory. A `verify` step (run before
// any consumer trusts a binary) re-resolves CARGO_TARGET_DIR the same way,
// looks for the stamp INSIDE that resolution, and refuses to report a result
// if it is missing or if the binary currently on disk no longer matches what
// was recorded. Because the stamp lives inside the resolved directory itself,
// a consumer that resolves a DIFFERENT directory than the builder did simply
// finds no stamp there — the two questions "did we resolve the same path?"
// and "is the file at that path what the build step made?" collapse into one
// check instead of needing to be asked separately.
//
// Deliberately does NOT try to detect "environment variable said X but really
// meant Y" — there is no such thing. It detects the only thing that matters:
// whether the bytes a consumer is about to trust are the exact bytes a
// preceding `record` call fingerprinted at the location it is looking.

import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const STAMP_FILENAME = ".cargo-target-dir-agreement.json";

/** The binaries this guard knows to fingerprint by default. Callers may pass
 * a narrower or wider list (e.g. a packet that only builds `chiefd`). */
export const DEFAULT_BINARIES = ["chiefd", "beacond"];

/**
 * Resolve `CARGO_TARGET_DIR` exactly the way every other resolver in this
 * repo already does (`tests/e2e/harness/chiefd-binary-path.ts`,
 * `scripts/binary-freshness-gate.sh`, `scripts/release-chiefd.ts`): trim the
 * env var, fall back to `<repoRoot>/apps/chiefd/target` when unset or blank.
 * A SECOND, independent resolver would itself be a place for this exact
 * defect to hide — this one is deliberately the same algorithm as the
 * existing ones, not a new one.
 */
export function resolveTargetDir(env, repoRoot) {
  const override = env.CARGO_TARGET_DIR?.trim();
  return override && override.length > 0 ? override : join(repoRoot, "apps", "chiefd", "target");
}

/**
 * Fingerprint a single debug test binary: sha256 of its bytes, size, and mtime.
 * Returns `null` when the binary does not exist — a missing binary is a
 * legitimate state (nothing built yet) that `record`/`verify` turn into a
 * loud message, not a thrown error here.
 */
export function fingerprintBinary(binaryPath) {
  if (!existsSync(binaryPath)) return null;
  const stat = statSync(binaryPath);
  const bytes = readFileSync(binaryPath);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  return { path: binaryPath, sha256, size: stat.size, mtimeMs: stat.mtimeMs };
}

/** Fingerprint every named binary under `<targetDir>/debug/<name>`. Names
 * with no binary present are simply absent from the returned map, so a
 * partial build (e.g. `--bin chiefd` only) stamps only what it built. */
export function fingerprintBinaries(targetDir, names) {
  const result = {};
  for (const name of names) {
    const fp = fingerprintBinary(join(targetDir, "debug", name));
    if (fp) result[name] = fp;
  }
  return result;
}

/** Build the JSON-serializable stamp a `record` call writes. `stampedAtMs` is
 * passed in rather than read from `Date.now()` so this stays pure and
 * testable with a fixed clock. */
export function buildStamp({ resolvedTargetDir, gitSha, gitDirty, binaries, stampedAtMs }) {
  return { resolvedTargetDir, gitSha, gitDirty, binaries, stampedAtMs };
}

export function stampPath(resolvedTargetDir) {
  return join(resolvedTargetDir, STAMP_FILENAME);
}

export function writeStamp(resolvedTargetDir, stamp) {
  writeFileSync(stampPath(resolvedTargetDir), JSON.stringify(stamp, null, 2) + "\n");
}

/** Returns `null` (never throws) when no stamp exists at this location —
 * "verify found nothing to compare against" is a normal, expected outcome
 * `compareStamp`'s caller turns into a refusal, not a crash. */
export function readStamp(resolvedTargetDir) {
  const path = stampPath(resolvedTargetDir);
  if (!existsSync(path)) return null;
  return JSON.parse(readFileSync(path, "utf8"));
}

/**
 * Compare a previously written stamp against what is on disk RIGHT NOW at
 * the same resolved target dir. Returns `{ ok: true }` or
 * `{ ok: false, reasons: string[] }` — never throws, so a caller can print
 * every disagreement found rather than stopping at the first.
 *
 * Checks, in order of how they were actually hit in #914's incident log:
 *   1. no stamp at all              -> #2: stale/abandoned target dir, or a
 *                                         consumer resolving a dir the build
 *                                         step never touched
 *   2. a binary the stamp names is
 *      now missing                  -> the target dir was cleaned or the
 *                                         binary never finished building
 *   3. a binary's bytes changed     -> #3/#4: rebuilt (by this packet or
 *                                         another) since the last `record`,
 *                                         without a matching re-record —
 *                                         exactly "which build does the
 *                                         consumer think it's testing?"
 */
export function compareStamp(stamp, currentBinaries) {
  if (!stamp) {
    return {
      ok: false,
      reasons: ["no build stamp found at this resolved CARGO_TARGET_DIR — nothing here was `record`ed"],
    };
  }
  const reasons = [];
  for (const [name, recorded] of Object.entries(stamp.binaries)) {
    const current = currentBinaries[name];
    if (!current) {
      reasons.push(`"${name}" was recorded at build time but is now MISSING at ${recorded.path}`);
      continue;
    }
    if (current.sha256 !== recorded.sha256) {
      reasons.push(
        `"${name}" at ${current.path} does not match the recorded build — ` +
          `recorded sha256 ${recorded.sha256.slice(0, 12)}… (${recorded.size}B), ` +
          `found sha256 ${current.sha256.slice(0, 12)}… (${current.size}B). ` +
          "Something rebuilt or replaced this binary since the last `record` without re-stamping."
      );
    }
  }
  return reasons.length > 0 ? { ok: false, reasons } : { ok: true };
}

// CLI: `node cargo-target-dir-agreement.mjs record|verify [--root <repoRoot>] [--binaries a,b]`
if (import.meta.url === `file://${process.argv[1]}`) {
  const { execFileSync } = await import("node:child_process");

  const args = process.argv.slice(2);
  const mode = args[0];
  if (mode !== "record" && mode !== "verify") {
    console.error("usage: cargo-target-dir-agreement.mjs record|verify [--root <repoRoot>] [--binaries a,b]");
    process.exit(2);
  }
  let root = process.cwd();
  let binaryNames = DEFAULT_BINARIES;
  for (let i = 1; i < args.length; i++) {
    if (args[i] === "--root") root = args[++i];
    else if (args[i] === "--binaries") binaryNames = args[++i].split(",").filter(Boolean);
  }

  const resolvedTargetDir = resolveTargetDir(process.env, root);
  const currentBinaries = fingerprintBinaries(resolvedTargetDir, binaryNames);

  console.log(`[cargo-target-dir-agreement] CARGO_TARGET_DIR resolved to: ${resolvedTargetDir}`);
  console.log(
    `[cargo-target-dir-agreement] binaries examined: ${binaryNames.join(", ")} -> found ${
      Object.keys(currentBinaries).length
    }/${binaryNames.length}`
  );

  if (mode === "record") {
    if (Object.keys(currentBinaries).length === 0) {
      console.error(
        `[cargo-target-dir-agreement] REFUSING TO RECORD: no binaries found under ${join(
          resolvedTargetDir,
          "release"
        )} — build first.`
      );
      process.exit(1);
    }
    let gitSha = "unknown";
    let gitDirty = true;
    try {
      gitSha = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
      gitDirty = execFileSync("git", ["-C", root, "status", "--porcelain"], { encoding: "utf8" }).trim().length > 0;
    } catch {
      // Not fatal — the fingerprint comparison in `verify` does not depend on
      // git state; sha/dirty are diagnostic context only.
    }
    const stamp = buildStamp({
      resolvedTargetDir,
      gitSha,
      gitDirty,
      binaries: currentBinaries,
      stampedAtMs: Date.now(),
    });
    writeStamp(resolvedTargetDir, stamp);
    for (const [name, fp] of Object.entries(currentBinaries)) {
      console.log(`[cargo-target-dir-agreement] recorded "${name}": sha256 ${fp.sha256.slice(0, 12)}… (${fp.size}B)`);
    }
    console.log(`[cargo-target-dir-agreement] stamp written to ${stampPath(resolvedTargetDir)}`);
    process.exit(0);
  }

  // verify
  const stamp = readStamp(resolvedTargetDir);
  const result = compareStamp(stamp, currentBinaries);
  if (!result.ok) {
    console.error("[cargo-target-dir-agreement] REFUSING TO REPORT SUCCESS — build/consumer disagreement:");
    for (const reason of result.reasons) console.error(`  - ${reason}`);
    process.exit(1);
  }
  console.log(
    `[cargo-target-dir-agreement] PASS — every examined binary at ${resolvedTargetDir} matches its recorded build ` +
      `(sha ${stamp.gitSha.slice(0, 12)}${stamp.gitDirty ? ", dirty at record time" : ""})`
  );
  process.exit(0);
}
