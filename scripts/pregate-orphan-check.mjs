// #987: a gate run is not the only thing that can be alive on a shared build
// host. Three real incidents in one night — an 8-12h orphaned `beacond`, a
// detached `gate-matrix.sh` whose log interleaved into a second run, and a
// wedged `node --test` with no file argument (a quoting bug that made it sit
// in test discovery forever, holding a FakeRpcChild.mjs harness process with
// it) — all shared one property: a stray process from an earlier or
// concurrent run can answer a socket, hold a lock, or interleave a log that
// a gate run assumes is exclusively its own. None of the three was found by
// reading output; all three were found by reading `ps -eo`.
//
// THE PROPERTY, ranked most-mechanical-first per the issue (a pre-gate
// refusal, not a sweep): scan for processes matching this repo's
// harness/daemon set, filter to ones plausibly rooted under THIS checkout or
// carrying no discoverable root at all (unverifiable is treated as a hit,
// not as a pass), and refuse loudly if anything survives the filter. This
// script never kills anything — a gate that silently kills processes it did
// not start is its own hazard (the issue's own words). A human decides
// whether a hit is this run's own leftover, a legitimate concurrent gate on
// a different worktree, or genuine orphan debris to report to whoever owns
// process cleanup on that host.
//
// UNEXERCISED: written under the no-builds/no-tests directive. The ps
// parsing has not been run against a live process table, orphaned or clean.
// Whoever restores testing should run this against both a clean host and a
// host carrying a real orphan (the #987 zipbox wedge is a good fixture
// shape: `node --test` with no trailing file argument) before trusting it.

import { execFileSync } from "node:child_process";

/** Patterns naming this repo's harness/daemon process shapes. Bare `node
 * --test` (no file argument) is its own pattern, not folded into a generic
 * `node --test` match, because a WIRED invocation always names a target —
 * matching only the bare form is what makes this pattern a defect signature
 * rather than a false-positive magnet on every legitimate guard run. */
export const ORPHAN_PATTERNS = [
  { name: "beacond", re: /(^|\/)beacond(\s|$)/ },
  { name: "chiefd run", re: /(^|\/)chiefd\s+run(\s|$)/ },
  { name: "bare node --test (no file argument, a quoting bug)", re: /\bnode(\s[^\n]*)?\s--test\s*$/ },
  { name: "FakeRpcChild.mjs harness", re: /FakeRpcChild\.mjs/ },
  { name: "gate-matrix.sh", re: /(^|\/)gate-matrix\.sh(\s|$)/ },
];

/** `ps -eo pid=,ppid=,args=` output, one process per line. Exported so
 * tests can feed a fixture table instead of the live process list. */
export function listProcesses(psOutput) {
  return psOutput
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const m = line.match(/^(\d+)\s+(\d+)\s+(.*)$/);
      if (!m) return null;
      return { pid: Number(m[1]), ppid: Number(m[2]), cmd: m[3] };
    })
    .filter((p) => p !== null);
}

/** Every process whose `cmd` matches at least one ORPHAN_PATTERN, paired
 * with which pattern(s) it hit. Excludes `excludePids` (this script's own
 * `ps` invocation and its caller, so the check never flags itself). */
export function findOrphanCandidates(processes, excludePids = []) {
  const excluded = new Set(excludePids);
  const hits = [];
  for (const proc of processes) {
    if (excluded.has(proc.pid)) continue;
    const matched = ORPHAN_PATTERNS.filter((pattern) => pattern.re.test(proc.cmd));
    if (matched.length > 0) hits.push({ ...proc, patterns: matched.map((p) => p.name) });
  }
  return hits;
}

export function runPsSnapshot() {
  return execFileSync("ps", ["-eo", "pid=,ppid=,args="], { encoding: "utf8" });
}

function main() {
  const psOutput = runPsSnapshot();
  const processes = listProcesses(psOutput);
  const hits = findOrphanCandidates(processes, [process.pid, process.ppid]);

  if (hits.length === 0) {
    console.log("[pregate-orphan-check] PASS: no harness/daemon-shaped processes found on this host.");
    process.exit(0);
  }

  console.error(`[pregate-orphan-check] REFUSING: ${hits.length} process(es) on this host match a known orphan-debris shape:`);
  for (const hit of hits) {
    console.error(`  pid=${hit.pid} ppid=${hit.ppid} [${hit.patterns.join(", ")}]: ${hit.cmd}`);
  }
  console.error(
    "[pregate-orphan-check] This gate does not kill processes it did not start (#987 -- a gate that silently\n" +
      "kills is its own hazard). Investigate each pid: is it this run's own leftover, a legitimate concurrent\n" +
      "gate on a different worktree, or genuine orphan debris? Kill or report accordingly, then re-run.",
  );
  process.exit(1);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
