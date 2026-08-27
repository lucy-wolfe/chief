// Every HTTP route `chief-cli` calls must be registered by `chiefd-api`.
//
// # The defect this closes, and why the compiler cannot
//
// `chief-cli` links NONE of the backend crates — `backend-tmux-boundary`
// enforces that in both directions — so a route is a STRING on one side and a
// `.route("...")` registration on the other, and nothing in the type system
// relates them. Deleting a route is therefore a change the compiler is happy
// with and the caller finds out about at runtime, as a 404 dressed up as
// whatever the caller does with a refusal.
//
// It is not hypothetical. `desired-state-only` deleted
// `POST /v1/org/runtime/actions`, and `chief-cli`'s `actuator_present()` and
// `runtime_observation()` went on POSTing it: `chief ls` would have rendered
// every company `unknown` (a failed read is not evidence either way — so the
// wrong answer arrives looking exactly like the honest one) and `chief attach`
// would have refused to start an actuator for a company nobody was actuating.
// Both surfaces fail QUIETLY, which is why a guard is worth more here than a
// louder error would be.
//
// # Scope: `/v1/org/*` only
//
// That prefix IS chiefd's company surface, and it is the whole of what the CLI
// asks another process for. The routes it does NOT check are the ones it must
// not: `/v1/company/*` and `/v1/founder/*` are served by `chief-cli`'s OWN
// router (`src/host/router.rs`) and by the Founder extension, so a scan that
// demanded chiefd register them would fail on a correct tree — a guard that
// cries wolf gets suppressed, and then it is not a guard.
//
// # What it does not check
//
// The METHOD and the BODY. Both sides are POST for everything but the health
// probe, and a body mismatch is a decode error the peer reports loudly with the
// field named — a different failure with a different shape. This guard is about
// the one that is silent.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** Where the caller's route strings live. */
const CLIENT_ROOT = "apps/chiefd/crates/chief-cli/src";
/** Where the router registers them. */
const ROUTER_ROOTS = [
  "apps/chiefd/crates/chiefd-api/src",
  "apps/chiefd/crates/beacond/src",
];

/** Every `.rs` file under `root`, recursively. */
function rustFiles(root) {
  const out = [];
  const walk = (directory) => {
    for (const entry of readdirSync(join(repoRoot, directory))) {
      const relativePath = join(directory, entry);
      if (statSync(join(repoRoot, relativePath)).isDirectory()) {
        walk(relativePath);
        continue;
      }
      if (entry.endsWith(".rs")) out.push(relativePath);
    }
  };
  walk(root);
  return out;
}

/**
 * Blank out `//` comments so a route named in a TOMBSTONE is not read as a
 * call. Tombstones name deleted routes ON PURPOSE — that is the whole point of
 * writing one — and a guard that could not tell a tombstone from a call would
 * make the honest record of a deletion into a failure.
 *
 * String contents are preserved: a `//` inside a URL (there are none today, but
 * a future absolute URL would have one) must not blank the rest of the line.
 */
function stripComments(source) {
  let out = "";
  let inString = false;
  for (let index = 0; index < source.length; index += 1) {
    const char = source[index];
    if (inString) {
      out += char;
      if (char === "\\") {
        out += source[index + 1] ?? "";
        index += 1;
      } else if (char === '"') {
        inString = false;
      }
      continue;
    }
    if (char === '"') {
      inString = true;
      out += char;
      continue;
    }
    if (char === "/" && source[index + 1] === "/") {
      while (index < source.length && source[index] !== "\n") index += 1;
      out += "\n";
      continue;
    }
    out += char;
  }
  return out;
}

const ROUTE_LITERAL = /"(\/v1\/org\/[a-z0-9\-/]*)"/g;

/** Route strings the CLI really calls, keyed by route -> the sites naming it. */
function clientRoutes() {
  const found = new Map();
  for (const file of rustFiles(CLIENT_ROOT)) {
    const source = stripComments(readFileSync(join(repoRoot, file), "utf8"));
    for (const [, route] of source.matchAll(ROUTE_LITERAL)) {
      if (!found.has(route)) found.set(route, []);
      found.get(route).push(relative(repoRoot, file));
    }
  }
  return found;
}

/** Routes the servers register. */
function registeredRoutes() {
  const found = new Set();
  for (const root of ROUTER_ROOTS) {
    for (const file of rustFiles(root)) {
      const source = stripComments(readFileSync(join(repoRoot, file), "utf8"));
      for (const [, route] of source.matchAll(ROUTE_LITERAL)) found.add(route);
    }
  }
  return found;
}

test("every route chief-cli calls is registered by a chiefd router", () => {
  const called = clientRoutes();
  const registered = registeredRoutes();

  const missing = [];
  for (const [route, sites] of called) {
    if (registered.has(route)) continue;
    missing.push(`${route} — called from ${[...new Set(sites)].join(", ")}`);
  }

  assert.deepEqual(
    missing,
    [],
    "these routes are POSTed by the operator client and registered by nobody. The two sides " +
      "share no types (chief-cli links no backend crate), so this is a 404 at runtime and a " +
      "clean build — and both surfaces that hit it fail QUIETLY: `chief ls` renders " +
      "`unknown`, which is what an honest failed read looks like, and `chief attach` " +
      "declines to start an actuator.\n  " +
      missing.join("\n  ")
  );
});

test("non-vacuity: both corpora are real, so the comparison can actually fail", () => {
  // A guard that scans nothing passes. Both halves are asserted, because
  // either one going empty produces the same green.
  const called = clientRoutes();
  const registered = registeredRoutes();

  assert.ok(
    called.size >= 10,
    `only ${called.size} route literals found in ${CLIENT_ROOT} — the client scan has stopped ` +
      `seeing its corpus and would pass by comparing nothing`
  );
  assert.ok(
    registered.size >= 40,
    `only ${registered.size} routes found across ${ROUTER_ROOTS.join(", ")} — the router scan ` +
      `has stopped seeing its corpus and would fail everything or nothing at random`
  );
  // The specific pair this guard was written for: the desired set is the one
  // route the actuator cannot work without.
  assert.ok(
    called.has("/v1/org/runtime/desired"),
    "the actuator's desired-set read is not among the client's routes — either it moved or " +
      "the scan is looking in the wrong place"
  );
});

test("negative self-test: a route the routers do not register is caught", () => {
  // The check, run over a doctored input. Without this, a broken matcher and a
  // correct tree are indistinguishable.
  const registered = registeredRoutes();
  assert.ok(
    !registered.has("/v1/org/runtime/actions"),
    "`/v1/org/runtime/actions` is deleted; a router that registers it again means this guard " +
      "is asserting against a stale expectation"
  );

  const doctored = new Map([["/v1/org/runtime/actions", ["doctored.rs"]]]);
  const missing = [...doctored.keys()].filter((route) => !registered.has(route));
  assert.deepEqual(missing, ["/v1/org/runtime/actions"]);
});

test("a route named only in a tombstone is not read as a call", () => {
  // Tombstones name deleted routes deliberately. If a comment counted, writing
  // an honest record of a deletion would break the build — which teaches people
  // to delete quietly.
  const source = stripComments(
    '// TOMBSTONE: `POST "/v1/org/runtime/actions"` is deleted.\nlet x = "/v1/org/runtime/desired";\n'
  );
  const routes = [...source.matchAll(ROUTE_LITERAL)].map(([, route]) => route);
  assert.deepEqual(routes, ["/v1/org/runtime/desired"]);
});
