// Third sighting tonight of one class: absence and failure look identical in
// this gate's output. #942 was misreported as "confirmed failing" when the
// real story was `@chief/piing`'s test:unit being SIGINT'd (exit 130) mid-run
// after `@chief/cli`'s (unrelated, pre-existing) failure -- because
// `turbo run test:unit` had no `--continue`, so ONE package's failure
// cancelled every other in-flight package, and the cancelled package's
// torn-down-tmux/mock-server stderr noise read exactly like a real assertion
// failure. `--continue` (added to package.json alongside this file, mirroring
// `lint`'s existing flag) stops the collateral kill. This script makes the
// remaining distinction VISIBLE rather than relying on the flag alone: a
// killed or never-reached package must be NAMED as such, never silently
// absent and never folded into the same "FAIL" bucket as a real assertion
// failure -- the next interruption (an OOM kill, a manual Ctrl-C, a CI runner
// timeout) will not come from this exact cause, and a report that only knows
// "pass" and "fail" will misclassify it again.
//
// Scope is DERIVED from turbo's own `--dry=json` (never a hand-typed package
// list, matching this repo's established standard) so a new package
// declaring `test:unit` is picked up automatically rather than the guard
// rotting silently behind package.json's growth.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const SIGNAL_EXIT_CODES = {
  130: "SIGINT",
  137: "SIGKILL",
  143: "SIGTERM",
};

/** Every package that actually declares the given turbo task, from turbo's
 *  own dry-run plan -- never a hand-maintained list. */
export function scopedPackages(root, task) {
  const raw = execFileSync(
    process.platform === "win32" ? "npx.cmd" : "npx",
    ["turbo", "run", task, "--dry=json"],
    { cwd: root, encoding: "utf8" },
  );
  const plan = JSON.parse(raw);
  const names = new Set();
  for (const t of plan.tasks ?? []) {
    if (t.task === task && t.package && t.package !== "//") names.add(t.package);
  }
  return [...names].sort();
}

function stripAnsi(text) {
  return text.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "");
}

/** Classify one package's fate from a turbo `--output-logs=full` log:
 *  'pass'      — a Test Files summary line with zero failures
 *  'fail'      — a Test Files summary line reporting >=1 failure, OR a
 *                non-signal nonzero script exit (a real crash, not a kill)
 *  'killed'    — a script exit line whose code is a known termination signal
 *                and NO Test Files summary line ever appeared for it
 *  'unreached' — the package appears nowhere in the log at all: turbo never
 *                started it (cancelled before spawn), or emitted nothing */
export function classifyPackage(logText, task, pkg) {
  const prefix = `${pkg}:${task}:`;
  const lines = logText.split("\n").filter((line) => line.startsWith(prefix));
  if (lines.length === 0) return { package: pkg, status: "unreached", detail: "no log lines for this package at all" };

  const summaryLine = lines.find((line) => /Test Files\s+\d/.test(line));
  const exitLine = lines.find((line) => /error: script ".*" exited with code (\d+)/.test(line));

  if (summaryLine) {
    const failedMatch = summaryLine.match(/(\d+)\s+failed/);
    if (failedMatch && Number(failedMatch[1]) > 0) {
      return { package: pkg, status: "fail", detail: summaryLine.trim() };
    }
    return { package: pkg, status: "pass", detail: summaryLine.trim() };
  }

  if (exitLine) {
    const code = Number(exitLine.match(/exited with code (\d+)/)[1]);
    const signal = SIGNAL_EXIT_CODES[code];
    if (signal) {
      return {
        package: pkg,
        status: "killed",
        detail: `exit ${code} (${signal}) — cancelled mid-run, never produced a Test Files summary; this is NOT a failed assertion`,
      };
    }
    return { package: pkg, status: "fail", detail: `exit ${code} (not a known termination signal) with no Test Files summary` };
  }

  return { package: pkg, status: "unreached", detail: "package logged output but never reached a Test Files summary or an exit line" };
}

export function classifyLog(root, task, logText) {
  const scope = scopedPackages(root, task);
  const clean = stripAnsi(logText);
  return { scope, results: scope.map((pkg) => classifyPackage(clean, task, pkg)) };
}

function main() {
  const [, , logPath, task = "test:unit"] = process.argv;
  if (!logPath) {
    console.error("usage: node scripts/turbo-package-completion.mjs <log-file> [task]");
    process.exit(3);
  }
  const root = new URL("..", import.meta.url).pathname;
  const { results } = classifyLog(root, task, readFileSync(logPath, "utf8"));

  let failCount = 0;
  let killedCount = 0;
  let unreachedCount = 0;
  for (const r of results) {
    const label = { pass: "PASS", fail: "FAIL", killed: "KILLED", unreached: "UNREACHED" }[r.status];
    console.log(`${label.padEnd(10)} ${r.package}${r.detail ? ` — ${r.detail}` : ""}`);
    if (r.status === "fail") failCount += 1;
    if (r.status === "killed") killedCount += 1;
    if (r.status === "unreached") unreachedCount += 1;
  }
  if (killedCount > 0) {
    console.log(`\n${killedCount} package(s) were KILLED mid-run (collateral of another package's failure without --continue), never a real assertion failure — do not report these as failing tests.`);
  }
  if (unreachedCount > 0) {
    console.log(`\n${unreachedCount} package(s) are UNREACHED — never started, or started and emitted nothing this parser recognizes. Investigate before trusting this run's total.`);
  }
  process.exit(failCount > 0 || unreachedCount > 0 || killedCount > 0 ? 1 : 0);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
