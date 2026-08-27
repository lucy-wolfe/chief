// The discovery port is written down ONCE, in Rust, the way it already is in
// TypeScript.
//
// # Why this file exists
//
// `packages/chiefing/test/PublicSurface.test.ts` has held the TypeScript side
// to exactly one compiled-in beacond address since discovery landed ("6969/
// DEFAULT_BEACOND_URL is compiled in exactly once, in Company.ts"). The Rust
// side had no equivalent and had drifted to three independent definitions:
//
//   * `crates/beacond/src/config.rs`'s `DEFAULT_BIND` (plus a second copy in
//     its own `USAGE` environment table);
//   * `crates/chiefd-daemon/src/beacon.rs`'s private `DEFAULT_BEACOND_URL`;
//   * `crates/chief-cli/src/discovery.rs`'s private
//     `DEFAULT_BEACOND_URL` — a SECOND private const in the SAME crate.
//
// The cost is already documented in the tree rather than hypothetical.
// `lifecycle/discovery.rs`'s `unreachable_beacond_detail` exists, with its own
// unit test, because an INSTALLED beacond that predated a port move started
// perfectly, bound the old address and was never found: "the binary contained
// no occurrence of `6969` at all". That is what a copy of a port costs, and
// three copies is three chances to repeat it.
//
// # What it checks
//
// Exactly one occurrence of the literal port in PRODUCTION Rust, and that the
// one occurrence is in beacond's own config. Deliberately excluded, each for a
// stated reason:
//
//   * comments and doc comments — several narrate the incident above ON
//     PURPOSE (`discovery.rs`'s "beacond moved to a static 6969", `host.rs`'s
//     "deliberately BELOW beacond's 6969"), and a guard that forbade naming a
//     port in prose would delete the only record of why this rule exists;
//   * `#[cfg(test)]` modules — a test asserting the OBSERVABLE default
//     legitimately writes the expected address out in full, which is what
//     makes it a test rather than a restatement of the code.
//
// Run with `node --test scripts/test/beacond-port-single-definition.test.mjs`.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const cratesRoot = join(repoRoot, "apps/chiefd/crates");

/** The port itself, read from its one definition rather than retyped here. */
const CONFIG_RS = "apps/chiefd/crates/beacond/src/config.rs";
const DEFINITION = /pub const DEFAULT_BIND: &str = "127\.0\.0\.1:(\d+)";/;

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
 * This repo's Rust convention keeps `#[cfg(test)] mod …` as the last item in a
 * file (the same assumption `organization-revision-tripwire.test.mjs` makes
 * and states), so everything before the first such attribute is production.
 */
function productionLines(text) {
  const [production] = text.split(/^#\[cfg\(test\)\]/m);
  return production
    .split("\n")
    .map((line, index) => ({ line, number: index + 1 }))
    .filter(({ line }) => !/^\s*(\/\/|\*|\/\*)/.test(line));
}

test("the discovery port has exactly one definition in production Rust", () => {
  const configSource = readFileSync(join(repoRoot, CONFIG_RS), "utf8");
  const definition = configSource.match(DEFINITION);
  assert.ok(definition, `${CONFIG_RS} must define DEFAULT_BIND as a loopback address`);
  const port = definition[1];

  const occurrences = [];
  for (const file of rustFiles(cratesRoot)) {
    const path = relative(repoRoot, file);
    for (const { line, number } of productionLines(readFileSync(file, "utf8"))) {
      if (line.includes(port)) occurrences.push(`${path}:${number}: ${line.trim()}`);
    }
  }

  assert.deepEqual(
    occurrences.map((occurrence) => occurrence.split(":")[0]),
    [CONFIG_RS],
    `the discovery port belongs in ${CONFIG_RS} and nowhere else:\n  ${occurrences.join("\n  ")}`
  );
  assert.equal(
    occurrences.length,
    1,
    `one definition, not ${occurrences.length}:\n  ${occurrences.join("\n  ")}`
  );
});

test("every consumer reads that definition instead of restating it", () => {
  // The two sites that each carried their own copy. Naming them is the point:
  // this is the regression proof, so a reintroduced private const fails here
  // by name rather than only through the count above.
  for (const consumer of [
    "apps/chiefd/crates/chiefd-daemon/src/beacon.rs",
    "apps/chiefd/crates/chief-cli/src/discovery.rs",
  ]) {
    const source = readFileSync(join(repoRoot, consumer), "utf8");
    const production = productionLines(source)
      .map(({ line }) => line)
      .join("\n");
    assert.ok(
      !/const\s+DEFAULT_BEACOND_URL/.test(production),
      `${consumer} must read beacond::config, not compile in its own default`
    );
    assert.match(
      production,
      /beacond::config::default_url/,
      `${consumer} must resolve its default through beacond::config::default_url()`
    );
  }

  // And chiefd must DECLARE the dependency it now reads through — a
  // `[dev-dependencies]`-only entry would compile the tests and break the
  // binary.
  const manifest = readFileSync(join(repoRoot, "apps/chiefd/crates/chiefd-daemon/Cargo.toml"), "utf8");
  const [dependencies] = manifest.split(/^\[dev-dependencies\]/m);
  assert.match(
    dependencies,
    /^beacond = \{ path = "\.\.\/beacond" \}$/m,
    "chiefd must declare beacond as a production dependency"
  );
});

test("beacond's own usage text quotes the definition rather than a second copy", () => {
  // The fourth copy, and the one an operator actually reads: `beacond --help`
  // printed the address from a hand-written line in a `concat!`. A test
  // asserting two literals agree is a slower way to have one literal.
  const configSource = readFileSync(join(repoRoot, CONFIG_RS), "utf8");
  const [production] = configSource.split(/^#\[cfg\(test\)\]/m);
  assert.match(production, /pub fn usage\(\) -> String/, "usage must be built, not stored");
  assert.match(production, /\{default_bind\}/, "usage must interpolate DEFAULT_BIND");
});
