// The minimum Pi version is written down ONCE, and every document that quotes
// it quotes the same number.
//
// # Why this file exists
//
// A 2026 ruling deleted every Pi version gate ("no minimum version"), and the
// `pinned_pi` tombstone in `chief-cli/src/preflight.rs` records it. The
// operator reversed that, narrowly: a FLOOR came back, not the pin. The pin
// asked "is this the one version we support" and had to be edited two or three
// times a week, which is how often Pi ships; the floor asks "is this at least
// old-enough" and passes every newer Pi for ever.
//
// A floor has the same failure mode every other compiled-in constant in this
// workspace has had. `beacond-port-single-definition.test.mjs`'s header records
// what a second copy of a port cost: an installed beacond bound the old address
// and was never found, and "the binary contained no occurrence of 6969 at all".
// A second copy of a version number costs the same way, and worse — one of the
// copies is a README, and the reader of a README cannot tell it has drifted.
//
// # What it checks
//
//   1. `MINIMUM_PI_VERSION` is defined exactly once in production Rust, in
//      `host-primitives/src/pi_floor.rs`.
//   2. The literal version string appears in NO other production Rust line.
//   3. Every document that states a minimum Pi version states THAT one.
//   4. The release packager reads the constant from the Rust source rather
//      than carrying its own copy, and its regex still resolves.
//
// Deliberately excluded, each for a stated reason:
//
//   * comments and doc comments — `pi_floor.rs`'s own module note and the
//     `pinned_pi` tombstone both narrate this rule on purpose, and a guard that
//     forbade naming the number in prose would delete the record of why the
//     rule exists;
//   * `#[cfg(test)]` modules — a test asserting the observable floor
//     legitimately writes the expected value out in full, which is what makes
//     it a test rather than a restatement of the code.
//
// Run with `node --test scripts/test/pi-floor-single-definition.test.mjs`.

import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const cratesRoot = join(repoRoot, "apps/chiefd/crates");

const PI_FLOOR_RS = "apps/chiefd/crates/host-primitives/src/pi_floor.rs";
const DEFINITION = /pub const MINIMUM_PI_VERSION: &str = "(\d+\.\d+\.\d+)";/;

/** The packager that stamps the floor into a release manifest. */
const PACKAGER = "scripts/package-release.ts";

/**
 * The shared emitter behind both `bun run release` and the packager. It owns
 * `piFloor()` and `assembleVersionTree()`, so it is where the floor is actually
 * parsed out of the Rust source; the packager reaches it through this file.
 */
const RELEASE_EMITTER = "scripts/release-chiefd.ts";

/**
 * Documents that are ALLOWED to state a Pi version, and must state this one.
 *
 * A closed list rather than a sweep of every markdown file: `plans/` and
 * `CHANGELOG.md` legitimately quote whatever the floor was on the day they were
 * written, and rewriting history to match a moved constant is the opposite of
 * what those files are for.
 */
const DOCUMENTS = ["README.md", "CONTRIBUTING.md", "docs/OPERATING.md"];

/** Any dotted numeric triple, which is what a Pi version looks like. */
const ANY_TRIPLE = /\b\d+\.\d+\.\d+\b/g;

function rustFiles(dir, collected = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "target") continue;
      rustFiles(path, collected);
    } else if (entry.name.endsWith(".rs")) {
      collected.push(path);
    }
  }
  return collected;
}

/**
 * Production lines: comments and `#[cfg(test)]` modules removed.
 *
 * Same assumption `beacond-port-single-definition.test.mjs` states — this
 * repo's Rust convention keeps `#[cfg(test)] mod …` last in a file.
 */
function productionLines(text) {
  const [production] = text.split(/^#\[cfg\(test\)\]/m);
  return production
    .split("\n")
    .map((line, index) => ({ line, number: index + 1 }))
    .filter(({ line }) => !/^\s*(\/\/|\*|\/\*)/.test(line));
}

function declaredFloor() {
  const source = readFileSync(join(repoRoot, PI_FLOOR_RS), "utf8");
  const match = source.match(DEFINITION);
  assert.ok(match, `${PI_FLOOR_RS} must define MINIMUM_PI_VERSION as a dotted triple`);
  return match[1];
}

test("the minimum Pi version has exactly one definition in production Rust", () => {
  const floor = declaredFloor();
  const occurrences = [];
  for (const file of rustFiles(cratesRoot)) {
    const path = relative(repoRoot, file);
    for (const { line, number } of productionLines(readFileSync(file, "utf8"))) {
      if (line.includes(floor)) occurrences.push(`${path}:${number}: ${line.trim()}`);
    }
  }

  assert.deepEqual(
    occurrences.map((occurrence) => occurrence.split(":")[0]),
    [PI_FLOOR_RS],
    `the Pi floor belongs in ${PI_FLOOR_RS} and nowhere else:\n  ${occurrences.join("\n  ")}`
  );
  assert.equal(
    occurrences.length,
    1,
    `one definition, not ${occurrences.length}:\n  ${occurrences.join("\n  ")}`
  );
});

test("every consumer reads the constant instead of restating it", () => {
  const floor = declaredFloor();
  const consumer = "apps/chiefd/crates/chief-cli/src/preflight.rs";
  const production = productionLines(readFileSync(join(repoRoot, consumer), "utf8"))
    .map(({ line }) => line)
    .join("\n");
  assert.match(
    production,
    /host_primitives::pi_floor::/,
    `${consumer} must read host_primitives::pi_floor, not compile in its own number`
  );
  assert.ok(
    !production.includes(floor),
    `${consumer} must not restate the floor literally`
  );
});

test("every document that states a minimum Pi version states the declared one", () => {
  const floor = declaredFloor();
  const wrong = [];
  let mentions = 0;
  for (const document of DOCUMENTS) {
    const path = join(repoRoot, document);
    if (!existsSync(path)) continue;
    for (const [index, line] of readFileSync(path, "utf8").split("\n").entries()) {
      if (!/\bPi\b/.test(line)) continue;
      for (const triple of line.match(ANY_TRIPLE) ?? []) {
        mentions += 1;
        if (triple !== floor) wrong.push(`${document}:${index + 1}: ${line.trim()}`);
      }
    }
  }
  // NON-VACUOUSNESS. A guard that finds nothing to check is not passing, it is
  // blind — and this one goes blind the moment somebody rewords the README
  // sentence that carries the number.
  assert.ok(
    mentions > 0,
    `no document in ${DOCUMENTS.join(", ")} states a Pi version at all. Either the quick start ` +
      "stopped naming the floor, or the sentence it lived in was reworded past this check."
  );
  assert.deepEqual(
    wrong,
    [],
    `these lines state a Pi version that is not the declared floor (${floor}):\n  ${wrong.join("\n  ")}`
  );
});

test("the release machinery reads the floor out of the Rust source", () => {
  // The floor is parsed once, in the shared emitter's `piFloor()`, and the
  // packager reaches it through `assembleVersionTree`. So the emitter must read
  // `pi_floor.rs`, and NEITHER script may carry its own copy of the number.
  const emitterPath = join(repoRoot, RELEASE_EMITTER);
  assert.ok(existsSync(emitterPath), `${RELEASE_EMITTER} must exist`);
  const emitter = readFileSync(emitterPath, "utf8");
  assert.ok(
    emitter.includes("pi_floor.rs"),
    `${RELEASE_EMITTER} must read ${PI_FLOOR_RS} rather than carrying its own copy of the floor`
  );
  const floor = declaredFloor();
  assert.ok(
    !emitter.includes(floor),
    `${RELEASE_EMITTER} must not restate the floor literally — parse it out of the Rust source`
  );
  const packagerPath = join(repoRoot, PACKAGER);
  if (existsSync(packagerPath)) {
    assert.ok(
      !readFileSync(packagerPath, "utf8").includes(floor),
      `${PACKAGER} must not restate the floor literally — it flows from ${RELEASE_EMITTER}`
    );
  }
});
