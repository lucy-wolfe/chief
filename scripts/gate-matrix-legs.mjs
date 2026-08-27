// #941 rework: runs the FULL derived guard corpus as legs of the matrix,
// rather than a wrapper alongside gate-matrix.sh that keeps its own copy.
//
// WHY THIS FILE EXISTS, AND WHY IT MUST NOT ENUMERATE GUARD NAMES
// -------------------------------------------------------------------
// The merger's own #934 guard never ran in any of its batch gates, because
// every batch driver was derived by `sed` from the previous one — the chain
// forked before the guard existed, and nothing since has propagated. A
// hand-typed list of guard names here would be the SAME defect with better
// provenance: it goes stale exactly as silently, and it would then be stale
// for every seat rather than one. So this file imports
// scripts/guard-count.mjs's `deriveAllGuards()` — the SAME derivation
// `scripts/test/guard-wiring.test.mjs` and `scripts/test/guard-count.test.mjs`
// already hold to a mutation-tested standard — and runs exactly what it
// returns. A guard added to `scripts/test/` or wired into a workflow's
// `run:` line appears here with NO EDIT to this file.
//
// Emits `GATE_MATRIX_GUARD_COUNTS:test.mjs=<n>,shell-gate=<m>,combined=<n+m>`
// as its own line, derived from the same tagged list it runs — not a second,
// independently-typed count that could silently drift from what actually ran.
//
// Usage: node scripts/gate-matrix-legs.mjs --root <repoRoot>
// Exit 0 = every derived guard passed. Exit 1 = at least one failed, or the
// derivation itself was vacuous (deriveAllGuards's own callers already
// refuse on that; see guard-count.mjs).

import { spawnSync } from "node:child_process";
import { join } from "node:path";

import { deriveAllGuards } from "./guard-count.mjs";
import { createPrivateTmuxTmpdir, legEnvWithPrivateTmux } from "./private-tmux-tmpdir.mjs";

/** Strip ANSI escape codes — vitest/turbo output puts them BETWEEN a label
 * and the digits that follow it, which silently breaks any regex anchored
 * on the raw bytes (observed live: a per-package grep for `Tests` dropped
 * seven lines whole because of exactly this, with the visually-rendered
 * text reading correctly in a terminal that interprets the codes). Every
 * leg's captured output is normalized through this before any pattern
 * match, not just the ones expected to carry color. */
export function stripAnsi(text) {
  // eslint-disable-next-line no-control-regex
  return text.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "");
}

/**
 * The reporter every production `node --test` spawn ASKS FOR, rather than
 * inherits.
 *
 * # The default reporter is a Node VERSION fact
 *
 * Through Node 24 a non-TTY `node --test` defaulted to TAP; from Node 26 it
 * defaults to `spec` unconditionally, whose failure lines read `✖ name` and
 * whose tail reads `ℹ tests N`. #1035 already paid for this once inside
 * `guard-tree-purity`, whose executed-count arms parsed `# tests N`, read 0 on
 * a Node 26 host, and refused five ways about a tree that was never dirty.
 *
 * That fix pinned the reporter for the NESTED runner in one test file. The
 * three sites that run the real suite did not, and one of them parses the
 * format: `ci-guard-shard.mjs` extracts a failing shard's failing subtests with
 * `startsWith("not ok ")`. Under `spec` that filter matches nothing, so a red
 * shard prints its verdict and then a tail with none of the failing lines in
 * it — the exit status is still right, and the DIAGNOSIS silently empties.
 *
 * Nothing in CI pins the Node version either, so this is not a change we would
 * make; it is a change GitHub can make for us, on a runner image bump, with no
 * commit anywhere in this repo.
 *
 * One definition, imported by every spawn site, so the format we parse and the
 * format we ask for cannot drift apart again.
 */
export const NODE_TEST_REPORTER_ARGS = ["--test-reporter=tap"];

/** Build the runnable command for one derived guard entry. test.mjs guards
 * run directly via `node --test` against the file the derivation named —
 * bypassing package.json's script-name indirection entirely, since every
 * test:* wiring for these files is exactly `node --test scripts/test/<name>`
 * and re-deriving that through package.json would be a second resolver for
 * a fact the directory listing already states. shell-gate guards run the
 * exact file the derivation resolved, not the (possibly indirect) `via`
 * command string — the file IS the guard; how CI's package.json happens to
 * spell invoking it is not a second identity for the same guard. */
export function legCommand(entry, root) {
  if (entry.category === "test.mjs") {
    return {
      cmd: "node",
      args: ["--test", ...NODE_TEST_REPORTER_ARGS, join(root, "scripts", "test", entry.name)],
    };
  }
  if (entry.category === "bun-test-suite") {
    return { cmd: "bun", args: ["test", join(root, entry.name)] };
  }
  return { cmd: "bash", args: [join(root, entry.name)] };
}

/** Suites the corpus derives but this driver deliberately does NOT run,
 * each keyed to the OPEN issue that closes the entry. The map is empty and
 * that is a fact about the tree, not an oversight: its only entry was
 * `apps/cli/test/CollaborativeOpsLockFree.test.ts` (withheld pending #981
 * because its conformance subtest asserted five file-mutex symbols were
 * deleted while four still existed), and #751/E4 deleted that suite outright
 * along with the whole lock mechanism it was arguing about — chiefd's
 * single-writer queue replaced it, so there is no longer a symbol worklist
 * to disagree about. Mandate 0: a withhold row naming a file that no longer
 * exists is removed, not repointed, and it must not be repointed at a
 * surviving suite that was never the subject.
 *
 * The MECHANISM stays. It is not dead code waiting for a caller: an empty
 * map makes `runnable` the whole derived corpus and prints no RESIDUAL
 * SCOPE block, which is exactly the correct output today, and the next suite
 * that genuinely cannot run gets one line here plus the issue that clears
 * it — rather than being silently dropped from a hand-edited leg list, which
 * is the defect this whole file exists to prevent. */
export const WITHHELD_BUN_TEST_SUITES = {};


/** #941 follow-up (merger): the CATEGORICAL split.
 *
 * The corpus runs `[test.mjs]` entries ONLY. The three CI-wired shell gates
 * are run as EXPLICIT STAGES by gate-matrix.sh, because they are
 * order-sensitive in a way corpus-time cannot express: cargo-test-workspace.sh
 * must run after the release build AND after in-repo provisioning, and
 * typecheck.sh sits at a fixed point in the sequence. Letting the corpus own
 * them silently moved them.
 *
 * An enumerated EXCLUSION list here would rot exactly like every other
 * hand-typed list this packet exists to retire: add a fourth shell gate to
 * ci.yml and it would be skipped by BOTH sides — excluded here, and with no
 * explicit stage running it — with nothing saying so. So the split is
 * categorical, and the two sides are RECONCILED: the set of [shell-gate]
 * entries the derivation returns must EQUAL the set gate-matrix.sh says it
 * runs explicitly. Any difference, in either direction, REFUSES and names it.
 *
 * Two derivations that must agree, rather than one list trusted.
 */
export function reconcileShellGates(derivedShellGateNames, explicitNames) {
  const norm = (n) => String(n).trim().replace(/^\.\//, "");
  const derived = new Set(derivedShellGateNames.map(norm));
  const explicit = new Set(explicitNames.map(norm));
  const missingFromMatrix = [...derived].filter((n) => !explicit.has(n)).sort();
  const notDerived = [...explicit].filter((n) => !derived.has(n)).sort();
  return { ok: missingFromMatrix.length === 0 && notDerived.length === 0, missingFromMatrix, notDerived };
}

/** The spawn options EVERY leg is run with.
 *
 * It exists as a named function, rather than as an object literal at the one
 * call site, so the rule it carries can be tested by CALLING it. A regex over
 * this file could only ever check that the call site LOOKS right; this can be
 * handed a bad namespace and asked what it does, which is the difference
 * between pinning a rule and pinning a spelling.
 *
 * `env` is the whole point. Before #1204's follow-up this call passed no `env`
 * at all, so every leg inherited an environment with `TMUX_TMPDIR` unset and
 * any unsocketed `tmux` in the corpus reached `/tmp/tmux-<uid>/default` — the
 * operator's own server, which one guard run destroyed along with several
 * people's live sessions.
 *
 * @returns {import("node:child_process").SpawnSyncOptionsWithStringEncoding}
 */
export function legSpawnOptions(root, tmuxTmpdir, baseEnv = process.env) {
  return {
    encoding: "utf8",
    cwd: root,
    maxBuffer: 1024 * 1024 * 64,
    env: legEnvWithPrivateTmux(baseEnv, tmuxTmpdir),
  };
}

export function guardCountsLine(guards) {
  const testMjs = guards.filter((g) => g.category === "test.mjs").length;
  const shellGate = guards.filter((g) => g.category === "shell-gate").length;
  const bunTestSuite = guards.filter((g) => g.category === "bun-test-suite").length;
  return `GATE_MATRIX_GUARD_COUNTS:test.mjs=${testMjs},shell-gate=${shellGate},bun-test-suite=${bunTestSuite},combined=${testMjs + shellGate + bunTestSuite}`;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  let root = process.cwd();
  const explicitShellGates = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--root") root = args[++i];
    if (args[i] === "--explicit-shell-gate") explicitShellGates.push(args[++i]);
  }

  // CONTAINMENT FIRST, before a single leg is spawned. Established here and
  // inherited by everything below, so no leg — present or future — can name
  // the operator's tmux server.
  const tmuxTmpdir = createPrivateTmuxTmpdir();
  console.log(`[gate-matrix-legs] private tmux namespace for this run: ${tmuxTmpdir}`);

  const guards = deriveAllGuards({
    guardTestDir: join(root, "scripts", "test"),
    workflowsDir: join(root, ".github", "workflows"),
    packageJsonPath: join(root, "package.json"),
  });

  if (guards.length === 0) {
    console.error("[gate-matrix-legs] REFUSING TO REPORT SUCCESS: deriveAllGuards() returned zero guards — a vacuity failure in the derivation, not evidence the tree has no guards.");
    process.exit(1);
  }

  // The counts line still reports the FULL derivation, because it is a fact
  // about the tree, not a description of what this process chose to run.
  console.log(guardCountsLine(guards));

  const derivedShell = guards.filter((g) => g.category === "shell-gate").map((g) => g.name);
  const rec = reconcileShellGates(derivedShell, explicitShellGates);
  if (!rec.ok) {
    console.error("[gate-matrix-legs] REFUSING TO GATE: the derived [shell-gate] set and the matrix's explicit stage set disagree.");
    for (const n of rec.missingFromMatrix) console.error(`  DERIVED BUT NOT RUN BY THE MATRIX: ${n} — it is CI-wired and nothing in this run executes it.`);
    for (const n of rec.notDerived) console.error(`  RUN BY THE MATRIX BUT NOT DERIVED: ${n} — the matrix names a shell gate the derivation does not know about.`);
    process.exit(1);
  }

  const runnable = guards.filter(
    (g) => g.category === "test.mjs" || (g.category === "bun-test-suite" && !(g.name in WITHHELD_BUN_TEST_SUITES)),
  );
  const withheld = guards.filter((g) => g.category === "bun-test-suite" && g.name in WITHHELD_BUN_TEST_SUITES);

  let rc = 0;
  const results = [];
  for (const entry of runnable) {
    const { cmd, args: cmdArgs } = legCommand(entry, root);
    const startMs = Date.now();
    const result = spawnSync(cmd, cmdArgs, legSpawnOptions(root, tmuxTmpdir));
    const durationMs = Date.now() - startMs;
    const output = stripAnsi(`${result.stdout || ""}${result.stderr || ""}`);
    const ok = result.status === 0;
    if (!ok) rc = 1;
    const label = `[${entry.category}] ${entry.name}`;
    console.log(`${ok ? "PASS" : "FAIL"}  ${label}  (${durationMs}ms)`);
    if (!ok) {
      console.log(`  exit=${result.status}`);
      console.log(
        output
          .split("\n")
          .slice(-40)
          .map((l) => `  ${l}`)
          .join("\n")
      );
    }
    results.push({ label, ok, durationMs });
  }

  const passed = results.filter((r) => r.ok).length;
  console.log(`[gate-matrix-legs] ${passed}/${results.length} derived guards passed`);

  // #977 residual scope, stated rather than left implicit: this driver does
  // NOT run 100% of what CI runs. It runs the full [test.mjs] corpus and all
  // [bun-test-suite] legs except those explicitly withheld pending a named
  // issue. Anyone reading a green result should be able to see the gap from
  // this output alone, not have to know it from memory.
  if (withheld.length > 0) {
    console.log(`[gate-matrix-legs] RESIDUAL SCOPE: ${withheld.length} bun-test-suite leg(s) withheld, not run by this driver:`);
    for (const w of withheld) console.log(`  WITHHELD  [${w.category}] ${w.name}  (pending ${WITHHELD_BUN_TEST_SUITES[w.name]})`);
  }
  process.exit(rc);
}
