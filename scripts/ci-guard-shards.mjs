import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

import { NODE_TEST_REPORTER_ARGS } from "./gate-matrix-legs.mjs";
import { describeDiff, diffSnapshots, isClean, snapshotTree } from "./guard-tree-purity.mjs";

function run(command, args, cwd) {
  return new Promise((resolve) => {
    const child = spawn(command, args, { cwd, stdio: "inherit" });
    child.once("error", () => resolve(1));
    child.once("close", (code) => resolve(code ?? 1));
  });
}

function parseArgs(args) {
  let shards = 4;
  let shard;
  let skipSerial = false;
  let serialOnly = false;
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--shards") shards = Number(args[++index]);
    else if (args[index] === "--shard") shard = Number(args[++index]);
    else if (args[index] === "--skip-serial") skipSerial = true;
    else if (args[index] === "--serial-only") serialOnly = true;
    else throw new Error(`unknown argument: ${args[index]}`);
  }
  if (!Number.isInteger(shards) || shards < 1) throw new Error("--shards must be a positive integer");
  if (shard !== undefined && (!Number.isInteger(shard) || shard < 0 || shard >= shards)) {
    throw new Error(`--shard must be an integer in the range 0..${shards - 1}`);
  }
  if (skipSerial && serialOnly) throw new Error("--skip-serial and --serial-only cannot be combined");
  if (serialOnly && shard !== undefined) throw new Error("--serial-only cannot be combined with --shard");
  return { shards, shard, skipSerial, serialOnly };
}

function prepareShardWorktree(repoRoot, tempRoot, shard) {
  const childRoot = join(tempRoot, `.ci-guard-shard-${shard}`);
  execFileSync("git", ["worktree", "add", "--detach", childRoot, "HEAD"], { cwd: repoRoot, stdio: "inherit" });
  const nodeModules = join(repoRoot, "node_modules");
  if (existsSync(nodeModules)) {
    symlinkSync(nodeModules, join(childRoot, "node_modules"), process.platform === "win32" ? "junction" : "dir");
  }
  return childRoot;
}

async function runSerialGuardsWithPurity(root, guardNames) {
  const before = snapshotTree(root);
  let exitCode = 0;
  for (const name of guardNames) {
    console.log(`Running isolated serial guard: ${name}`);
    const code = await run(
      process.execPath,
      ["--test", ...NODE_TEST_REPORTER_ARGS, join(root, "scripts", "test", name)],
      root,
    );
    if (code !== 0) exitCode = code;
  }
  const residue = diffSnapshots(before, snapshotTree(root));
  if (!isClean(residue)) {
    console.error(
      "[guard-tree-purity] serial guard path changed its working tree. " +
        "Each live probe must restore files and directories exactly:\n" +
        describeDiff(residue),
    );
    exitCode = 1;
  }
  return exitCode;
}

async function runShard(childRoot, shard, shards, excludedNames) {
  const buildCode = await run(
    "bun",
    ["x", "turbo", "run", "build", "--filter=./packages/*", "--output-logs=new-only"],
    childRoot,
  );
  if (buildCode !== 0) return buildCode;
  const args = ["scripts/ci-guard-shard.mjs", "--shard", String(shard), "--shards", String(shards)];
  for (const name of excludedNames) args.push("--exclude", name);
  return run(process.execPath, args, childRoot);
}

async function main() {
  const { shards, shard, skipSerial, serialOnly } = parseArgs(process.argv.slice(2));
  const root = process.cwd();
  // This guard edits packages/testing and runs the full workspace build twice
  // to prove the typecheck gate. Keep that live mutation outside the parallel
  // worktrees; the other guards remain parallel and read-only with respect to
  // the checked-out source.
  const serialGuardNames = ["assert-typecheck-nonvacuous.test.mjs"];
  let serialCode = 0;
  if (!skipSerial) {
    serialCode = await runSerialGuardsWithPurity(root, serialGuardNames);
  }
  if (serialOnly) {
    process.exitCode = serialCode;
    return;
  }
  if (shard !== undefined) {
    const buildCode = await run(
      "bun",
      ["x", "turbo", "run", "build", "--filter=./packages/*", "--output-logs=new-only"],
      root,
    );
    if (buildCode !== 0) {
      process.exitCode = buildCode;
      return;
    }
    const args = ["scripts/ci-guard-shard.mjs", "--shard", String(shard), "--shards", String(shards)];
    for (const name of serialGuardNames) args.push("--exclude", name);
    process.exitCode = await run(process.execPath, args, root);
    return;
  }
  const worktreeParent = process.env.HOME ? join(process.env.HOME, "worktrees") : tmpdir();
  mkdirSync(worktreeParent, { recursive: true });
  const tempRoot = mkdtempSync(join(worktreeParent, "ci-guard-shards-"));
  const childRoots = [];
  console.log(`CI_GUARD_SHARDS: count=${shards}`);
  try {
    // Git updates the parent repository's worktree registry for every add.
    // Creating these entries concurrently races on that registry and can
    // leave a child worktree with only part of the checkout. Create them in
    // order, then run the expensive builds and guards concurrently.
    for (let shard = 0; shard < shards; shard += 1) {
      console.log(`Preparing worktree for shard ${shard + 1}/${shards}`);
      childRoots.push(prepareShardWorktree(root, tempRoot, shard));
    }
    const results = await Promise.all(childRoots.map((childRoot, shard) => runShard(childRoot, shard, shards, serialGuardNames)));
    if (serialCode !== 0 || results.some((code) => code !== 0)) process.exitCode = 1;
  } finally {
    for (const childRoot of childRoots.reverse()) {
      execFileSync("git", ["worktree", "remove", "--force", childRoot], { cwd: root, stdio: "inherit" });
    }
    rmSync(tempRoot, { recursive: true, force: true });
  }
}

try {
  await main();
} catch (error) {
  console.error(`[ci-guard-shards] ${error instanceof Error ? error.stack : String(error)}`);
  process.exitCode = 1;
}
