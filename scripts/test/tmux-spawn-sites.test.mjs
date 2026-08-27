// Every place production code can start a tmux PROCESS, and why each is
// allowed to.
//
// # Why this file exists
//
// The operator asked "why are we running tmux commands which is slow and boots
// a process? can't we use the socket api that's fast" — and the answer they
// were first given was half right. `ControlTransport` does hold one persistent
// `tmux -C` client, and a command over it costs under a millisecond against
// ~25ms for a spawn. But the claim that every per-command spawn caller was
// `#[cfg(test)]` came from one grep, and one grep is not a guarantee: it had
// already missed `attach.rs`, which mints rails through `tmux::run` in
// production, three spawns per company entry.
//
// A grep run once answers for the tree as it was that afternoon. A spawn added
// next month is invisible again, and the operator asks the same question again
// with the same answer being wrong in a new place.
//
// # The rule
//
// **A production tmux spawn happens only where it is named here, with the
// reason it cannot use the control client.** Not "spawns are banned" — three
// of them genuinely cannot go over a control client, and pretending otherwise
// would be a lie of a different kind. The rule is that the set is CLOSED, and
// every member states why it is a member.
//
// # What it checks
//
// The site list is DERIVED, never written down: the tree is walked, test code
// is removed by brace-matching each `#[cfg(test)]` block, and what remains is
// searched for anything that can start a tmux process. The SANCTIONED map
// below is the only hand-written part, and it holds reasons rather than
// evidence — so a stale entry is a wrong justification, not a missed site.
//
// Two directions, because a one-directional guard rots:
//
//   1. A site that is not sanctioned fails. That is the new-spawn case.
//   2. A sanctioned entry with no site left fails as STALE. That is the
//      case where somebody does the right thing — converts a spawn to the
//      transport — and the justification for it lingers, teaching the next
//      reader that a spawn there is normal.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

/** The operator-facing crate. Every tmux spawn in the product is in here. */
export const CRATE = join("apps", "chiefd", "crates", "chief-cli", "src");

/**
 * Every file allowed to start a tmux process in production, and WHY it cannot
 * use the control client instead.
 *
 * Reasons, not evidence. The sites themselves are derived below; this map only
 * answers "and that is legitimate because…".
 */
export const SANCTIONED = {
  "control.rs":
    "the control client's own attach. This IS the fast path — `tmux -C " +
    "attach-session` is how a persistent client comes to exist, so it cannot " +
    "be sent over one.",
  "actuate/runner.rs":
    "`SystemTmuxRunner`, the spawn transport the control transport falls back " +
    "to. Its whole purpose is to be the process-per-command path.",
  "preflight.rs":
    "the host capability probe. It runs before any company session exists, " +
    "and a control client can only attach to a session that is already there " +
    "— verified on tmux 3.3a: attaching with none answers `no sessions` and " +
    "creates nothing.",
  "tmux.rs":
    "the spawn primitive itself (`spawn_once`), plus the interactive handover " +
    "(`attach-session`/`switch-client`) which must inherit the operator's own " +
    "stdio and therefore cannot be a captured control-mode reply.",
  "attach.rs":
    "rail minting on company entry, over `tmux::run`. THIS ONE IS NOT A LAW " +
    "OF NATURE: the session is already present by the time it runs (the " +
    "`company.session.present` wait precedes it), so it could use the " +
    "transport and pay under a millisecond instead of ~25ms three times. It " +
    "is named here because it is real and was being missed, not because it " +
    "is right.",
};

/** Files whose name marks them as tests wherever they sit in the tree. */
const TEST_FILE = /(^|[/\\])tests?\.rs$/;

/** Anything that can put a tmux process on this machine. */
// The crate's own tmux helpers. A caller of these spawns whether or not it
// ever names a binary.
const TMUX_HELPERS = [/\btmux::run\(/, /\bsuper::run\(/, /\bspawn_once\(/];

/**
 * How far after a `Command::new(` to look for proof it is tmux being spawned.
 *
 * A tmux binary is not always a literal: `SystemTmuxRunner` and `ControlClient`
 * both spawn a CONFIGURED binary so a test can point them at a stub, so a
 * pattern that only knew `Command::new("tmux")` called the real spawn transport
 * clean — the stale-entry assertion caught that while this guard was being
 * written. Widening it to any `Command::new(binary)` then swept in `daemon.rs`
 * and `discovery.rs`, which spawn `chiefd` and `beacond`.
 *
 * `-L` is what separates them: it is tmux's socket flag, every tmux spawn in
 * this crate passes it, and nothing else here does. Proximity rather than
 * whole-file matching, so a file that spawns both tmux and something else is
 * judged per call rather than as a whole.
 */
const TMUX_PROOF = /\.arg\(\s*"-L"\s*\)/;
const PROOF_WINDOW = 600;

/** Every `.rs` file under `dir`, recursively. */
function rustFiles(dir) {
  const found = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      found.push(...rustFiles(full));
    } else if (entry.endsWith(".rs")) {
      found.push(full);
    }
  }
  return found;
}

/**
 * `source` with every `#[cfg(test)]` block removed.
 *
 * Brace-matched rather than "cut from the first `#[cfg(test)]` to the end of
 * the file", which is the shape these files happen to have today and would
 * silently blind this guard to any production code written below a test
 * module.
 */
export function withoutTests(source) {
  let out = "";
  let index = 0;
  for (;;) {
    const marker = source.indexOf("#[cfg(test)]", index);
    if (marker === -1) {
      out += source.slice(index);
      return out;
    }
    out += source.slice(index, marker);
    const open = source.indexOf("{", marker);
    const semicolon = source.indexOf(";", marker);

    // `#[cfg(test)] mod tests;` IS THE COMMON FORM IN THIS CRATE, and it has no
    // braces. Hunting for the next `{` regardless swallowed everything from the
    // declaration down to the end of whatever function came after it — which is
    // how the first draft of this guard excused a spawn injected into
    // `placement.rs` and reported itself green. A guard blind in the exact file
    // shape most of the crate uses is worse than no guard, because it is
    // believed.
    if (open === -1 || (semicolon !== -1 && semicolon < open)) {
      index = semicolon === -1 ? source.length : semicolon + 1;
      continue;
    }

    let depth = 0;
    let cursor = open;
    for (; cursor < source.length; cursor += 1) {
      if (source[cursor] === "{") depth += 1;
      else if (source[cursor] === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    index = cursor + 1;
  }
}

/** Comments removed, so prose about spawning is not read as spawning. */
function code(source) {
  return source.replace(/\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Does `body` start a tmux process anywhere in it? */
export function spawnsTmux(body) {
  if (TMUX_HELPERS.some((pattern) => pattern.test(body))) return true;
  const calls = /Command::new\(/g;
  for (let hit = calls.exec(body); hit !== null; hit = calls.exec(body)) {
    const window = body.slice(hit.index, hit.index + PROOF_WINDOW);
    if (/Command::new\(\s*"tmux"\s*\)/.test(window) || TMUX_PROOF.test(window)) return true;
  }
  return false;
}

/** Every production file under the crate that can start a tmux process. */
function spawnSites() {
  const root = join(repoRoot, CRATE);
  const sites = [];
  for (const file of rustFiles(root)) {
    const slug = relative(root, file).split(sep).join("/");
    if (TEST_FILE.test(slug)) continue;
    if (spawnsTmux(code(withoutTests(readFileSync(file, "utf8"))))) sites.push(slug);
  }
  return sites.sort();
}

test("every production tmux spawn site is sanctioned, with a reason", () => {
  const unsanctioned = spawnSites().filter((slug) => !(slug in SANCTIONED));
  assert.deepEqual(
    unsanctioned,
    [],
    "a new production tmux spawn appeared. Every command that CAN go over the control " +
      "client must, because a spawn costs ~25ms against under a millisecond — and the " +
      "operator has already asked once why tmux felt slow. If this site genuinely cannot " +
      "use the transport, add it to SANCTIONED with the reason; if it can, route it through " +
      "ControlTransport instead"
  );
});

test("a sanctioned entry whose spawn is gone is stale, not a silent pass", () => {
  const live = new Set(spawnSites());
  const gone = Object.keys(SANCTIONED).filter((slug) => !live.has(slug));
  assert.deepEqual(
    gone,
    [],
    "a file is excused for spawning tmux and no longer does. Delete the entry — a lingering " +
      "justification teaches the next reader that a spawn there is normal, which is exactly " +
      "how the set stops being closed"
  );
});

test("every sanctioned entry states a reason", () => {
  const unexplained = Object.entries(SANCTIONED)
    .filter(([, reason]) => typeof reason !== "string" || reason.trim().length < 40)
    .map(([slug]) => slug);
  assert.deepEqual(
    unexplained,
    [],
    "an allowlist without reasons is a list of things nobody may question. Each entry says " +
      "why the control client cannot carry it"
  );
});

test("RED: test-only code is not mistaken for a production spawn", () => {
  // The guard's own instrument. If `withoutTests` stopped removing test
  // blocks, every fixture that stands up a tmux server would read as a
  // production spawn and the first assertion would fail for the wrong reason
  // — or, worse, a real site could be excused by a stale entry it never
  // earned.
  const sample = [
    "fn production() { let x = 1; }",
    "#[cfg(test)]",
    "mod tests {",
    '    fn fixture() { Command::new("tmux"); }',
    "}",
    "fn after_the_tests() { let y = 2; }",
  ].join("\n");

  const stripped = withoutTests(sample);
  assert.ok(
    !stripped.includes('Command::new("tmux")'),
    `the test block survived stripping: ${stripped}`
  );
  assert.ok(
    stripped.includes("after_the_tests"),
    `production code BELOW a test module must survive, or the guard goes blind to it: ${stripped}`
  );
});

test("RED: a `mod tests;` declaration does not swallow the code after it", () => {
  // THE BUG THIS GUARD SHIPPED WITH, caught by injecting a real spawn and
  // watching it report green. `#[cfg(test)] mod tests;` has no braces, so
  // hunting for the next `{` consumed the whole function that followed —
  // and that declaration form is what most of this crate uses, so the guard
  // was blind precisely where it was most needed.
  const sample = [
    "#[cfg(test)]",
    "mod tests;",
    "",
    'fn after_the_declaration() { Command::new("tmux").arg("-L"); }',
  ].join("\n");

  const stripped = withoutTests(sample);
  assert.ok(
    stripped.includes("after_the_declaration"),
    `a brace-less test declaration must consume only itself: ${stripped}`
  );
  assert.ok(
    spawnsTmux(stripped),
    "and the spawn below it must still be seen — this is the assertion that was silently " +
      "false, and a guard that cannot fail is worse than none because it is believed"
  );
});
