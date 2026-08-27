/**
 * #106 — tearing down a test tmux server, including its socket FILE.
 *
 * `tmux kill-server` kills the server process and **never unlinks the socket
 * file**. Reproduced directly, not inferred:
 *
 *     tmux -L probe new-session -d 'sleep 60'   -> /tmp/tmux-0/probe exists
 *     tmux -L probe kill-server ; sleep 1       -> SOCKET FILE REMAINS
 *
 * So a teardown that only calls `kill-server` is **correct in intent and
 * incomplete in effect**: the server really is gone (a sweep of 10,178 leaked
 * files found exactly ONE live listener), but the directory entry stays
 * forever. At roughly two files per minute per active suite that reached
 * 10,178 entries in `/tmp/tmux-0` from a few days of ordinary test runs.
 *
 * merger-6's framing, which is the thing to remember:
 * **a socket nobody unlinks is a leak with a correct-looking teardown.**
 *
 * It costs no disk — sockets are 0 bytes — so nothing about it shows up as
 * pressure. It shows up as tens of thousands of directory entries, which is
 * why a periodic sweep would have reset the clock forever rather than fixing
 * anything.
 *
 * # Scoped to names the caller minted, never a pattern
 *
 * [`killTmuxServer`] unlinks EXACTLY the socket it was handed. It never globs,
 * never matches a prefix, and never reasons about age. A pattern-based cleaner
 * is one stale assumption away from deleting a live server's socket out from
 * under it — the sweep that recovered those 10,178 files probed each one
 * individually for that reason. Suites already track the names they generate;
 * that set is the authority.
 */
import { rmSync } from "node:fs";
import { join } from "node:path";

/**
 * Where tmux puts the socket for `-L <socketName>`: `$TMUX_TMPDIR/tmux-<uid>/`,
 * defaulting to `/tmp` exactly as tmux itself does.
 */
export function tmuxSocketPath(socketName: string): string {
  // process-env-ok: TMUX_TMPDIR is a genuine ambient property of the machine
  // (mirroring tmux's own resolution), not fixture-specific config -- no
  // fixture's scoped environment would ever want to shadow it.
  const base = process.env.TMUX_TMPDIR || "/tmp";
  const uid = typeof process.getuid === "function" ? process.getuid() : 0;
  return join(base, `tmux-${uid}`, socketName);
}

/**
 * Kill a test tmux server AND unlink its socket file.
 *
 * Both halves are best-effort and independent on purpose. A server that never
 * started, or already died, must not fail a teardown — but its socket file may
 * still exist and still needs removing, so the unlink is NOT conditional on the
 * kill succeeding. That ordering is the actual bug this fixes: every existing
 * teardown stopped after the kill.
 */
export function killTmuxServer(socketName: string): void {
  if (!socketName) return;
  try {
    Bun.spawnSync(["tmux", "-L", socketName, "kill-server"]);
  } catch {
    /* never started, or already gone — the unlink below still matters */
  }
  try {
    rmSync(tmuxSocketPath(socketName), { force: true });
  } catch {
    /* another teardown removed it, or it was never created */
  }
}
