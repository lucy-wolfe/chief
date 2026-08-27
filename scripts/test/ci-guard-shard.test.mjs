import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { parseShardArgs, runSelectedGuards, selectShard } from "../ci-guard-shard.mjs";

const entries = Array.from({ length: 9 }, (_, index) => ({ category: "test.mjs", name: `${index}.test.mjs` }));

function withGuardFixture(sourceForRoot, body) {
  const root = mkdtempSync(join(tmpdir(), "ci-guard-shard-test-"));
  try {
    mkdirSync(join(root, "scripts", "test"), { recursive: true });
    writeFileSync(join(root, "scripts", "test", "fixture.test.mjs"), sourceForRoot(root));
    writeFileSync(join(root, ".gitignore"), "ignored-output/\n");
    execFileSync("git", ["init", "-q"], { cwd: root });
    execFileSync("git", ["add", "-A"], { cwd: root });
    execFileSync(
      "git",
      ["-c", "user.name=CI shard test", "-c", "user.email=ci-shard@example.invalid", "commit", "-q", "-m", "fixture"],
      { cwd: root },
    );
    body(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("selectShard assigns every guard to one stable round-robin shard", () => {
  const shards = Array.from({ length: 4 }, (_, shard) => selectShard(entries, shard, 4).map((entry) => entry.name));
  assert.deepEqual(shards, [
    ["0.test.mjs", "4.test.mjs", "8.test.mjs"],
    ["1.test.mjs", "5.test.mjs"],
    ["2.test.mjs", "6.test.mjs"],
    ["3.test.mjs", "7.test.mjs"],
  ]);
  assert.deepEqual(shards.flat().sort(), entries.map((entry) => entry.name).sort());
});

test("selectShard rejects an invalid shard", () => {
  assert.throws(() => selectShard(entries, 4, 4), /invalid guard shard/);
  assert.throws(() => selectShard(entries, 0, 0), /invalid guard shard/);
});

test("selectShard excludes serial guards before assigning round-robin slots", () => {
  assert.deepEqual(
    selectShard(entries, 0, 2, new Set(["0.test.mjs"])).map((entry) => entry.name),
    ["1.test.mjs", "3.test.mjs", "5.test.mjs", "7.test.mjs"],
  );
});

test("parseShardArgs reads the shard identity, exclusions, and optional root", () => {
  assert.deepEqual(parseShardArgs(["--shard", "2", "--shards", "4", "--exclude", "0.test.mjs", "--root", "/tmp/repo"]), {
    shard: 2,
    shards: 4,
    root: "/tmp/repo",
    excludedNames: new Set(["0.test.mjs"]),
  });
});

test("the real shard runner rejects a passing guard that changes a tracked file", () => {
  withGuardFixture(
    (root) => `import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
test("pollutes", () => writeFileSync(join(${JSON.stringify(root)}, ".gitignore"), "changed\\n"));
`,
    (root) => {
      assert.equal(runSelectedGuards(root, [{ name: "fixture.test.mjs" }]), 1);
    },
  );
});

test("the real shard runner accepts a guard that restores its directory", () => {
  withGuardFixture(
    (root) => `import { mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
test("restores", () => {
  const path = join(${JSON.stringify(root)}, "probe", "empty");
  mkdirSync(path, { recursive: true });
  rmSync(join(${JSON.stringify(root)}, "probe"), { recursive: true });
});
`,
    (root) => {
      assert.equal(runSelectedGuards(root, [{ name: "fixture.test.mjs" }]), 0);
    },
  );
});

test("the top-level runner accepts one CI shard or the isolated serial guard", () => {
  const runner = readFileSync(join(process.cwd(), "scripts", "ci-guard-shards.mjs"), "utf8");
  assert.match(runner, /--shard/);
  assert.match(runner, /--skip-serial/);
  assert.match(runner, /--serial-only/);
});

test("parallel guard worktrees keep the live mutating guard out of child shards", () => {
  const runner = readFileSync(join(process.cwd(), "scripts", "ci-guard-shards.mjs"), "utf8");
  assert.match(runner, /serialGuardNames = \["assert-typecheck-nonvacuous\.test\.mjs"\]/);
  assert.match(runner, /args\.push\("--exclude", name\)/);
  assert.match(runner, /runSerialGuardsWithPurity\(root, serialGuardNames\)/);
});
