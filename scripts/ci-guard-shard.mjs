import { spawnSync } from "node:child_process";
import { join } from "node:path";

import { deriveAllGuards } from "./guard-count.mjs";
import { describeDiff, diffSnapshots, isClean, snapshotTree } from "./guard-tree-purity.mjs";
import { GUARD_WIRING_MANIFEST } from "./guard-wiring-manifest.mjs";
import { NODE_TEST_REPORTER_ARGS, stripAnsi } from "./gate-matrix-legs.mjs";

/** Split the wired guard corpus in a stable round-robin order.
 * Round-robin keeps the known slow guards spread across shards without a
 * hand-maintained list of guard names. */
export function selectShard(entries, shard, shardCount, excludedNames = new Set()) {
  if (!Number.isInteger(shard) || !Number.isInteger(shardCount) || shard < 0 || shard >= shardCount || shardCount < 1) {
    throw new Error(`invalid guard shard ${shard}/${shardCount}`);
  }
  let testIndex = 0;
  return entries.filter((entry) => {
    if (entry.category !== "test.mjs") return false;
    if (excludedNames.has(entry.name)) return false;
    const selected = testIndex % shardCount === shard;
    testIndex += 1;
    return selected;
  });
}

export function parseShardArgs(args) {
  let root = process.cwd();
  let shard;
  let shards;
  const excludedNames = new Set();
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--root") root = args[++index];
    else if (args[index] === "--shard") shard = Number(args[++index]);
    else if (args[index] === "--shards") shards = Number(args[++index]);
    else if (args[index] === "--exclude") excludedNames.add(args[++index]);
    else throw new Error(`unknown argument: ${args[index]}`);
  }
  if (!Number.isInteger(shard) || !Number.isInteger(shards)) {
    throw new Error("usage: node scripts/ci-guard-shard.mjs --shard N --shards COUNT [--root PATH]");
  }
  return { root, shard, shards, excludedNames };
}

/** Run one selected guard set and fail if it changes any non-ignored path. */
export function runSelectedGuards(root, selected) {
  const before = snapshotTree(root);
  const childEnv = { ...process.env };
  // A runner invoked from a Node test inherits this marker. A nested
  // `node --test` then skips every file and exits zero unless it is removed.
  delete childEnv.NODE_TEST_CONTEXT;
  let exitCode = 0;
  let passedCount = 0;
  for (const entry of selected) {
    const target = join(root, "scripts", "test", entry.name);
    const startedAt = Date.now();
    const result = spawnSync(process.execPath, ["--test", ...NODE_TEST_REPORTER_ARGS, target], {
      cwd: root,
      encoding: "utf8",
      env: childEnv,
      maxBuffer: 1024 * 1024 * 64,
    });
    const durationMs = Date.now() - startedAt;
    const output = stripAnsi(`${result.stdout ?? ""}${result.stderr ?? ""}`);
    const passed = result.status === 0;
    if (!passed) exitCode = 1;
    else passedCount += 1;
    console.log(`${passed ? "PASS" : "FAIL"} [${entry.name}] (${durationMs}ms)`);
    if (!passed) {
      console.log(`  exit=${result.status ?? "signal"}`);
      // The FAILING subtests are what a reader needs, and node's TAP puts each
      // one where it ran -- often hundreds of lines above the tail. A 40-line
      // window reliably printed the last few PASSING subtests and the summary
      // counts, and nothing about the failure. So print the failing lines
      // first, in full, and keep a tail for context.
      //
      // `not ok ` is TAP, which is why the spawn above ASKS for TAP. Left to
      // the host's default this filter matches nothing on Node 26 (`spec`
      // renders failures as `✖ name`), and a red shard reports its verdict with
      // an empty diagnosis -- correct exit code, no evidence.
      const lines = output.split("\n");
      for (const line of lines.filter((line) => line.trimStart().startsWith("not ok ")).slice(0, 40)) {
        console.log(`  ${line}`);
      }
      console.log(lines.slice(-200).map((line) => `  ${line}`).join("\n"));
    }
  }

  const residue = diffSnapshots(before, snapshotTree(root));
  if (!isClean(residue)) {
    exitCode = 1;
    console.error(
      "[guard-tree-purity] guard shard changed its working tree. " +
        "Each live probe must restore files and directories exactly:\n" +
        describeDiff(residue),
    );
  }
  console.log(`[ci-guard-shard] ${passedCount}/${selected.length} selected guards passed`);
  return exitCode;
}

function runGuardShard({ root, shard, shards, excludedNames }) {
  const guards = deriveAllGuards({
    guardTestDir: join(root, "scripts", "test"),
    workflowsDir: join(root, ".github", "workflows"),
    packageJsonPath: join(root, "package.json"),
  });
  const wired = guards.filter(
    (entry) => entry.category === "test.mjs" && GUARD_WIRING_MANIFEST[entry.name]?.status === "wired",
  );
  if (wired.length === 0) throw new Error("refusing to run an empty wired repo guard corpus");
  const selected = selectShard(wired, shard, shards, excludedNames);
  console.log(`CI_GUARD_SHARD: shard=${shard + 1}/${shards} selected=${selected.length} total=${wired.length}`);
  return runSelectedGuards(root, selected);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    process.exitCode = runGuardShard(parseShardArgs(process.argv.slice(2)));
  } catch (error) {
    console.error(`[ci-guard-shard] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
