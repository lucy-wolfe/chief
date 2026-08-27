// A guard run can never reach the operator's tmux server.
//
// # The incident
//
// `bun run test:pre-push-guards` destroyed live tmux sessions belonging to
// several people on a shared box.
//
// `scripts/gate-matrix-legs.mjs` spawned every leg with
// `spawnSync(cmd, args, { encoding, cwd, maxBuffer })` — no `env` key. Each leg
// inherited the ambient environment, in which `TMUX_TMPDIR` is unset on an
// ordinary box, so any `tmux` command without `-L` resolved to
// `/tmp/tmux-<uid>/default`: the operator's own server.
//
// # What this file pins, and why each half is here
//
// The RULE (`scripts/private-tmux-tmpdir.mjs`) and the CALL SITE that must
// apply it. Both, because they fail independently and in opposite ways:
//
//   * a correct helper nobody calls protects nothing, and reads as protection —
//     the exact shape this repo has been bitten by before (a CI-wired guard
//     nobody runs produces the same outcome as a broken one);
//   * a call site that passes SOMETHING is not evidence it passes something
//     SAFE.
//
// So the rule is tested by calling it with the values that hurt, and the call
// site is tested both behaviourally (`legSpawnOptions` is handed a bad
// namespace and asked what it does) and statically (the driver's one
// `spawnSync` really does route through it).
//
// # Every static check here has a negative control
//
// A static check that can no longer see what it forbids is worse than none: it
// keeps reporting green after the thing it reads has been renamed out from
// under it. So each matcher is also run against a deliberately BROKEN copy of
// the same source, and must report the violation. That is the difference
// between a test that passes and a test that works.
//
// # Deliberately NOT a rule about `kill-server`
//
// `scripts/test/tmux-fixture-socket-isolation.test.mjs` argues the rule belongs
// on the socket NAME rather than the teardown verb, and it is right — a fixture
// sharing a server with a concurrent run is already broken before it tears
// anything down. This is the orthogonal layer: that guard makes fixtures name
// servers nobody else uses; this one makes the whole run unable to REACH a
// server it did not create, whatever it names.
//
// Run with `node --test scripts/test/guard-run-tmux-isolation.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { legSpawnOptions } from "../gate-matrix-legs.mjs";
import {
  PRIVATE_TMUX_TMPDIR_PREFIX,
  createPrivateTmuxTmpdir,
  legEnvWithPrivateTmux,
  reachesTheDefaultTmuxServer,
  withoutInheritedTmuxIdentity,
} from "../private-tmux-tmpdir.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const DRIVER = join(repoRoot, "scripts", "gate-matrix-legs.mjs");
const CARGO_GATE = join(repoRoot, "scripts", "cargo-test-workspace.sh");
const WRAPPER = join(repoRoot, "scripts", "with-private-tmux.sh");
const CHIEF_CLI = join(repoRoot, "apps", "chiefd", "crates", "chief-cli", "src");

/** Namespaces this file minted, removed at the end. Safe to remove precisely
 * because no test here starts a tmux server in one — see the module's own note
 * on why the REAL run leaves its directory behind. */
const minted = [];

test.after(() => {
  for (const dir of minted) rmSync(dir, { recursive: true, force: true });
});

test("a minted namespace is private, per-run, and not tmux's default", () => {
  const first = createPrivateTmuxTmpdir();
  const second = createPrivateTmuxTmpdir();
  minted.push(first, second);

  assert.notEqual(first, second, "two runs must not share one namespace");
  for (const dir of [first, second]) {
    assert.ok(dir.startsWith(`${tmpdir()}${sep}`), `${dir} must live under the system temp root`);
    assert.ok(
      basename(dir).startsWith(PRIVATE_TMUX_TMPDIR_PREFIX),
      `${dir} must be traceable back to the file that minted it`,
    );
    assert.equal(
      statSync(dir).mode & 0o777,
      0o700,
      "another user on a shared box must not be able to enter this namespace",
    );
    assert.notEqual(dir, "/tmp", "the default namespace is the thing being escaped, not reused");
  }
});

test("the hazard is named as a hazard: what still reaches /tmp/tmux-<uid>", () => {
  // The empty string is the interesting one — it reads as "set" to any check
  // that only asks whether the key exists.
  for (const env of [undefined, {}, { TMUX_TMPDIR: "" }, { TMUX_TMPDIR: "tmux" }, { TMUX_TMPDIR: "." }]) {
    assert.equal(
      reachesTheDefaultTmuxServer(env),
      true,
      `${JSON.stringify(env ?? null)} resolves to the operator's namespace and must be reported as such`,
    );
  }
  assert.equal(reachesTheDefaultTmuxServer({ TMUX_TMPDIR: "/var/folders/x/private" }), false);
});

test("the namespace is FORCED, never merged from the ambient environment", () => {
  const dir = createPrivateTmuxTmpdir();
  minted.push(dir);

  // An operator running the guards from inside their own tmux carries a
  // TMUX_TMPDIR that points AT the server we are protecting. Honouring it would
  // be the whole bug, politely spelled.
  const env = legEnvWithPrivateTmux({ PATH: "/usr/bin", TMUX_TMPDIR: "/tmp" }, dir);
  assert.equal(env.TMUX_TMPDIR, dir, "the ambient value must be overridden, not preferred");
  assert.equal(env.PATH, "/usr/bin", "every other variable is passed through untouched");
  assert.equal(reachesTheDefaultTmuxServer(env), false);
});

test("an unestablished namespace REFUSES rather than falling back", () => {
  // A fallback here would silently restore the ambient environment at the one
  // moment the run most needs to stop.
  for (const bad of [undefined, null, "", "relative/dir", 7]) {
    assert.throws(
      () => legEnvWithPrivateTmux({ PATH: "/usr/bin" }, bad),
      /REFUSING TO SPAWN/,
      `${JSON.stringify(bad ?? null)} is not a namespace and must not be treated as one`,
    );
  }
  assert.throws(() => legEnvWithPrivateTmux({}, ""), /operator's own server/);
});

test("every leg the driver spawns is handed the private namespace", () => {
  const dir = createPrivateTmuxTmpdir();
  minted.push(dir);

  const options = legSpawnOptions("/repo", dir, { PATH: "/usr/bin" });
  assert.equal(options.env.TMUX_TMPDIR, dir);
  assert.equal(reachesTheDefaultTmuxServer(options.env), false);
  assert.equal(options.cwd, "/repo", "the existing spawn contract is unchanged");
  assert.equal(options.encoding, "utf8");
  assert.equal(options.maxBuffer, 1024 * 1024 * 64);

  // The refusal reaches the driver, so a future edit that drops the namespace
  // stops the run instead of quietly widening it back to the whole box.
  assert.throws(() => legSpawnOptions("/repo", undefined, {}), /REFUSING TO SPAWN/);
});

/**
 * Every `spawnSync(` in the driver that does NOT route its options through
 * `legSpawnOptions`, by line number.
 *
 * A window rather than a parse: this driver has one spawn site and it is one
 * statement. The negative control below is what keeps the window honest.
 */
function spawnSitesMissingContainment(source) {
  const offenders = [];
  const needle = "spawnSync(";
  for (let at = source.indexOf(needle); at !== -1; at = source.indexOf(needle, at + needle.length)) {
    if (!source.slice(at, at + 240).includes("legSpawnOptions(")) {
      offenders.push(source.slice(0, at).split("\n").length);
    }
  }
  return offenders;
}

test("the driver's real source routes every spawn through the containment", () => {
  const source = readFileSync(DRIVER, "utf8");
  assert.ok(source.includes("spawnSync("), "no spawn site found — this check would be vacuous");
  assert.deepEqual(
    spawnSitesMissingContainment(source),
    [],
    "a leg is spawned without the private tmux namespace",
  );
  assert.ok(
    source.includes("createPrivateTmuxTmpdir()"),
    "the driver must mint a namespace for the run, not expect one from the environment",
  );
});

test("NEGATIVE CONTROL: the spawn-site check sees the exact regression it exists for", () => {
  // Character-for-character the options object the driver carried on the day it
  // destroyed the operator's sessions.
  const broken = readFileSync(DRIVER, "utf8").replace(
    "legSpawnOptions(root, tmuxTmpdir)",
    '{ encoding: "utf8", cwd: root, maxBuffer: 1024 * 1024 * 64 }',
  );
  assert.notEqual(broken, readFileSync(DRIVER, "utf8"), "the mutation must actually apply");
  assert.equal(
    spawnSitesMissingContainment(broken).length,
    1,
    "the check must FAIL on the pre-fix driver, or it is not measuring anything",
  );
});

/**
 * Where the cargo gate hands off to the one wrapper.
 *
 * The rule CHANGED with the wrapper and the test changed with it, deliberately
 * and not by weakening: this script used to carry its own copy of the mint/
 * chmod/export block, and a rule that lives in two places rots in two places.
 * The property being pinned is the SAME one — no path out of this script
 * escapes containment — asserted against delegation instead of against an
 * inline copy.
 *
 * The ordering is still the whole point. `cargo-test-workspace.sh` `exec`s a
 * shard script when CI supplies the matrix variables, so a hand-off placed
 * after that line would protect the local path only, leaving every CI shard
 * uncontained.
 *
 * Line-anchored and comment-excluding. An `indexOf` over the whole text read
 * this script's own header — "THE sanctioned wrapper for `cargo test
 * --workspace`" on line 2 — as the command itself, and a matcher that cannot
 * tell a command from a sentence ABOUT the command reports faults that are not
 * there and misses ones that are.
 */
function cargoGateContainment(source) {
  const at = (needle) => {
    const lines = source.split("\n");
    return lines.findIndex((line) => {
      const text = line.trimStart();
      return !text.startsWith("#") && text.includes(needle);
    });
  };
  const delegates = at("scripts/with-private-tmux.sh");
  const shardExec = at("cargo-test-workspace-shard.sh");
  const cargo = at("cargo test --workspace");
  return {
    delegates: delegates !== -1,
    beforeShardExec: delegates !== -1 && shardExec !== -1 && delegates < shardExec,
    beforeCargo: delegates !== -1 && cargo !== -1 && delegates < cargo,
    // A second copy of the wrapper's own logic is the thing being removed.
    keepsNoSecondCopy: !source.includes('chmod 700 "$TMUX_TMPDIR"'),
  };
}

test("the Rust lane reaches cargo only through the wrapper, on every path", () => {
  // This lane drives real tmux and calls `kill-server` in eleven places, and
  // the socket a product path resolves can arrive from the ambient `$TMUX`
  // rather than from any fixture.
  const state = cargoGateContainment(readFileSync(CARGO_GATE, "utf8"));
  assert.equal(state.delegates, true, "the cargo gate must hand off to the one wrapper");
  assert.equal(state.beforeShardExec, true, "the CI shard path must not escape containment");
  assert.equal(state.beforeCargo, true, "cargo must run already contained");
  assert.equal(state.keepsNoSecondCopy, true, "the wrapper's logic must live in exactly one file");
});

test("NEGATIVE CONTROL: the cargo-gate check sees an escape, an omission, and a comment", () => {
  const real = readFileSync(CARGO_GATE, "utf8");

  // Omission: no hand-off at all — the state before this packet.
  // `replaceAll`, not `replace`: the script names the wrapper twice, once in a
  // comment that explains the mechanism and once in the line that does the
  // work, and a single-occurrence replace edits the COMMENT and leaves the
  // delegation standing. The control then "passes" against an unchanged
  // script, which is precisely the dead-control failure these blocks exist to
  // prevent.
  assert.equal(
    cargoGateContainment(real.replaceAll("scripts/with-private-tmux.sh", "scripts/unrelated.sh"))
      .delegates,
    false,
  );

  // Escape: contained, but only after the shard `exec` — every CI shard runs
  // on the operator's namespace while the local path looks protected.
  const late = "#!/usr/bin/env bash\nexec bash cargo-test-workspace-shard.sh\n"
    + 'exec bash "$ROOT/scripts/with-private-tmux.sh" bash "$0"\ncargo test --workspace\n';
  const lateState = cargoGateContainment(late);
  assert.equal(lateState.delegates, true, "the fixture does delegate");
  assert.equal(lateState.beforeShardExec, false, "...but too late for the shard path");

  // A sentence ABOUT the command is not the command.
  const commented = "#!/usr/bin/env bash\n# wrapper for `cargo test --workspace` (#857)\n"
    + 'exec bash "$ROOT/scripts/with-private-tmux.sh" bash "$0"\n'
    + "exec bash cargo-test-workspace-shard.sh\ncargo test --workspace --locked\n";
  assert.equal(cargoGateContainment(commented).beforeCargo, true, "a commented mention is not the command");
});

/** What the one wrapper must do, read from the wrapper itself. */
function wrapperRules(source) {
  return {
    mints: source.includes('TMUX_TMPDIR="$(mktemp -d'),
    exports: source.includes("export TMUX_TMPDIR"),
    locksMode: source.includes('chmod 700 "$TMUX_TMPDIR"'),
    // The half a namespace alone does not give — see the module docs.
    unsetsTmux: /^\s*unset TMUX$/m.test(source),
    unsetsPane: /^\s*unset TMUX_PANE$/m.test(source),
    execs: /^\s*exec "\$@"$/m.test(source),
    refusesEmpty: source.includes('if [ "$#" -eq 0 ]'),
  };
}

test("the wrapper mints a namespace AND drops the launcher's pane identity", () => {
  const rules = wrapperRules(readFileSync(WRAPPER, "utf8"));
  for (const [rule, held] of Object.entries(rules)) {
    assert.equal(held, true, `the wrapper must satisfy: ${rule}`);
  }
});

test("NEGATIVE CONTROL: the wrapper check sees each rule dropped", () => {
  const real = readFileSync(WRAPPER, "utf8");
  assert.equal(wrapperRules(real.replace("\nunset TMUX\n", "\n")).unsetsTmux, false);
  assert.equal(wrapperRules(real.replace("unset TMUX_PANE", "true")).unsetsPane, false);
  assert.equal(wrapperRules(real.replace('chmod 700 "$TMUX_TMPDIR"', "true")).locksMode, false);
  assert.equal(wrapperRules(real.replace('exec "$@"', '"$@"')).execs, false);
});

test("the JS half strips exactly the two variables the shell half unsets", () => {
  const stripped = withoutInheritedTmuxIdentity({
    PATH: "/usr/bin",
    TMUX: "/tmp/tmux-0/default,123,0",
    TMUX_PANE: "%4",
  });
  assert.equal("TMUX" in stripped, false, "the launcher's server must not be inherited");
  assert.equal("TMUX_PANE" in stripped, false);
  assert.equal(stripped.PATH, "/usr/bin", "everything else survives");

  // And the leg environment applies it, so the harness and the wrapper agree.
  const dir = createPrivateTmuxTmpdir();
  minted.push(dir);
  const env = legEnvWithPrivateTmux({ PATH: "/usr/bin", TMUX: "/tmp/tmux-0/default,1,0" }, dir);
  assert.equal("TMUX" in env, false, "a leg must not inherit whose pane launched the run");
  assert.equal(env.TMUX_TMPDIR, dir);
});

test("the vitest lanes run contained too", () => {
  const scripts = JSON.parse(readFileSync(join(repoRoot, "package.json"), "utf8")).scripts;
  assert.match(
    scripts.test,
    /with-private-tmux\.sh/,
    "`bun run test` reaches TmuxHostedCompanyDaemon and must be contained",
  );
});

/**
 * Every `TMUX`-mutating test site in `chief-cli` that does NOT hold the lock.
 *
 * THE defect this closes, and it is not hygiene: `company.rs::boot_socket`
 * derives the tmux socket from `$TMUX`, whose basename inside an operator's
 * pane is literally `default`. A test that installs a `default`-shaped `$TMUX`
 * therefore races every concurrent test that resolves a socket — and libtest is
 * multi-threaded, while the `cli` CI shard runs these binaries twice at once.
 *
 * Three sites mutated it; each carried a comment asserting `SAFETY:
 * single-threaded test`, which was false; and only one took a lock, a private
 * one no other module could reach. A comment is not a lock.
 *
 * Scoped by enclosing `fn`, because the lock guard is a statement in the same
 * function body.
 */
function unlockedEnvMutations(source) {
  const offenders = [];
  const bodies = source.split(/\n    (?=\/\/\/|#\[|pub |fn )/);
  for (const body of bodies) {
    const mutates = /(?:set_var|remove_var)\("TMUX"/.test(body);
    if (mutates && !body.includes("env_lock()")) offenders.push(body.slice(0, 80));
  }
  return offenders;
}

/** Every `.rs` file under `chief-cli/src`, derived rather than listed —
 * a maintained list of files-that-touch-$TMUX would go stale the first time
 * somebody added a third one, which is the whole failure mode this guard is
 * about. */
function rustSources() {
  return readdirSync(CHIEF_CLI, { recursive: true })
    .map(String)
    .filter((name) => name.endsWith(".rs"))
    .map((name) => ({ name, source: readFileSync(join(CHIEF_CLI, name), "utf8") }));
}

test("every TMUX-mutating test in chief-cli holds the crate's one lock", () => {
  const mutators = rustSources().filter(({ source }) => /(?:set_var|remove_var)\("TMUX"/.test(source));
  assert.ok(mutators.length > 0, "no $TMUX mutation found anywhere — this check would be vacuous");
  for (const { name, source } of mutators) {
    assert.deepEqual(unlockedEnvMutations(source), [], `${name} mutates $TMUX without the lock`);
  }
});

test("the crate has exactly ONE env lock, and it is not per-module", () => {
  // Two modules holding two different mutexes are not excluding each other,
  // which is exactly what the previous arrangement did: `company.rs` had a
  // private lock with one caller while `tmux.rs` took none at all.
  const definitions = rustSources()
    .filter(({ source }) => source.includes("fn env_lock()"))
    .map(({ name }) => name);
  assert.deepEqual(definitions, ["tmux.rs"], "one definition, in the shared test-support module");
});

test("NEGATIVE CONTROL: the lock check sees an unlocked mutation", () => {
  const unlocked = [
    "mod tests {",
    "    #[test]",
    "    fn a_test_that_forgot_the_lock() {",
    '        std::env::set_var("TMUX", "/tmp/tmux-0/default,1,0");',
    "    }",
    "",
    "    #[test]",
    "    fn a_test_that_took_it() {",
    "        let _guard = super::test_support::env_lock();",
    '        std::env::remove_var("TMUX");',
    "    }",
    "}",
  ].join("\n");
  const offenders = unlockedEnvMutations(unlocked);
  assert.equal(offenders.length, 1, "exactly the unlocked one must be reported");
  assert.match(offenders[0], /forgot_the_lock/);
});
