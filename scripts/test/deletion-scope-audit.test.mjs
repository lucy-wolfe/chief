// #919: proves scripts/deletion-scope-audit.mjs actually derives the real
// reference set (never a hand-typed list) and that checkAgainstDeclared
// actually fires on an under-named list -- the demonstrated-red standard
// this repo requires (#873's own "a check that cannot fail proves nothing").

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  checkAgainstDeclared,
  deriveApiSurfaceReferences,
  deriveApiSurfaceStrings,
  deriveReferences,
  searchApiSurfaceStrings,
} from "../deletion-scope-audit.mjs";

/** A real (minimal) workspace shape -- root package.json with `workspaces`,
 *  one member with its OWN tsconfig declaring `@/*` -> `./src/*`, mirroring
 *  `packages/testing/tsconfig.json`'s real convention exactly. Required
 *  because #919's own first live use found this tool blind to that alias
 *  style: a synthetic fixture with no tsconfig at all could never have
 *  caught it, so this fixture is deliberately shaped like the real repo,
 *  not a simplified stand-in for it. */
function fixture() {
  const root = mkdtempSync(join(tmpdir(), "deletion-scope-audit-"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "fixture-root", workspaces: ["packages/*"] }));
  writeFileSync(join(root, "tsconfig.base.json"), JSON.stringify({ compilerOptions: {} }));

  const memberDir = join(root, "packages", "widget");
  mkdirSync(join(memberDir, "src"), { recursive: true });
  mkdirSync(join(memberDir, "test"), { recursive: true });
  mkdirSync(join(memberDir, "docs"), { recursive: true });
  writeFileSync(join(memberDir, "package.json"), JSON.stringify({ name: "@fixture/widget" }));
  writeFileSync(
    join(memberDir, "tsconfig.json"),
    JSON.stringify({
      compilerOptions: { baseUrl: ".", paths: { "@/*": ["./src/*"] }, moduleResolution: "bundler" },
      include: ["src/**/*.ts", "test/**/*.ts"],
    }),
  );

  writeFileSync(join(memberDir, "src", "widget-registry.ts"), "export const widgetRegistry = 1\n");
  // Real load-bearing importer, RELATIVE style.
  writeFileSync(
    join(memberDir, "src", "consumer.ts"),
    "import { widgetRegistry } from './widget-registry'\nexport { widgetRegistry }\n",
  );
  // Real load-bearing importer, from a sibling test dir with a deeper relative path.
  writeFileSync(
    join(memberDir, "test", "WidgetRegistry.test.ts"),
    "import { widgetRegistry } from '../src/widget-registry'\nconsole.log(widgetRegistry)\n",
  );
  // Real load-bearing importer, ALIAS style -- the exact #919 boundary found
  // live: `packages/testing/src/index.ts` importing `ChiefdBinary.ts` via
  // `@/ChiefdBinary`, filed as a non-hit by the pre-fix relative-only scan.
  writeFileSync(join(memberDir, "src", "index.ts"), "export { widgetRegistry } from '@/widget-registry'\n");
  // A string that merely LOOKS like an import of it -- must never be treated as one.
  writeFileSync(
    join(memberDir, "test", "Unrelated.test.ts"),
    "const description = \"imports from './widget-registry', kind of\"\nconsole.log(description)\n",
  );
  // A plain-text mention -- informational, not load-bearing.
  writeFileSync(join(memberDir, "docs", "NOTES.md"), "See widget-registry for the old design.\n");
  // A substring-only hit (the stem glued inside a longer word, no word
  // boundary on either side) -- must be filtered and stated, not silently
  // bucketed into either load-bearing or informational.
  writeFileSync(join(memberDir, "docs", "OTHER.md"), "prewidget-registrypost is unrelated.\n");

  return { root, target: "packages/widget/src/widget-registry.ts" };
}

test("derives load-bearing importers via real resolution, RELATIVE and ALIAS styles both, never a text-mention count", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    assert.deepEqual(derived.loadBearing, [
      "packages/widget/src/consumer.ts",
      "packages/widget/src/index.ts",
      "packages/widget/test/WidgetRegistry.test.ts",
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("RED (the exact live #919 alias boundary): an @/-alias importer is load-bearing, never filed as merely informational or dropped", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    assert.ok(
      derived.loadBearing.includes("packages/widget/src/index.ts"),
      "an @/-alias import of the target must resolve to a real load-bearing edge, matching packages/testing/src/index.ts's real @/ChiefdBinary import",
    );
    assert.ok(!derived.informational.includes("packages/widget/src/index.ts"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a string that merely looks like an import is never treated as a load-bearing reference", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    assert.ok(!derived.loadBearing.includes("packages/widget/test/Unrelated.test.ts"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a plain-text mention is reported as informational, never merged into load-bearing", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    // Unrelated.test.ts is not a load-bearing importer (its "import" text is
    // inside a string literal), but it DOES contain a real whole-word mention
    // of the stem, so it is correctly informational, not silently dropped.
    assert.deepEqual(derived.informational, ["packages/widget/docs/NOTES.md", "packages/widget/test/Unrelated.test.ts"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a substring-only hit (not a whole word) is filtered and the filtered count is stated, not hidden", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    assert.ok(!derived.informational.includes("packages/widget/docs/OTHER.md"));
    assert.equal(derived.filteredOut.nonWordBoundarySubstringHits, 1);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a specifier this tool cannot resolve is named under 'unresolved', never silently folded into informational (fail-closed)", () => {
  const { root, target } = fixture();
  try {
    writeFileSync(
      join(root, "packages", "widget", "src", "broken.ts"),
      "import { widgetRegistry } from '@/does-not-exist-anywhere'\nexport { widgetRegistry }\n",
    );
    const derived = deriveReferences(root, target);
    assert.ok(
      derived.unresolved.some((u) => u.file === "packages/widget/src/broken.ts" && u.specifier === "@/does-not-exist-anywhere"),
      "an unresolvable alias specifier must be named, never silently dropped",
    );
    assert.ok(!derived.informational.includes("packages/widget/src/broken.ts"));
    assert.ok(!derived.loadBearing.includes("packages/widget/src/broken.ts"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an unresolved specifier is marked PLAUSIBLE only when its own final path segment names the target's stem", () => {
  const { root, target } = fixture();
  try {
    // Plausible: last segment is exactly "widget-registry" (the target's stem).
    writeFileSync(
      join(root, "packages", "widget", "src", "plausible-broken.ts"),
      "import { widgetRegistry } from '../not-a-real-dir/widget-registry'\nexport { widgetRegistry }\n",
    );
    // Implausible: last segment names something else entirely.
    writeFileSync(
      join(root, "packages", "widget", "src", "implausible-broken.ts"),
      "import { widgetRegistry } from '@/does-not-exist-anywhere'\nexport { widgetRegistry }\n",
    );
    const derived = deriveReferences(root, target);
    const plausibleEntry = derived.unresolved.find((u) => u.file === "packages/widget/src/plausible-broken.ts");
    const implausibleEntry = derived.unresolved.find((u) => u.file === "packages/widget/src/implausible-broken.ts");
    assert.equal(plausibleEntry.plausible, true);
    assert.equal(implausibleEntry.plausible, false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// The demonstrated red: an under-named declared list must fail, not pass.
// ---------------------------------------------------------------------------

test("RED: a declared list omitting a real load-bearing importer fails, naming exactly what is missing", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    // The exact #830 shape: the story names two real importers and misses the third.
    const result = checkAgainstDeclared(derived, ["packages/widget/src/consumer.ts", "packages/widget/src/index.ts"]);
    assert.equal(result.ok, false);
    assert.deepEqual(result.missing, ["packages/widget/test/WidgetRegistry.test.ts"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("GREEN: a declared list covering every real load-bearing importer, with no unresolved specifiers, passes", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    const result = checkAgainstDeclared(derived, [
      "packages/widget/src/consumer.ts",
      "packages/widget/src/index.ts",
      "packages/widget/test/WidgetRegistry.test.ts",
    ]);
    assert.equal(result.ok, true);
    assert.deepEqual(result.missing, []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("FAIL-CLOSED: a complete declared list still fails while a PLAUSIBLE unresolved specifier exists -- completeness cannot be certified through a gap this tool admits it cannot see", () => {
  const { root, target } = fixture();
  try {
    // Plausible: its own final segment names the target's stem.
    writeFileSync(
      join(root, "packages", "widget", "src", "broken.ts"),
      "import { widgetRegistry } from '../not-a-real-dir/widget-registry'\nexport { widgetRegistry }\n",
    );
    const derived = deriveReferences(root, target);
    const result = checkAgainstDeclared(derived, [
      "packages/widget/src/consumer.ts",
      "packages/widget/src/index.ts",
      "packages/widget/test/WidgetRegistry.test.ts",
    ]);
    assert.equal(result.ok, false);
    assert.deepEqual(result.missing, []);
    assert.ok(result.unresolved.length > 0);
    assert.ok(result.unresolved.every((u) => u.plausible));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("NOT FAIL-CLOSED ON NOISE: an IMPLAUSIBLE unresolved specifier (names something unrelated) does not block a complete declared list -- the exact fix for the guard-nobody-can-satisfy hazard", () => {
  const { root, target } = fixture();
  try {
    writeFileSync(
      join(root, "packages", "widget", "src", "unrelated-broken.ts"),
      "import { thing } from '@/totally-unrelated-module'\nexport { thing }\n",
    );
    const derived = deriveReferences(root, target);
    const result = checkAgainstDeclared(derived, [
      "packages/widget/src/consumer.ts",
      "packages/widget/src/index.ts",
      "packages/widget/test/WidgetRegistry.test.ts",
    ]);
    assert.equal(result.ok, true);
    assert.ok(result.implausibleUnresolved.some((u) => u.specifier === "@/totally-unrelated-module"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a declared list may omit informational-only mentions without failing (docs are not a compile break)", () => {
  const { root, target } = fixture();
  try {
    const derived = deriveReferences(root, target);
    // docs/NOTES.md is deliberately never in this declared list.
    const result = checkAgainstDeclared(derived, [
      "packages/widget/src/consumer.ts",
      "packages/widget/src/index.ts",
      "packages/widget/test/WidgetRegistry.test.ts",
    ]);
    assert.equal(result.ok, true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL REPO sanity: running against a real in-tree file does not crash and returns the expected shape", () => {
  const repoRoot = new URL("../..", import.meta.url).pathname;
  const derived = deriveReferences(repoRoot, "scripts/deletion-scope-audit.mjs");
  assert.ok(Array.isArray(derived.loadBearing));
  assert.ok(Array.isArray(derived.informational));
  assert.ok(Array.isArray(derived.unresolved));
  assert.equal(typeof derived.filteredOut.nonWordBoundarySubstringHits, "number");
});

test("a bare external package specifier that cannot resolve (e.g. an unbuilt sibling) is EXTERNAL, not UNRESOLVED -- a deletion target is always local, never a published package", () => {
  const { root, target } = fixture();
  try {
    writeFileSync(
      join(root, "packages", "widget", "src", "external-consumer.ts"),
      "import { thing } from '@chief/not-built-yet'\nexport { thing }\n",
    );
    const derived = deriveReferences(root, target);
    assert.ok(!derived.unresolved.some((u) => u.specifier === "@chief/not-built-yet"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("REAL REPO regression: packages/testing/src/index.ts's real @/ChiefdBinary import is found as load-bearing (the exact live incident)", () => {
  const repoRoot = new URL("../..", import.meta.url).pathname;
  const derived = deriveReferences(repoRoot, "packages/testing/src/ChiefdBinary.ts");
  assert.ok(
    derived.loadBearing.includes("packages/testing/src/index.ts"),
    "packages/testing/src/index.ts imports ChiefdBinary.ts via '@/ChiefdBinary' -- must be load-bearing, not filed under informational as it was before this fix",
  );
});

// ---------------------------------------------------------------------------
// #919 boundary: the api-surface string-literal companion, found on this
// tool's first live use against the real `LocksClient` deletion. Reproduces
// that exact shape -- a resource class with route-path literals, mounted
// under a property elsewhere, referenced ONLY by a JSON fixture keyed on
// "<mountKey>.<method>" strings that `deriveReferences` cannot see at all.
// ---------------------------------------------------------------------------

function locksFixture() {
  const root = mkdtempSync(join(tmpdir(), "deletion-scope-audit-locks-"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "fixture-root", workspaces: [] }));
  writeFileSync(join(root, "tsconfig.base.json"), JSON.stringify({ compilerOptions: {} }));
  mkdirSync(join(root, "src", "resources"), { recursive: true });
  mkdirSync(join(root, "test", "fixtures"), { recursive: true });

  writeFileSync(
    join(root, "src", "resources", "Locks.ts"),
    [
      "export class LocksClient {",
      "  acquire(scope) { return this.post('/v1/locks/acquire', scope) }",
      "  release(scope) { return this.post('/v1/locks/release', scope) }",
      "  private post(path, body) { return { path, body } }",
      "}",
    ].join("\n"),
  );
  writeFileSync(
    join(root, "src", "Client.ts"),
    "import { LocksClient } from './resources/Locks'\nexport class Client {\n  readonly locks: LocksClient\n}\n",
  );
  // The class the exact incident found: a JSON route table keyed by
  // "<mountKey>.<method>" strings -- no import, no filename mention, so
  // deriveReferences (both load-bearing AND informational) reports zero.
  writeFileSync(
    join(root, "test", "fixtures", "route-table.json"),
    JSON.stringify({ "locks.acquire": "/v1/locks/acquire", "locks.release": "/v1/locks/release" }, null, 2),
  );

  return root;
}

test("RED (the exact live #919 string-literal boundary): deriveReferences alone is blind to the string-keyed JSON fixture even though it correctly sees the real import edge", () => {
  const root = locksFixture();
  try {
    const derived = deriveReferences(root, "src/resources/Locks.ts");
    // Client.ts's `readonly locks: LocksClient` mount IS a real import edge
    // -- deriveReferences correctly finds it, exactly as the live incident's
    // ChiefdClient.ts mount was correctly found. The boundary is specifically
    // that route-table.json (a real dependent with no import and no
    // filename mention) is invisible to BOTH of deriveReferences's buckets.
    assert.deepEqual(derived.loadBearing, ["src/Client.ts"]);
    assert.ok(
      !derived.informational.includes("test/fixtures/route-table.json"),
      "route-table.json never mentions the filename 'Locks' as prose -- invisible to deriveReferences alone, exactly as found live",
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("deriveApiSurfaceStrings finds the route-path literals AND the <mountKey>.<method> dot-keys", () => {
  const root = locksFixture();
  try {
    const surface = deriveApiSurfaceStrings(root, "src/resources/Locks.ts");
    assert.deepEqual(surface.routePaths.sort(), ["/v1/locks/acquire", "/v1/locks/release"]);
    assert.deepEqual(surface.dotKeys.sort(), ["locks.acquire", "locks.release"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("GREEN: the api-surface companion catches the JSON fixture deriveReferences alone misses", () => {
  const root = locksFixture();
  try {
    const surface = deriveApiSurfaceReferences(root, "src/resources/Locks.ts");
    const hitFiles = surface.hits.map((h) => h.file).sort();
    assert.deepEqual([...new Set(hitFiles)], ["test/fixtures/route-table.json"]);
    const candidates = surface.hits.map((h) => h.candidate).sort();
    assert.ok(candidates.includes("/v1/locks/acquire") || candidates.includes("locks.acquire"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an unquoted substring occurrence is filtered and stated, never a silent hit (e.g. '/v1/locks/acquire-extended' must not match '/v1/locks/acquire')", () => {
  const root = locksFixture();
  try {
    writeFileSync(
      join(root, "test", "fixtures", "route-table.json"),
      JSON.stringify({ note: "/v1/locks/acquire-extended is unrelated" }, null, 2),
    );
    const result = searchApiSurfaceStrings(root, "src/resources/Locks.ts", ["/v1/locks/acquire"]);
    assert.deepEqual(result.hits, []);
    assert.equal(result.filteredOut.unquotedSubstringHits, 1);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a target with no route-path literals and no exported class methods reports the companion as not-applicable, never a false zero", () => {
  const root = mkdtempSync(join(tmpdir(), "deletion-scope-audit-noapi-"));
  try {
    mkdirSync(join(root, "src"), { recursive: true });
    writeFileSync(join(root, "src", "constants.ts"), "export const MAX = 5\n");
    const surface = deriveApiSurfaceReferences(root, "src/constants.ts");
    assert.deepEqual(surface.candidates, []);
    assert.ok(surface.note);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
