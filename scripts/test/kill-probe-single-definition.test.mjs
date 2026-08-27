// The `kill(pid, 0)` liveness judge is written exactly ONCE in the Rust tree.
//
// # The defect this closes
//
// `kill(pid, None)` sends no signal and only reports reachability, and it has
// three outcomes, not two:
//
//   * `Ok(())`  — the process exists;
//   * `ESRCH`   — the process is GONE. The only errno that proves absence;
//   * `EPERM`   — the process EXISTS and merely belongs to another user.
//
// Reading `EPERM` as death is therefore a polarity error, and every copy of
// the probe is a fresh chance to make it. The workspace had FOUR copies:
//
//   * `beacond::liveness::pid_is_live` — right, but spelled `Err(_) => true`,
//     so the arm that carried the whole judgement named nothing;
//   * `chief_cli::discovery::process_alive` — a byte-for-byte second copy
//     whose doc comment CLAIMED it matched beacond's polarity, a claim nothing
//     checked;
//   * `beacond::watchdog::pid_is_live` — `.is_ok()`. `EPERM` read as
//     owner-death, and the reaction to owner-death is `std::process::exit(0)`;
//   * `chiefd_daemon::docstore_only::install_watch_pid_watchdog` — `.is_err()`,
//     the same inversion, twice in one function.
//
// So two of the four were already wrong, in the direction that makes a daemon
// kill itself while its owner is running, and no test could see it because
// each copy was locally plausible. This file makes a fifth copy fail by file
// and line.
//
// It matches on the SHAPE — a `nix::sys::signal::kill(…)` whose signal
// argument is `None` — not on any function name, because the four copies
// shared no name between them.
//
// Run with `node --test scripts/test/kill-probe-single-definition.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const RUST_ROOT = join(repoRoot, "apps", "chiefd", "crates");

/** The one file the probe is DEFINED in. */
const DEFINITION = "apps/chiefd/crates/beacond/src/liveness.rs";

const SKIPPED_DIRS = skipSet();

function rustFiles(dir, collected = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIPPED_DIRS.has(entry.name)) continue;
      rustFiles(join(dir, entry.name), collected);
    } else if (entry.name.endsWith(".rs")) {
      collected.push(join(dir, entry.name));
    }
  }
  return collected;
}

/**
 * The argument list of a `kill(` call, read by balancing parentheses rather
 * than by a line-local regex: three of this repo's `kill` calls are written
 * across four lines, and a line-local match would miss exactly the multi-line
 * copy a future author is most likely to write.
 */
function callArguments(source, openIndex) {
  let depth = 0;
  for (let index = openIndex; index < source.length; index += 1) {
    if (source[index] === "(") depth += 1;
    else if (source[index] === ")") {
      depth -= 1;
      if (depth === 0) return source.slice(openIndex + 1, index);
    }
  }
  return undefined;
}

/**
 * The last argument of an argument list, split on TOP-LEVEL commas only.
 *
 * `args.slice(args.lastIndexOf(",") + 1)` is the obvious version and it is
 * wrong twice: rustfmt writes a TRAILING comma on every multi-line call, which
 * makes the last argument the empty string, and a turbofish or a nested call
 * can carry commas of its own. The first of those made this guard pass
 * vacuously against exactly the multi-line copy it exists to catch — observed,
 * not predicted.
 */
function lastArgument(args) {
  const parts = [];
  let depth = 0;
  let current = "";
  for (const character of args) {
    if (character === "(" || character === "<" || character === "[") depth += 1;
    else if (character === ")" || character === ">" || character === "]") depth -= 1;
    if (character === "," && depth === 0) {
      parts.push(current);
      current = "";
    } else {
      current += character;
    }
  }
  parts.push(current);
  return parts
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .pop() ?? "";
}

/** Every `signal::kill(…, None)` — i.e. every liveness PROBE, as opposed to a
 * `kill` that actually delivers SIGTERM or SIGKILL. */
function livenessProbes() {
  const found = [];
  for (const absolute of rustFiles(RUST_ROOT)) {
    const path = relative(repoRoot, absolute).split(sep).join("/");
    const source = readFileSync(absolute, "utf8");
    const pattern = /signal::kill\(/g;
    let match;
    while ((match = pattern.exec(source)) !== null) {
      const open = match.index + match[0].length - 1;
      const args = callArguments(source, open);
      if (args === undefined) continue;
      // The signal is the LAST argument. `None` in that position — with or
      // without a turbofish — is the null signal, which is the probe.
      const signal = lastArgument(args);
      if (!/^None(::<[^>]*>)?$/.test(signal)) continue;
      found.push({ path, line: source.slice(0, match.index).split("\n").length });
    }
  }
  return found;
}

test("the kill(pid, 0) liveness probe exists in exactly one place", () => {
  const probes = livenessProbes();
  // Non-vacuity: a detector that finds NOTHING reports "exactly one place" and
  // "no such code in the tree" as the same green. This guard's own first
  // draft did precisely that against a multi-line call.
  assert.deepEqual(
    probes.filter(({ path }) => path === DEFINITION).map(({ path, line }) => `${path}:${line}`).length,
    1,
    `the detector no longer finds the probe at its own definition (${DEFINITION}), so every other assertion ` +
      `in this file is passing over an empty set`
  );
  const strays = probes
    .filter(({ path }) => path !== DEFINITION)
    .map(({ path, line }) => `${path}:${line}`);
  assert.deepEqual(
    strays,
    [],
    `a second \`kill(pid, 0)\` liveness probe. Call \`beacond::liveness::pid_is_live\` instead: two of the four ` +
      `copies this guard replaced read EPERM — a process that EXISTS — as death, and the reaction to that ` +
      `verdict is \`std::process::exit(0)\` in a watchdog and "offer to deregister" in the operator client`
  );
});

test("the one definition names every outcome instead of discarding the errno", () => {
  // Comment lines are dropped on purpose: the definition NARRATES the arm it
  // replaced ("this judge used to spell the third one `Err(_) => true`"), and
  // a guard that forbade describing the defect would delete the only record of
  // why the rule exists.
  const source = readFileSync(join(repoRoot, DEFINITION), "utf8")
    .split("\n")
    .filter((line) => !/^\s*(\/\/|\*|\/\*)/.test(line))
    .join("\n");
  // The exact arm this whole packet is about. `Err(_)` here is a rich error in
  // hand, discarded, and replaced by a bare value.
  assert.ok(
    !/Err\(_\)\s*=>/.test(source),
    `${DEFINITION} discards an errno with \`Err(_) =>\`. The residual arm must bind the errno so an outcome ` +
      `that proves nothing can say so — that is the difference between a judgement and a guess wearing a ` +
      `judgement's return type`
  );
  assert.match(
    source,
    /Err\(nix::errno::Errno::EPERM\)\s*=>|Errno::EPERM\)\s*=>/,
    `${DEFINITION} must name EPERM explicitly: it is the outcome that proves EXISTENCE and is the one every ` +
      `deleted copy of this probe got wrong`
  );
  assert.match(
    source,
    /Err\(nix::errno::Errno::ESRCH\)\s*=>|Errno::ESRCH\)\s*=>/,
    `${DEFINITION} must name ESRCH explicitly: it is the only errno that proves a process is gone`
  );
});
