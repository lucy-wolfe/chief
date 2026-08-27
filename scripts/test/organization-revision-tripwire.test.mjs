// #691: `revision` was the organization manifest/public-schema concurrency
// primitive this program is retiring -- OrganizationManifest.revision,
// org_settings.revision, expectedRevision/expected_revision/EXPECTED_REVISION
// fields, and --revision CLI flags. Confirmed already gone from production
// code by direct grep before this guard was written (chiefd-core's
// organization.rs/org_settings.rs, chiefd-api's wire types, the CLI's
// argument parsing): zero hits for `pub revision`, `expected_revision`,
// `expectedRevision`, `EXPECTED_REVISION`, or `--revision` anywhere outside a
// test asserting the ABSENCE/REJECTION of exactly those things.
//
// This guard is the acceptance criterion #691 names but nothing had written
// yet: "a repository tripwire rejects new organization revision concepts".
// Deliberately narrow and file-scoped rather than a general identifier
// scanner -- this repo already has one cautionary tale about a broad
// regex-based guard (`dep-declaration.test.mjs`'s header) that classified
// correctly in principle but drowned its one true positive in 27 false
// ones. A revision REGRESSION is rare and file-local by construction (the
// manifest type, the settings row, the public wire schemas), so scoping to
// those exact files and a short, exact identifier list keeps the false-
// positive surface at zero rather than trading breadth for noise.
//
// Run with `node --test scripts/test/organization-revision-tripwire.test.mjs`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");

// Exact, case-sensitive identifiers a revision-shaped concurrency field
// would use. No bare "revision" substring match (too broad -- it would hit
// this file's own comments, `git revision` terminology, changelog prose,
// and the doc-comment citations already living in these files that name
// the retired concept to explain why it's gone). Every one of these is a
// concrete, code-shaped token: a Rust struct field, a TS object key, a wire
// JSON key, or a CLI flag literal.
const FORBIDDEN_IDENTIFIERS = [
  "pub revision:",
  "pub expected_revision",
  "expected_revision:",
  "expectedRevision:",
  "EXPECTED_REVISION",
  '"--revision"',
  "'--revision'",
];

// Files this guard actually scans: the organization manifest type, the
// org_settings row store, and the public wire schemas -- exactly the three
// surfaces #691 names. NOT chiefd-host, NOT org-tmux.ts, NOT test files (a
// test asserting the ABSENCE of a legacy identifier legitimately contains
// its literal text -- see `wire/org.rs`'s
// `structural_ops_reject_legacy_revision_fences` -- and is not itself a
// regression).
const SCANNED_FILES = [
  "apps/chiefd/crates/chiefd-core/src/store/organization.rs",
  "apps/chiefd/crates/chiefd-core/src/store/org_settings.rs",
  "apps/chiefd/crates/chiefd-api/src/wire/org.rs",
];

function nonTestLines(text) {
  // A crude but sufficient split: this repo's Rust convention keeps
  // `#[cfg(test)] mod tests { ... }` as the LAST item in a file (confirmed
  // by grep across this same session's other guards), so everything before
  // the first `mod tests` marker is production code.
  return text.split(/^\s*mod tests\b/m)[0];
}

test("#691: no organization-revision-shaped identifier appears in production code in the manifest/settings/wire-schema surfaces", () => {
  const offenders = [];
  for (const relPath of SCANNED_FILES) {
    let text;
    try {
      text = readFileSync(join(repoRoot, relPath), "utf8");
    } catch {
      // A scanned file moving or being deleted is itself worth surfacing,
      // not silently skipping -- fixed relative paths, not a glob, so a
      // rename must update this list.
      offenders.push(`${relPath}: file not found -- update SCANNED_FILES if it moved`);
      continue;
    }
    const production = nonTestLines(text);
    for (const identifier of FORBIDDEN_IDENTIFIERS) {
      if (production.includes(identifier)) {
        offenders.push(`${relPath}: contains forbidden identifier "${identifier}"`);
      }
    }
  }
  assert.deepEqual(offenders, [], `organization-revision concept(s) reintroduced:\n  ${offenders.join("\n  ")}`);
});

test("#691 self-check: the guard's own file list is non-empty and every path is a real, distinct file", () => {
  assert.ok(SCANNED_FILES.length >= 3, "SCANNED_FILES should not shrink to near-nothing silently");
  assert.equal(new Set(SCANNED_FILES).size, SCANNED_FILES.length, "no duplicate scanned paths");
});
