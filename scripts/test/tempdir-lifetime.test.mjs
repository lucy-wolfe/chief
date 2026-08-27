// A `tempfile::TempDir` that is never dropped is a directory that is never
// removed. Two spellings disable the destructor, both of them deliberately,
// and both of them were in this tree:
//
//   * `TempDir::keep()` — consumes the handle and returns the path with
//     cleanup switched OFF. Six sites (`docstore/store.rs` x5,
//     `docstore/tasks.rs` x1, the last inside a fixture 46 tests share).
//   * `Box::leak(Box::new(tempfile::tempdir()...))` — leaks the handle to
//     borrow its path for `'static`. Four sites in `chiefd-api`'s tests.
//
// Together they left 86 directories and 37 MB behind on ONE
// `cargo test -p chiefd-api` run, measured with the recipe in the header of
// each fixed site. Nothing reaped them, so they accumulated on every build
// host until `/tmp` reached 100%, at which point SQLite answered ENOSPC to
// ordinary writes and chiefd labelled that `corrupt store: company-db` — a
// full disk wearing the one word that sends an operator hunting for damaged
// bytes. Three separate reports of a "reproducible product defect on clean
// main" on 2026-08-10 were that disk.
//
// Both spellings have honest uses elsewhere (a `TempDir` deliberately handed
// to a child process that outlives the parent, say), which is why this guard
// is scoped to the Rust workspace's own sources and states the alternative in
// its failure message rather than banning the API outright.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");
const CARGO_WORKSPACE = join(REPO_ROOT, "apps", "chiefd", "crates");

/** Every `.rs` file under the cargo workspace, derived rather than listed. */
function rustSources(root = CARGO_WORKSPACE) {
  const found = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === "target") continue;
        walk(full);
      } else if (entry.name.endsWith(".rs")) {
        found.push(full);
      }
    }
  };
  walk(root);
  return found;
}

/** Line-level hits for a pattern, as `path:line` with the source line. */
function scan(pattern) {
  const hits = [];
  for (const file of rustSources()) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, index) => {
      if (line.trim().startsWith("//")) return;
      if (pattern.test(line)) {
        hits.push(`${relative(REPO_ROOT, file)}:${index + 1}: ${line.trim()}`);
      }
    });
  }
  return hits;
}

// The vacuity floor, first. Every assertion below is "this scan found
// nothing", which is also what a scan pointed at an empty directory returns —
// the exact instrument-that-cannot-see-its-subject shape this repo keeps
// producing. If the cargo workspace moves again, this fails by name instead of
// going quietly green.
test("REAL REPO: the scan root exists and holds Rust sources", () => {
  assert.ok(
    statSync(CARGO_WORKSPACE).isDirectory(),
    `the cargo workspace is not at ${relative(REPO_ROOT, CARGO_WORKSPACE)} — this guard has stopped measuring its subject`
  );
  const sources = rustSources();
  assert.ok(
    sources.length > 100,
    `only ${sources.length} Rust files found under ${relative(REPO_ROOT, CARGO_WORKSPACE)} — the scan is vacuous and would pass against anything`
  );
});

test("REAL REPO: no tempdir handle is disposed of with keep()", () => {
  const hits = scan(/\btempdir\(\)[\s\S]*\.keep\(\)|^\s*let\s+\w+\s*=\s*dir\.keep\(\)|\bdir\.keep\(\)/);
  assert.deepEqual(
    hits,
    [],
    `TempDir::keep() disables cleanup, so these directories are never removed:\n${hits.join("\n")}\n\nKeep the handle alive for as long as the thing that borrows its path instead — return it beside the value (see docstore/tasks.rs's TempStore) or bind it in the caller's frame.`
  );
});

test("REAL REPO: no tempdir handle is leaked to borrow its path for 'static", () => {
  const hits = scan(/Box::leak\s*\(\s*Box::new\s*\(\s*tempfile::tempdir/);
  assert.deepEqual(
    hits,
    [],
    `a leaked TempDir never runs its destructor, so its directory survives the process:\n${hits.join("\n")}\n\nReturn the handle to the caller instead — a test's stack frame outlives the router or store that borrows its path.`
  );
});

// Demonstrated red: proves the patterns actually match the shapes they name,
// rather than only that the tree happens to be clean today.
test("the patterns fire on the two spellings this guard exists for", () => {
  const keep = /\bdir\.keep\(\)/;
  const leak = /Box::leak\s*\(\s*Box::new\s*\(\s*tempfile::tempdir/;
  assert.ok(keep.test(`let path = dir.keep().join("org.sqlite");`));
  assert.ok(leak.test(`let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));`));
  assert.ok(!keep.test(`let path = dir.path().join("org.sqlite");`));
  assert.ok(!leak.test(`let dir = tempfile::tempdir().expect("tempdir");`));
});
