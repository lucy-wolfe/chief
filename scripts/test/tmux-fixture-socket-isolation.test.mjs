// A tmux socket a chief-cli fixture drives is NEVER named by a bare literal.
//
// # The defect this closes
//
// `tmux -L <name>` resolves to `$TMUX_TMPDIR`-or-`/tmp` plus `tmux-<uid>/`
// plus the name, so two processes that spell the same name share one server.
// A test that opens by killing that server — the standard way to make a reused
// name usable — therefore kills whatever ELSE is using it.
//
// Measured, not theorised. `p1_forced_dual_writer_control_creates_the_physical_
// ts_target` named `tmux-p1-control-socket` and opened with a `kill-server` on
// it. Run eight chief-cli test processes at once and seven of them fail, each
// killing another's fixture. The name was there because a TypeScript half once
// mutated that exact server; E0-S2 removed that lane on 2026-08-04 and left the
// name behind, which is the shape of the problem — the reason expires, the
// hazard does not.
//
// Nothing in a green suite would have found it. CI runs `--lib` and
// `--bin chief` side by side in ONE job, and it is only that `actuate` is
// absent from the binary that kept the two copies apart.
//
// # Why the rule is about the NAME and not about the kill
//
// Forbidding `kill-server` would be the narrower rule and the wrong one: a
// fixture that shares a name with a concurrent run is already broken before it
// tears anything down, because both are minting sessions into one server. A
// name nobody else can spell needs no teardown discipline at all, which is what
// `tmux::test_support::unique_socket` and this crate's `format!("…-{pid}-{uuid}")`
// sockets already do.
//
// A literal is still fine where NOTHING is driven — a socket asserted to be
// absent, a name handed to a scripted fake — so this reads the `-L` argument,
// which is the one place a name becomes a real server, and nothing else.
//
// Run with `node --test scripts/test/tmux-fixture-socket-isolation.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { skipSet } from "../tree-walk-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The crate that owns every tmux concern on the operator side. */
const CRATE = join(repoRoot, "apps", "chiefd", "crates", "chief-cli");

/**
 * The three ways a `-L` name reaches tmux, each with the name as a literal:
 *   .arg("-L").arg("name")      — a builder pair
 *   .args(["-L", "name"])       — an array of arguments
 *   &["-L", "name", …]          — a slice handed to a runner
 * A composed name (`&socket`, `format!(…)`, a binding) matches none of them,
 * which is the point.
 */
const LITERAL_DASH_L = [
  /\.arg\(\s*"-L"\s*\)\s*\.arg\(\s*"[^"]*"\s*\)/g,
  /"-L"\s*,\s*"[^"]*"/g,
];

const SKIPPED_DIRS = skipSet();

function rustSources(dir = CRATE, collected = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIPPED_DIRS.has(entry.name)) continue;
      rustSources(join(dir, entry.name), collected);
    } else if (entry.name.endsWith(".rs")) {
      collected.push(join(dir, entry.name));
    }
  }
  return collected;
}

/** Comment lines are excluded on purpose: the doc comment on the very test
 * this guard was written for NAMES the socket it used to spell, and a rule
 * that forbade describing the defect would delete the record of why it
 * exists. */
function isComment(line) {
  return /^\s*(\/\/|\*|\/\*)/.test(line);
}

test("chief-cli names no tmux socket it drives with a bare literal", () => {
  const offenders = [];
  for (const path of rustSources()) {
    const lines = readFileSync(path, "utf8").split("\n");
    lines.forEach((line, index) => {
      if (isComment(line)) return;
      for (const pattern of LITERAL_DASH_L) {
        for (const match of line.matchAll(pattern)) {
          offenders.push(`${relative(repoRoot, path)}:${index + 1}: ${match[0]}`);
        }
      }
    });
  }
  assert.deepEqual(
    offenders,
    [],
    "a tmux socket named by a literal is shared with every concurrent test "
      + "process that spells it. Compose the name from the pid and something "
      + "unique to the call — `tmux::test_support::unique_socket` in the "
      + `binary, \`format!("…-{pid}-{uuid}")\` in the library.\n`
      + offenders.join("\n"),
  );
});

test("the guard reads the -L argument, so it can actually fail", () => {
  // The negative control. Every static check in this repo has to prove it can
  // still see the thing it forbids, or a refactor that changes the spelling
  // turns it into a check that passes over anything.
  const shapes = [
    'Command::new("tmux").arg("-L").arg("tmux-p1-control-socket").arg("kill-server")',
    '.args(["-L", "tmux-p1-control-socket", "kill-server"])',
    'run(&["-L", "chiefd-test", "has-session"])',
  ];
  for (const shape of shapes) {
    const seen = LITERAL_DASH_L.some((pattern) => new RegExp(pattern.source).test(shape));
    assert.ok(seen, `the guard must still recognise a literal -L name: ${shape}`);
  }
  const composed = [
    '.arg("-L").arg(&self.socket)',
    '.args(["-L", &socket.0, "kill-server"])',
    'run(&socket, &["kill-server"])',
  ];
  for (const shape of composed) {
    const seen = LITERAL_DASH_L.some((pattern) => new RegExp(pattern.source).test(shape));
    assert.ok(!seen, `a composed name is what the rule ASKS for, not an offence: ${shape}`);
  }
});
