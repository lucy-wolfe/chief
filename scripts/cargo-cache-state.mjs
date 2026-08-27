// #941: a shared, persistent CARGO_TARGET_DIR means a cargo leg may or may
// not have compiled anything on any given run, and nothing said which. That
// is a green whose meaning depends on invisible state — exactly the failure
// class turbo's own `0 cached, 11 total` line exists to rule out for the TS
// side. This is that same line for cargo.
//
// THE PROPERTY: `build` runs a cargo command with `--message-format=json`,
// reads every `compiler-artifact` message's `fresh` field (cargo's own
// up-to-date-check result — true when a crate's fingerprint matched and it
// was NOT recompiled, false when it was actually built), and turns that into
// a human line plus a machine-readable stamp written into the resolved
// target dir. `assert` re-reads that stamp and refuses (fail-closed, no
// warning) unless it exists and was written at or after a caller-supplied
// `--since` timestamp — i.e. genuinely produced by THIS gate run, not left
// over from a previous one that happened to share the same shared dir.
//
// Mirrors cargo-target-dir-agreement.mjs's shape deliberately: exported pure
// functions, a thin CLI entrypoint, unit-tested directly rather than only
// exercised end-to-end.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const STAMP_FILENAME = ".cargo-cache-state.json";

export function stampPath(resolvedTargetDir) {
  return join(resolvedTargetDir, STAMP_FILENAME);
}

/** Parse cargo's `--message-format=json` stdout into `{compiled, fresh, total}`
 * counts over `compiler-artifact` messages. Ignores every other message
 * reason (compiler-message, build-script-executed, build-finished, …) —
 * those are not the question this line answers. Malformed/non-JSON lines are
 * skipped rather than thrown on: a mixed stream (e.g. a stray non-JSON line
 * from a build script) should not crash the summary, only leave that line
 * uncounted. */
export function summarizeCacheState(jsonLinesText) {
  let compiled = 0;
  let fresh = 0;
  for (const line of jsonLinesText.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    let msg;
    try {
      msg = JSON.parse(trimmed);
    } catch {
      continue;
    }
    if (msg.reason !== "compiler-artifact") continue;
    if (msg.fresh === true) fresh++;
    else if (msg.fresh === false) compiled++;
  }
  return { compiled, fresh, total: compiled + fresh };
}

export function formatCacheStateLine(summary, resolvedTargetDir) {
  return (
    `[cargo-cache-state] ${resolvedTargetDir}: ` +
    `${summary.compiled} compiled, ${summary.fresh} cached, ${summary.total} total`
  );
}

export function buildStamp({ summary, resolvedTargetDir, gitSha, stampedAtMs }) {
  return { summary, resolvedTargetDir, gitSha, stampedAtMs };
}

export function writeStamp(resolvedTargetDir, stamp) {
  writeFileSync(stampPath(resolvedTargetDir), JSON.stringify(stamp, null, 2) + "\n");
}

/** Returns `null` (never throws) when no stamp exists — "nothing to assert
 * against" is a normal, expected outcome the CLI turns into a refusal. */
export function readStamp(resolvedTargetDir) {
  const path = stampPath(resolvedTargetDir);
  if (!existsSync(path)) return null;
  return JSON.parse(readFileSync(path, "utf8"));
}

/**
 * `sinceMs`: the stamp must have been written at or after this instant to
 * count as "emitted by this run". Without this, a stamp left over from a
 * PRIOR gate on the same shared dir would satisfy "a stamp exists" forever,
 * which defeats the entire point — the line must be about THIS run's build,
 * not merely present.
 */
export function assertCacheStateEmitted(stamp, sinceMs) {
  if (!stamp) {
    return {
      ok: false,
      reason: "no cache-state stamp found at this resolved CARGO_TARGET_DIR — the build step never ran or never emitted one",
    };
  }
  if (typeof sinceMs === "number" && stamp.stampedAtMs < sinceMs) {
    return {
      ok: false,
      reason:
        `cache-state stamp is stale — stamped at ${stamp.stampedAtMs}, but this gate run started at ${sinceMs}. ` +
        "A leftover stamp from an earlier run on this shared dir does not prove THIS build emitted one.",
    };
  }
  return { ok: true };
}

// CLI
if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  const mode = args[0];

  if (mode === "build") {
    // `node cargo-cache-state.mjs build --root <repoRoot> -- build --release ...`
    // Everything after `--` is passed to the `cargo` binary as its argv —
    // do NOT repeat the leading `cargo` there, it is already the spawned
    // command (i.e. pass `build --release ...`, not `cargo build --release ...`).
    const dashIdx = args.indexOf("--");
    if (dashIdx === -1) {
      console.error(
        "usage: cargo-cache-state.mjs build --root <repoRoot> -- <cargo subcommand + args, e.g. build --release ...>"
      );
      process.exit(2);
    }
    let root = process.cwd();
    for (let i = 1; i < dashIdx; i++) {
      if (args[i] === "--root") root = args[++i];
    }
    const cargoArgs = args.slice(dashIdx + 1);
    const resolvedTargetDir = (process.env.CARGO_TARGET_DIR?.trim() || join(root, "apps", "chiefd", "target"));

    const full = [...cargoArgs, "--message-format=json-render-diagnostics"];
    const result = spawnSync("cargo", full, { encoding: "utf8", maxBuffer: 1024 * 1024 * 256 });
    // Surface cargo's own diagnostics/errors as a human would see them —
    // json-render-diagnostics keeps compiler-message payloads human-readable
    // even though the outer stream is JSON lines.
    for (const line of (result.stdout || "").split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        const msg = JSON.parse(trimmed);
        if (msg.reason === "compiler-message" && msg.message?.rendered) {
          process.stderr.write(msg.message.rendered);
        }
      } catch {
        // non-JSON line on stdout; ignore, mirrors summarizeCacheState
      }
    }
    if (result.stderr) process.stderr.write(result.stderr);

    const summary = summarizeCacheState(result.stdout || "");
    console.log(formatCacheStateLine(summary, resolvedTargetDir));

    // A failed cargo invocation must NOT leave a stamp behind. Writing one
    // here (even an honest "0 compiled, 0 cached") would let a later
    // `assert` report OK about a build that never actually produced
    // anything — the exact "green whose meaning depends on invisible state"
    // this tool exists to rule out. Fail closed: no stamp, no successor step
    // trusts this target dir.
    if (result.status !== 0) {
      console.error(
        `[cargo-cache-state] cargo exited ${result.status} — refusing to stamp a cache-state claim for a failed build.`
      );
      process.exit(result.status ?? 1);
    }

    let gitSha = "unknown";
    try {
      gitSha = spawnSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).stdout.trim() || "unknown";
    } catch {
      // diagnostic only
    }
    writeStamp(
      resolvedTargetDir,
      buildStamp({ summary, resolvedTargetDir, gitSha, stampedAtMs: Date.now() })
    );

    process.exit(0);
  }

  if (mode === "assert") {
    // `node cargo-cache-state.mjs assert --root <repoRoot> --since <ms>`
    let root = process.cwd();
    let since;
    for (let i = 1; i < args.length; i++) {
      if (args[i] === "--root") root = args[++i];
      else if (args[i] === "--since") since = Number(args[++i]);
    }
    const resolvedTargetDir = (process.env.CARGO_TARGET_DIR?.trim() || join(root, "apps", "chiefd", "target"));
    const stamp = readStamp(resolvedTargetDir);
    const result = assertCacheStateEmitted(stamp, since);
    if (!result.ok) {
      console.error(`[cargo-cache-state] REFUSING TO GATE: ${result.reason}`);
      process.exit(1);
    }
    console.log(`[cargo-cache-state] OK — ${formatCacheStateLine(stamp.summary, resolvedTargetDir)} (sha ${stamp.gitSha.slice(0, 12)})`);
    process.exit(0);
  }

  console.error("usage: cargo-cache-state.mjs build --root <repoRoot> -- <cargo build args...>");
  console.error("       cargo-cache-state.mjs assert --root <repoRoot> --since <ms>");
  process.exit(2);
}
