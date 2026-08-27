/**
 * #106 — the teardown helper must actually UNLINK the socket file.
 *
 * This test exists because the bug it guards was invisible for days: every
 * suite's teardown called `tmux kill-server`, which really does kill the
 * server — so the teardown looked correct, nothing failed, and 10,178 socket
 * files accumulated in `/tmp/tmux-0` at roughly two per minute per active
 * suite. A sweep found exactly ONE live listener among them.
 *
 * The lesson is the reason this file exists rather than the fix being trusted:
 * **an unexercised cleanup is indistinguishable from no cleanup.** Asserting
 * that `kill-server` returned proves the process died; only asserting the PATH
 * IS GONE proves the leak is closed.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { killTmuxServer, tmuxSocketPath } from "./helpers/tmux-socket";

const sockets = new Set<string>();

function socket(): string {
  const value = `tmux-teardown-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  sockets.add(value);
  return value;
}

afterEach(() => {
  for (const name of sockets) killTmuxServer(name);
  sockets.clear();
});

describe("#106 tmux teardown unlinks the socket file", () => {
  test("a real server's socket file is GONE after killTmuxServer, not merely dead", () => {
    const name = socket();
    const path = tmuxSocketPath(name);

    // A real server, exactly as every suite creates one.
    Bun.spawnSync(["tmux", "-L", name, "new-session", "-d", "sleep 60"]);

    // POSITIVE CONTROL. Without this the test could pass against a helper that
    // does nothing at all, on a socket that was never created — the same
    // "probe that cannot produce the other answer" trap that cost this project
    // a full day of false readings. If tmux is unavailable here, that is a real
    // failure of this test's premise and must be loud, not skipped.
    expect(existsSync(path), `premise: 'tmux -L ${name} new-session' must create ${path}`).toBe(true);

    killTmuxServer(name);

    // THE ASSERTION THAT WAS MISSING EVERYWHERE. `kill-server` alone leaves
    // this file behind; that is the entire defect.
    expect(existsSync(path), `killTmuxServer must UNLINK ${path}, not just kill the server`).toBe(false);
  });

  test("it is safe on a socket that never existed, and still removes a stale file", () => {
    // Teardowns run on suites that failed before starting a server, so this
    // must never throw.
    const never = socket();
    expect(() => killTmuxServer(never)).not.toThrow();

    // A server that already died leaves its file behind — which is precisely
    // the state that accumulated 10,178 entries. The unlink must NOT be
    // conditional on the kill succeeding.
    const stale = socket();
    const path = tmuxSocketPath(stale);
    Bun.spawnSync(["tmux", "-L", stale, "new-session", "-d", "sleep 60"]);
    Bun.spawnSync(["tmux", "-L", stale, "kill-server"]);
    expect(existsSync(path), `premise: kill-server must LEAVE ${path} behind (the bug itself)`).toBe(true);

    killTmuxServer(stale);
    expect(existsSync(path), "a stale socket file must be removable after its server is already gone").toBe(false);
  });

  test("it removes only the socket it was given", () => {
    const keep = socket();
    const drop = socket();
    Bun.spawnSync(["tmux", "-L", keep, "new-session", "-d", "sleep 60"]);
    Bun.spawnSync(["tmux", "-L", drop, "new-session", "-d", "sleep 60"]);

    killTmuxServer(drop);

    // Scoped by exact name, never by pattern or age: a prefix-matching cleaner
    // is one stale assumption away from killing a live server's socket out
    // from under a concurrent suite.
    expect(existsSync(tmuxSocketPath(drop)), "the named socket is removed").toBe(false);
    expect(existsSync(tmuxSocketPath(keep)), "a DIFFERENT live socket must be untouched").toBe(true);
  });
});
