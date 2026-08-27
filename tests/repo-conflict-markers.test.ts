import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// An unresolved merge conflict is invisible to every other check in this
// repository. `abe37d8` (#164) merged CHANGELOG.md with NESTED, doubly
// unresolved markers at line 3; the full test suite, the typecheck, both Rust
// workspaces and the seam lints stayed green while they sat there through
// every subsequent merge until `604ea59` (#211) repaired them by hand.
//
// The patterns are built from character repetition rather than written out, so
// this file cannot match itself. A guard that trips on its own source is red on
// arrival and gets deleted.
const MARKERS = ["<", "|", "=", ">"].map((char) => ({
  label: char.repeat(7),
  pattern: new RegExp(`^[${char}]{7}`),
}));

// The tracked tree, not a diff. The defect's defining property was SURVIVAL
// across merges: a diff-scoped check reports clean on every pull request that
// does not touch the poisoned line, which is precisely how these markers
// outlived a dedupe audit that read the same file.
function trackedFiles(root: string): string[] {
  return execFileSync("git", ["ls-files", "-z"], { cwd: root, encoding: "utf8" })
    .split("\0")
    .filter((name) => name.length > 0);
}

test("no tracked file contains an unresolved merge conflict marker", () => {
  const root = process.cwd();
  const offences: string[] = [];

  for (const file of trackedFiles(root)) {
    let bytes: Buffer;
    try {
      bytes = readFileSync(join(root, file));
    } catch {
      // A tracked path that cannot be read (a submodule directory, a broken
      // symlink) carries no conflict markers of its own.
      continue;
    }
    // This repository's byte-oriented tools go silent on binary input rather
    // than erroring, so binary files are skipped explicitly instead of being
    // scanned and quietly contributing nothing.
    if (bytes.subarray(0, 8000).includes(0)) continue;

    const lines = bytes.toString("utf8").split("\n");
    for (const [index, line] of lines.entries()) {
      for (const marker of MARKERS) {
        if (marker.pattern.test(line)) {
          offences.push(`${file}:${index + 1} begins with ${marker.label}`);
        }
      }
    }
  }

  expect(offences).toEqual([]);
});
