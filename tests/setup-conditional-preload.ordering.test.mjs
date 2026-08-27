// #962: the RUNTIME proof that setup-conditional-preload.ts's ordering
// guarantee is real, not assumed. tests/setup-conditional-preload.ts
// dynamically `import()`s the workspace-build preflight then
// setup-durable-store.ts, SEQUENTIALLY (`await` before each), so the
// preflight's own module evaluation -- including the synchronous throw its
// `Bun.resolveSync` checks produce -- must complete before the second
// import is even attempted. That is an ECMAScript guarantee of `await`,
// not a timing assumption, but "the language guarantees it" and "this
// specific file's control flow actually uses it correctly" are different
// claims -- this test proves the second by spawning a process running the
// EXACT control-flow shape (tests/fixtures/ordering-proof/wrapper.mjs:
// `await import("./first.mjs"); await import("./second.mjs");`) against
// two instrumented fixture modules, in both directions:
//   SUCCESS PATH: both modules run, in order.
//   FAILURE PATH (mirrors the real defect this ordering exists to prevent
//   -- #937's bare "Cannot find module '@chief/piing'" masking every
//   file): the first module throws during its own evaluation; the second
//   must NEVER run.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const WRAPPER = join(HERE, "fixtures", "ordering-proof", "wrapper.mjs");

function runWrapper(extraEnv) {
  const dir = mkdtempSync(join(tmpdir(), "ordering-proof-"));
  const logPath = join(dir, "order.log");
  let error;
  try {
    execFileSync(process.execPath, [WRAPPER], {
      env: { ...process.env, ORDERING_PROOF_LOG_PATH: logPath, ...extraEnv },
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (caught) {
    error = caught;
  }
  const events = existsSync(logPath) ? readFileSync(logPath, "utf8").trim().split("\n").filter(Boolean) : [];
  rmSync(dir, { recursive: true, force: true });
  return { events, error };
}

test("SUCCESS PATH: sequential await import() runs both modules, in source order", () => {
  const { events, error } = runWrapper({});
  assert.equal(error, undefined, "the wrapper must exit cleanly when neither fixture throws");
  assert.deepEqual(events, ["first", "second"], "first.mjs must be fully evaluated before second.mjs starts");
});

test("FAILURE PATH: when the first module throws during evaluation, the second is never reached", () => {
  const { events, error } = runWrapper({ ORDERING_PROOF_FIRST_THROWS: "1" });
  assert.ok(error, "the wrapper must propagate the first module's throw, not swallow it");
  assert.deepEqual(
    events,
    ["first"],
    "second.mjs's import() must never even be attempted once first.mjs has thrown -- " +
      "this is the exact property that keeps setup-durable-store.ts's own worse error from firing first",
  );
});
