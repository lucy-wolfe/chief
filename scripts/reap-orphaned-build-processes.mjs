// #987: pregate-orphan-check.mjs REFUSES on a hit and never kills anything —
// correct for a gate about to run, wrong for a host nobody is actively
// gating on right now. This is the other half: an operator-invoked reaper
// for a shared build host that accumulated real orphan debris (an 8-12h
// beacond, a detached gate-matrix.sh, a wedged bare `node --test` holding a
// FakeRpcChild.mjs harness process) between gates rather than during one.
//
// Reuses pregate-orphan-check.mjs's own ORPHAN_PATTERNS/listProcesses so the
// two scripts can never name two different orphan shapes -- the refuse-path
// and the reap-path must agree on what counts as debris, or a process the
// refuser would block could still survive a "clean" sweep.
//
// AGE-GATED, not identity-gated: a process matching an ORPHAN_PATTERN is
// only a reap candidate once it has been alive longer than --min-age-seconds
// (default 2h, matching scripts/ci-shard.ts's own STALE_DATA_ROOT_AGE_MS
// convention for "well beyond any single shard's own budget"). This is the
// weakest of the issue's three ranked options on its own account -- an age
// threshold is a guess that will be wrong for a legitimately long run -- so
// it defaults to --dry-run (report only) and requires --kill explicitly to
// send a signal. Never SIGKILL directly: SIGTERM first, so a process with
// its own cleanup (chiefd's parent-death watchdog, a daemon's own shutdown
// path) gets the chance to take it.
//
// UNEXERCISED: written under the no-builds/no-tests directive. Never run
// against a live process table, orphaned or clean.

import { execFileSync } from "node:child_process";
import { ORPHAN_PATTERNS, listProcesses } from "./pregate-orphan-check.mjs";

export const DEFAULT_MIN_AGE_SECONDS = 2 * 60 * 60; // 2h, matching ci-shard.ts's STALE_DATA_ROOT_AGE_MS

/** Process start time in epoch seconds, via `ps -o lstart=` (portable across
 * the Linux/macOS `ps` variants this repo already has to support). Returns
 * null if the pid cannot be inspected (already gone, permission denied) --
 * the caller treats null as "cannot prove it's old enough", never as a hit. */
export function processStartEpochSeconds(pid, psRunner = defaultPsLstart) {
  const raw = psRunner(pid);
  if (!raw) return null;
  const parsed = Date.parse(raw.trim());
  if (Number.isNaN(parsed)) return null;
  return Math.floor(parsed / 1000);
}

function defaultPsLstart(pid) {
  try {
    return execFileSync("ps", ["-o", "lstart=", "-p", String(pid)], { encoding: "utf8" });
  } catch {
    return null;
  }
}

/** Every orphan-pattern-matching process (via pregate-orphan-check.mjs's own
 * findOrphanCandidates-equivalent match, inlined here rather than imported
 * so age-filtering can happen in one pass) whose start time is older than
 * `minAgeSeconds`, given the current time and a start-time lookup. */
export function findReapCandidates(processes, nowEpochSeconds, minAgeSeconds, startTimeOf = processStartEpochSeconds) {
  const candidates = [];
  for (const proc of processes) {
    const matched = ORPHAN_PATTERNS.filter((pattern) => pattern.re.test(proc.cmd));
    if (matched.length === 0) continue;
    const startedAt = startTimeOf(proc.pid);
    if (startedAt === null) continue; // cannot prove age -- not a reap candidate, only a refuse-path hit
    const ageSeconds = nowEpochSeconds - startedAt;
    if (ageSeconds < minAgeSeconds) continue;
    candidates.push({ ...proc, patterns: matched.map((p) => p.name), ageSeconds });
  }
  return candidates;
}

function parseArgs(argv) {
  const args = { dryRun: true, minAgeSeconds: DEFAULT_MIN_AGE_SECONDS };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--kill") args.dryRun = false;
    else if (argv[i] === "--min-age-seconds") args.minAgeSeconds = Number(argv[++i]);
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const psOutput = execFileSync("ps", ["-eo", "pid=,ppid=,args="], { encoding: "utf8" });
  const processes = listProcesses(psOutput);
  const now = Math.floor(Date.now() / 1000);
  const candidates = findReapCandidates(processes, now, args.minAgeSeconds);

  if (candidates.length === 0) {
    console.log(`[reap-orphaned-build-processes] no orphan-shaped process older than ${args.minAgeSeconds}s found.`);
    process.exit(0);
  }

  for (const c of candidates) {
    console.log(
      `[reap-orphaned-build-processes] pid=${c.pid} ppid=${c.ppid} age=${c.ageSeconds}s [${c.patterns.join(", ")}]: ${c.cmd}`,
    );
  }

  if (args.dryRun) {
    console.log(`[reap-orphaned-build-processes] DRY RUN: ${candidates.length} candidate(s) found, none signalled. Re-run with --kill to reap.`);
    process.exit(0);
  }

  for (const c of candidates) {
    try {
      process.kill(c.pid, "SIGTERM");
      console.log(`[reap-orphaned-build-processes] SIGTERM sent to pid=${c.pid}`);
    } catch (error) {
      console.log(`[reap-orphaned-build-processes] could not signal pid=${c.pid}: ${String(error)}`);
    }
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
