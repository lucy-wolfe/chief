// A test run may only reach a tmux server it could have started itself.
//
// # The defect this closes
//
// `bun run test:pre-push-guards` destroyed live tmux sessions belonging to
// several people on a shared box.
//
// `tmux -L <name>` resolves to `$TMUX_TMPDIR`-or-`/tmp`, plus `tmux-<uid>/`,
// plus the name. So `TMUX_TMPDIR` is not a preference about where scratch
// files go — it is the NAMESPACE, and it is the only thing standing between a
// command with no `-L` and `/tmp/tmux-0/default`, which on an operator's box
// is their own server with their own work in it.
//
// `scripts/gate-matrix-legs.mjs` spawned every guard leg with
// `spawnSync(cmd, args, { encoding, cwd, maxBuffer })` — no `env` key at all.
// Every leg therefore inherited the ambient environment, in which
// `TMUX_TMPDIR` is unset on any ordinary box, and one unsocketed
// `tmux kill-server` anywhere in the corpus reached the operator.
//
// # Why the rule lives here and not at the call sites
//
// The guard corpus is DERIVED: `deriveAllGuards()` reads the directory
// listing, so a new `scripts/test/*.test.mjs` file joins the run with no edit
// to any list, carrying whatever its author assumed about the environment. A
// per-fixture defence — the shape a guard that stubs its own `tmux` onto PATH
// implements correctly for itself — protects exactly the fixtures somebody
// remembered to write it into. Auditing today's corpus and finding it clean is
// a fact about today's FILES, not about the harness, and the harness is what
// hands every future leg its environment.
//
// So containment is established ONCE, by the process that spawns the legs,
// and inherited. After that a bare `tmux kill-server` from any leg destroys a
// server inside a scratch directory nobody else can name. The operator's
// requirement — "never kill the server wholesale EVER, it should just kill
// it's own sessions" — is met in the stronger form: wholesale and its-own
// become the same set, because no other server is spellable.
//
// # What this deliberately does NOT do
//
// It does not forbid `kill-server`. `scripts/test/tmux-fixture-socket-isolation.test.mjs`
// argues that the rule belongs on the socket NAME rather than on the teardown
// verb — a fixture that shares a server with a concurrent run is already
// broken before it tears anything down — and that argument is correct and
// untouched. This is the orthogonal layer: that guard makes fixtures name
// servers nobody else uses, and this one makes the whole run unable to reach
// servers it did not create, whatever it names them.
//
// It also does not remove the directory afterwards. Unlinking a socket file
// does not stop the server behind it; it makes it unreachable. Cleaning up at
// exit would trade one visible empty directory for an invisible orphan
// process, which is strictly worse. The residue is deliberate — see the plan.

import { chmodSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";

/** Distinctive enough that a stray directory can be traced back to this file. */
export const PRIVATE_TMUX_TMPDIR_PREFIX = "chief-guard-tmux-";

/**
 * Mint a private tmux namespace for one run.
 *
 * `mkdtemp` is already `0o700` by POSIX; the `chmod` is stated rather than
 * assumed because the mode is the half of this that keeps another user on a
 * shared box out, and a property nothing asserts is a property nobody
 * notices losing.
 */
export function createPrivateTmuxTmpdir(parent = tmpdir()) {
  const dir = mkdtempSync(join(parent, PRIVATE_TMUX_TMPDIR_PREFIX));
  chmodSync(dir, 0o700);
  return dir;
}

/**
 * True when a child given this environment would reach tmux's DEFAULT
 * namespace — `/tmp/tmux-<uid>/` — which is the operator's.
 *
 * Stated as the HAZARD rather than as its negation on purpose: the thing worth
 * naming in a test is the condition that hurt somebody. An unset value, an
 * empty string and a relative path all resolve there, and the empty string is
 * the one that reads as "set" to a careless check.
 */
export function reachesTheDefaultTmuxServer(env) {
  const configured = env?.TMUX_TMPDIR;
  return typeof configured !== "string" || configured === "" || !isAbsolute(configured);
}

/**
 * The environment a spawned leg gets: the caller's, with the namespace forced.
 *
 * FORCED, never defaulted. Honouring an ambient `TMUX_TMPDIR` would reintroduce
 * exactly the hazard `DECISIONS.md`'s 2026-08-18 entry names — safety that holds
 * only because some unrelated setting happens to have the right value, and is
 * "one unrelated setting away" from not holding. A caller who genuinely wants a
 * specific namespace passes it as `tmuxTmpdir`.
 *
 * Refuses rather than falling back. A fallback here would restore the ambient
 * environment silently, at the one moment the run most needs to stop.
 */
export function legEnvWithPrivateTmux(baseEnv, tmuxTmpdir) {
  if (typeof tmuxTmpdir !== "string" || tmuxTmpdir === "" || !isAbsolute(tmuxTmpdir)) {
    throw new Error(
      "REFUSING TO SPAWN: a private tmux namespace was not established for this run "
        + `(got ${JSON.stringify(tmuxTmpdir ?? null)}). Without an absolute TMUX_TMPDIR, an `
        + "unsocketed tmux command from any leg resolves to /tmp/tmux-<uid>/default — the "
        + "operator's own server.",
    );
  }
  return { ...withoutInheritedTmuxIdentity(baseEnv), TMUX_TMPDIR: tmuxTmpdir };
}

/** The variables that say WHOSE PANE launched this run, removed.
 *
 * The second defence, and the one a namespace alone does not give. Added after
 * the guard-harness containment shipped, when the widened hunt found that the
 * dangerous socket name never had to appear in a test at all:
 * `company.rs::boot_socket` has four tiers and tier 3 is the ambient `$TMUX`,
 * which is `<socket_path>,<pid>,<pane>` and whose basename inside an operator's
 * pane is literally `default`. Eight product call sites read it through
 * `boot_socket_from_env`, several of which then run destructive verbs.
 *
 * So a run launched from inside somebody's terminal inherited that person's
 * server as its answer. The pane identity of whoever started a test run is not
 * a fact about the run, and it is removed rather than overridden — there is no
 * correct value to substitute, and inventing one would be a fixture that forgot
 * to name its server, which is the failure mode this repo already refuses
 * elsewhere.
 *
 * Kept beside `legEnvWithPrivateTmux` and applied BY it, so the two halves
 * cannot drift: `scripts/with-private-tmux.sh` unsets exactly these two.
 */
export function withoutInheritedTmuxIdentity(baseEnv) {
  const { TMUX: _tmux, TMUX_PANE: _pane, ...rest } = { ...baseEnv };
  return rest;
}
