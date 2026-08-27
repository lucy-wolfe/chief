// #937: `bun test tests` masked all 347 files' real status behind one bare
// `Cannot find module '@chief/piing'` error the moment a workspace package
// was unbuilt -- nothing named the actual cause or the fix. Locks the
// ordering that closes it: `tests/setup-workspace-build-preflight.ts` must
// run FIRST in the effective preload chain, since the whole point is
// catching the resolution failure before anything that would otherwise
// throw it bare. (#1035 restated this from "before setup-durable-store.ts"
// once that file was deleted -- see assertPreflightRunsFirst below for why
// the replacement is stricter rather than looser.)
//
// #962 update: `bunfig.toml`'s `[test].preload` array no longer names both
// files directly -- it names ONE wrapper (`tests/setup-conditional-preload.ts`)
// that `await import()`s them sequentially, only when the invocation's own
// targets warrant it (see that file's own header for why: scoping tests/'s
// build requirement off scripts/test/*.test.mjs, which needs neither). A
// guard that only parses the LITERAL preload array went blind to the
// ordering the moment that indirection existed -- this file now resolves
// the EFFECTIVE chain (following one level of "a preload entry is itself a
// sequential-dynamic-import wrapper" expansion) instead of assuming the
// array is flat. Arm and control on the resolver itself: a deliberately
// mis-ordered synthetic wrapper must still be CAUGHT (arm), and the real
// wrapper's correct order must still PASS (control) -- a guard rewritten
// only until it stops complaining is indistinguishable from a guard
// deleted, so both directions are exercised here, not just the currently-
// true one.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");

function readBunfigPreloadOrder(root = REPO_ROOT) {
  const text = readFileSync(join(root, "bunfig.toml"), "utf8");
  const match = text.match(/preload\s*=\s*\[([^\]]*)\]/);
  if (!match) throw new Error("bunfig.toml has no [test] preload array -- has the format changed?");
  return match[1]
    .split(",")
    .map((entry) => entry.trim().replace(/^["']|["']$/g, ""))
    .filter(Boolean);
}

/** Every `await import('./name')` in `sourceText`, in source order, name only
 * (no leading `./`, no extension) -- the shape a sequential-dynamic-import
 * wrapper (like `setup-conditional-preload.ts`) uses to defer loading its
 * real preload targets. */
function extractSequentialDynamicImportNames(sourceText) {
  const re = /await\s+import\(\s*['"]\.\/([^'"]+)['"]\s*\)/g;
  const names = [];
  let m;
  while ((m = re.exec(sourceText))) names.push(m[1]);
  return names;
}

/** Resolve `bunfig.toml`'s literal preload array into the sequence of files
 * that ACTUALLY execute, in the order they actually execute -- expanding any
 * entry that is itself a sequential-dynamic-import wrapper (one level; the
 * repo has exactly one today, and a second layer would show up here as an
 * unexpanded entry, which the "both files present" test below would then
 * correctly fail rather than silently passing). An entry with no sequential
 * `await import()`s of its own is a real preload file and stays as-is. */
function readEffectivePreloadOrder(root = REPO_ROOT) {
  const literal = readBunfigPreloadOrder(root);
  const effective = [];
  for (const entry of literal) {
    const entryPath = join(root, entry.replace(/^\.\//, ""));
    let text;
    try {
      text = readFileSync(entryPath, "utf8");
    } catch {
      effective.push(entry); // unreadable -- let the caller's own existence checks catch it
      continue;
    }
    const wrapped = extractSequentialDynamicImportNames(text);
    if (wrapped.length === 0) {
      effective.push(entry);
      continue;
    }
    const entryDir = dirname(entry);
    for (const name of wrapped) effective.push(`${entryDir}/${name}.ts`);
  }
  return effective;
}

test("the workspace-build preflight is wired into the EFFECTIVE preload chain", () => {
  const order = readEffectivePreloadOrder();
  assert.ok(
    order.includes("./tests/setup-workspace-build-preflight.ts"),
    "the effective preload chain (following any wrapper indirection) must include the workspace-build preflight",
  );
});

/** The one assertion both the production test and the arm/control below
 * share, so ARM proves it actually throws on bad input and CONTROL proves
 * it actually passes on good input -- not two independently-written checks
 * that could quietly drift apart.
 *
 * #1035 RESTATED, NOT RELAXED. This used to be
 * `assertPreflightBeforeDurableStore`: preflight index < setup-durable-store
 * index. `tests/setup-durable-store.ts` is now DELETED -- it statically
 * imported `apps/cli/src/legacy/foundation/paths`, which #751/P0 removed
 * with the whole `apps/cli/src/legacy/` tree, so it could not even link.
 * Naming a deleted file is the stale-allowlist failure this repo's guards
 * exist to catch, and "both entries must be present" would now fail for a
 * reason that is not a defect.
 *
 * The replacement is STRICTER, not looser: the preflight must be FIRST in
 * the effective chain, not merely somewhere ahead of one named sibling.
 * That is the property the ordering was ever about -- the preflight's job
 * is to turn a bare "Cannot find module '@chief/piing'" into a named
 * diagnosis, which only works if NOTHING that could throw that error runs
 * ahead of it -- and it keeps holding when a second entry is added back,
 * whatever that entry is called. */
function assertPreflightRunsFirst(order) {
  assert.ok(order.length > 0, "the effective preload chain must not be empty");
  assert.equal(
    order[0],
    "./tests/setup-workspace-build-preflight.ts",
    "the preflight must be the FIRST entry in the effective chain, or the bare resolution error it exists to replace fires first",
  );
}

test("the preflight runs FIRST in the EFFECTIVE chain -- ordering is load-bearing, not incidental", () => {
  assertPreflightRunsFirst(readEffectivePreloadOrder());
});

test("the preflight file exists and names the packages it checks", () => {
  const path = join(REPO_ROOT, "tests", "setup-workspace-build-preflight.ts");
  assert.ok(existsSync(path));
  const text = readFileSync(path, "utf8");
  assert.match(text, /@chief\/piing/);
  assert.match(text, /@chief\/chiefing/);
});

// ---------------------------------------------------------------------------
// Arm and control on the resolver itself, against a synthetic fixture repo --
// proving readEffectivePreloadOrder() actually catches wrong ordering rather
// than having been loosened until the real (currently correct) wrapper
// merely happens to pass.
// ---------------------------------------------------------------------------

function fixtureRepo({ wrapperBody }) {
  const root = mkdtempSync(join(tmpdir(), "preflight-wiring-fixture-"));
  writeFileSync(join(root, "bunfig.toml"), '[test]\npreload = ["./tests/setup-conditional-preload.ts"]\n');
  const testsDir = join(root, "tests");
  mkdirSync(testsDir, { recursive: true });
  writeFileSync(join(testsDir, "setup-conditional-preload.ts"), wrapperBody);
  writeFileSync(join(testsDir, "setup-workspace-build-preflight.ts"), "// fixture\n");
  writeFileSync(join(testsDir, "setup-some-other-preload.ts"), "// fixture\n");
  return root;
}

test("ARM: assertPreflightRunsFirst() throws against a synthetic wrapper that imports another preload BEFORE the preflight", () => {
  const root = fixtureRepo({
    wrapperBody:
      "await import('./setup-some-other-preload')\nawait import('./setup-workspace-build-preflight')\n",
  });
  try {
    const order = readEffectivePreloadOrder(root);
    assert.deepEqual(
      order,
      ["./tests/setup-some-other-preload.ts", "./tests/setup-workspace-build-preflight.ts"],
      "fixture sanity: the resolver must see this wrapper's real (bad) order before the assertion is even exercised",
    );
    assert.throws(
      () => assertPreflightRunsFirst(order),
      /must be the FIRST entry/,
      "ARM FAILED: the guard's own assertion did not reject an order where another preload comes first",
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("CONTROL: assertPreflightRunsFirst() does not throw against the real setup-conditional-preload.ts's correct order", () => {
  assert.doesNotThrow(() => assertPreflightRunsFirst(readEffectivePreloadOrder()));
});
