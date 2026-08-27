// #933: 4279+ unmerged remote refs, most of them intermediate snapshots
// whose real content already reached canonical by a different SHA lineage.
// "Ahead of canonical" is NOT "has unlanded work" -- merging any of these
// wholesale would revert already-landed work, silently, in bulk (measured:
// `preserve/909-fix-fa8078c10`'s own payload is already on canonical at a
// different SHA; merging it would revert #911's 192-line deletion).
//
// This performs ONLY the enumeration + content-equivalence sweep the
// issue's own "Shape of the work" asks for as the FIRST step. It never
// merges, never deletes, never pushes. A branch is CONTENT-EQUIVALENT when
// every file it changed relative to its own merge-base with canonical now
// matches canonical's CURRENT content byte-for-byte (landed by another
// route, safe to consider for deletion in a later, separately-reviewed
// batch) -- never when the branch is merely "behind" or "old". A branch
// with even ONE changed file that still differs from canonical is UNIQUE
// CONTENT: a finding to report, never something this script acts on.
//
// THE CHECK THAT WOULD HAVE CAUGHT A NON-EQUIVALENT BRANCH (per the
// issue's condition 1): the per-file byte comparison against canonical's
// CURRENT tree, not canonical's tree at the branch's merge-base and not a
// SHA/ancestry check. A branch whose content diverged from canonical after
// the merge-base but was never actually re-integrated would show up here
// as UNIQUE (a real file mismatch), not as equivalent -- the exact
// `preserve/909` shape the issue names is a NEGATIVE case this check must
// get right: its payload matches canonical NOW even though its SHA never
// merged, so it correctly classifies equivalent; a branch that diverged
// and stayed diverged would correctly classify unique.
//
// SCOPE, STATED EXPLICITLY: `preserve/*` refs are always excluded (the
// issue's own condition 4). "Every branch belonging to the current fleet"
// is NOT mechanically derivable from git alone -- there is no ref
// namespace that reliably means "still-active tonight" versus "abandoned
// months ago" -- so the caller must supply that exclusion via
// `--exclude-pattern` (a regex tested against the short branch name,
// repeatable). Omitting it does not silently include fleet branches as
// "safe": every branch this script classifies is a REPORT, never an
// action, so the worst case of an unexcluded live branch is a wasted
// report line, not a deleted branch.
//
// WHICH DIRECTION THIS CLASSIFIER ERRS, STATED EXPLICITLY SO "UNIQUE" IS
// READ CORRECTLY: if a branch's real change landed on canonical and
// canonical LATER modified that same file again, the touched file no
// longer matches byte-for-byte and this reports UNIQUE -- a false
// positive in the HARMLESS direction (over-cautious: a branch that is
// actually safe to delete gets kept and reported as a finding instead).
// The classifier never errs the dangerous way: it cannot report EQUIVALENT
// for a branch whose real content never reached canonical, because that
// would require canonical's current file to coincidentally match content
// it never received. This asymmetry is what makes an EQUIVALENT verdict
// trustworthy and a UNIQUE verdict merely conservative, never alarming.
//
// A CENSUS MUST RECONCILE: `total remote branches` MUST equal `excluded +
// examined` exactly, printed every run -- an unreconciled census is
// exactly how a batch reported results for 4 of 7 packages tonight and
// nobody noticed, because an absent branch does not announce itself.
// Exclusion counts are broken out PER PATTERN (not lumped into one
// total), so a wrong `--exclude-pattern` is visible as a wrong count
// against a named pattern, not hidden inside a sum.

import { execFileSync } from "node:child_process";

function git(args, cwd = process.cwd()) {
  return execFileSync("git", args, { cwd, encoding: "utf8", maxBuffer: 1024 * 1024 * 64, stdio: ["ignore", "pipe", "ignore"] }).trim();
}

function gitOrNull(args, cwd = process.cwd()) {
  try {
    return git(args, cwd);
  } catch {
    return null;
  }
}

/** Every remote branch under `origin/`, excluding `HEAD` and the canonical
 *  ref itself, with its tip SHA. Derived from `git for-each-ref`, never a
 *  hand-maintained or previously-cached list -- the whole point is that
 *  4279 is too many to transcribe and transcribing invites drift the
 *  moment a new branch is pushed. */
export function listRemoteBranches(remote = "origin", cwd = process.cwd()) {
  const raw = git(["for-each-ref", `refs/remotes/${remote}`, "--format=%(refname:short) %(objectname)"], cwd);
  return raw
    .split("\n")
    .filter(Boolean)
    .map((line) => {
      const [ref, sha] = line.split(" ");
      return { ref, sha };
    })
    .filter((b) => !b.ref.endsWith("/HEAD"));
}

export function isExcluded(shortName, excludePatterns) {
  if (shortName.startsWith("preserve/")) return true;
  return excludePatterns.some((pattern) => pattern.test(shortName));
}

/** Which exclusion rule matched a branch, by LABEL, so a caller can count
 *  matches per rule rather than lumping every exclusion into one number.
 *  `excludePatterns` may be plain `RegExp`s (label = its own `.source`) or
 *  `{ label, pattern }` pairs (label stated explicitly, e.g. a CLI flag's
 *  own literal text). Returns `undefined` when nothing excludes it. */
export function exclusionLabel(shortName, excludePatterns) {
  if (shortName.startsWith("preserve/")) return "preserve/*";
  for (const entry of excludePatterns) {
    const { label, pattern } = entry instanceof RegExp ? { label: entry.source, pattern: entry } : entry;
    if (pattern.test(shortName)) return label;
  }
  return undefined;
}

/** Every file the branch changed relative to its OWN merge-base with
 *  canonical -- never the branch's full file list, since a file the
 *  branch never touched is not this branch's claim to make. */
function changedFilesSinceMergeBase(mergeBase, branchSha, cwd) {
  const out = gitOrNull(["diff", "--name-only", mergeBase, branchSha], cwd);
  if (out === null) return null;
  return out.split("\n").filter(Boolean);
}

/** Byte-for-byte comparison of one path at two revisions. `git show`
 *  returns a non-zero exit (caught by gitOrNull -> null) when the path
 *  does not exist at that revision -- treated as `undefined` content,
 *  never as an empty-string match, so "deleted in the branch, present in
 *  canonical" and "present in the branch, deleted in canonical" both
 *  correctly compare as DIFFERENT unless BOTH sides are absent. */
function contentAt(revision, path, cwd) {
  return gitOrNull(["show", `${revision}:${path}`], cwd);
}

/** Classify one branch: 'merged' (already an ancestor of canonical --
 *  out of scope for this issue entirely), 'equivalent' (every changed file
 *  matches canonical's current content), 'unique' (at least one changed
 *  file differs -- a finding, never a deletion candidate), or 'error'
 *  (git itself could not answer, e.g. a corrupt ref -- reported, not
 *  silently skipped). */
export function classifyBranch(canonicalRef, branch, cwd = process.cwd()) {
  if (gitOrNull(["merge-base", "--is-ancestor", branch.sha, canonicalRef], cwd) !== null) {
    return { ref: branch.ref, sha: branch.sha, status: "merged" };
  }
  const mergeBase = gitOrNull(["merge-base", canonicalRef, branch.sha], cwd);
  if (!mergeBase) {
    return { ref: branch.ref, sha: branch.sha, status: "error", detail: "no merge-base with canonical (unrelated history?)" };
  }
  const changed = changedFilesSinceMergeBase(mergeBase, branch.sha, cwd);
  if (changed === null) {
    return { ref: branch.ref, sha: branch.sha, status: "error", detail: "git diff against merge-base failed" };
  }
  if (changed.length === 0) {
    // No file differs from the merge-base at all (e.g. an empty/no-op
    // commit) -- vacuously equivalent, stated as its own reason rather
    // than silently folded into the normal case.
    return { ref: branch.ref, sha: branch.sha, status: "equivalent", changedFiles: 0, detail: "no file differs from its own merge-base" };
  }
  const mismatches = [];
  for (const path of changed) {
    const branchContent = contentAt(branch.sha, path, cwd);
    const canonicalContent = contentAt(canonicalRef, path, cwd);
    if (branchContent !== canonicalContent) mismatches.push(path);
  }
  if (mismatches.length === 0) {
    return { ref: branch.ref, sha: branch.sha, status: "equivalent", changedFiles: changed.length };
  }
  return { ref: branch.ref, sha: branch.sha, status: "unique", changedFiles: changed.length, mismatches };
}

/** @param {string} canonicalRef
 *  @param {{ remote?: string, excludePatterns?: { label: string, pattern: RegExp }[],
 *            cwd?: string, onProgress?: (done: number, total: number) => void }} options */
export function auditStrandedBranches(canonicalRef, { remote = "origin", excludePatterns = [], cwd = process.cwd(), onProgress } = {}) {
  const all = listRemoteBranches(remote, cwd);
  const canonicalShortName = gitOrNull(["rev-parse", "--abbrev-ref", canonicalRef], cwd) ?? canonicalRef;
  const results = [];
  const excludedByLabel = new Map();
  let canonicalSkipped = 0;
  let processed = 0;
  for (const branch of all) {
    const shortName = branch.ref.replace(new RegExp(`^${remote}/`), "");
    if (branch.ref === canonicalRef || shortName === canonicalShortName.replace(new RegExp(`^${remote}/`), "")) {
      canonicalSkipped += 1;
      continue;
    }
    const label = exclusionLabel(shortName, excludePatterns);
    if (label !== undefined) {
      excludedByLabel.set(label, (excludedByLabel.get(label) ?? 0) + 1);
      continue;
    }
    results.push(classifyBranch(canonicalRef, branch, cwd));
    processed += 1;
    if (onProgress && processed % 50 === 0) onProgress(processed, all.length);
  }
  const excludedTotal = [...excludedByLabel.values()].reduce((a, b) => a + b, 0);
  return {
    total: all.length,
    canonicalSkipped,
    excludedTotal,
    excludedByLabel: Object.fromEntries(excludedByLabel),
    examined: results.length,
    // Reconciliation invariant, computed here so the CLI can assert it
    // rather than trust arithmetic done twice: total must equal every
    // branch accounted for, by exactly one of these three buckets.
    reconciles: all.length === canonicalSkipped + excludedTotal + results.length,
    merged: results.filter((r) => r.status === "merged"),
    equivalent: results.filter((r) => r.status === "equivalent"),
    unique: results.filter((r) => r.status === "unique"),
    errors: results.filter((r) => r.status === "error"),
  };
}

async function main() {
  const args = process.argv.slice(2);
  const canonicalRef = args[0] ?? "origin/revamp/monorepo";
  const excludePatterns = [];
  for (let i = 1; i < args.length; i += 1) {
    if (args[i] === "--exclude-pattern" && args[i + 1]) {
      excludePatterns.push({ label: args[i + 1], pattern: new RegExp(args[i + 1]) });
      i += 1;
    }
  }
  console.log(`[stranded-branch-audit] canonical: ${canonicalRef}, exclude patterns: ${excludePatterns.map((p) => p.label).join(", ") || "(none)"}`);
  const startedAt = Date.now();
  const result = auditStrandedBranches(canonicalRef, {
    excludePatterns,
    onProgress: (done, total) => {
      const elapsedS = Math.round((Date.now() - startedAt) / 1000);
      console.log(`[progress] ${done} branches classified (elapsed ${elapsedS}s)`);
    },
  });
  console.log(`total remote refs: ${result.total}`);
  console.log(`  canonical ref itself (skipped): ${result.canonicalSkipped}`);
  console.log(`  excluded: ${result.excludedTotal}`);
  console.log(`    preserve/*: ${result.excludedByLabel["preserve/*"] ?? 0}`);
  for (const [label, count] of Object.entries(result.excludedByLabel)) {
    if (label === "preserve/*") continue;
    console.log(`    ${label}: ${count}`);
  }
  console.log(`  examined: ${result.examined}`);
  console.log(
    `RECONCILIATION: total (${result.total}) ${result.reconciles ? "==" : "!="} canonical-skipped (${result.canonicalSkipped}) + excluded (${result.excludedTotal}) + examined (${result.examined})` +
      (result.reconciles ? " -- every branch accounted for exactly once." : " -- MISMATCH, a branch vanished from this census. Refusing to trust this run's totals."),
  );
  if (!result.reconciles) {
    process.exitCode = 1;
  }
  console.log(`examined breakdown:`);
  console.log(`  already merged (out of scope): ${result.merged.length}`);
  console.log(`  content-equivalent (deletion CANDIDATES, not deleted here): ${result.equivalent.length}`);
  console.log(`  unique content (FINDINGS -- never touched): ${result.unique.length}`);
  console.log(`  errors: ${result.errors.length}`);
  if (result.equivalent.length > 0) {
    console.log("\nCONTENT-EQUIVALENT (deletion CANDIDATES for a separate, reviewed, batched deletion -- NOT deleted by this run):");
    for (const r of result.equivalent) console.log(`  ${r.ref}  sha=${r.sha}${r.detail ? `  (${r.detail})` : ""}`);
  }
  if (result.unique.length > 0) {
    console.log("\nUNIQUE CONTENT (report, do not merge or delete):");
    for (const r of result.unique) console.log(`  ${r.ref}  (${r.mismatches.length}/${r.changedFiles} changed files differ from canonical)`);
  }
  if (result.errors.length > 0) {
    console.log("\nERRORS (could not classify):");
    for (const r of result.errors) console.log(`  ${r.ref}: ${r.detail}`);
  }
  console.log(`\nNothing was merged, deleted, or pushed. content-equivalent branches are DELETION CANDIDATES for a separate, reviewed, batched deletion -- not deleted by this run.`);

  const jsonOutIdx = args.indexOf("--json-out");
  if (jsonOutIdx !== -1 && args[jsonOutIdx + 1]) {
    const { writeFileSync } = await import("node:fs");
    writeFileSync(args[jsonOutIdx + 1], JSON.stringify(result, null, 2));
    console.log(`\n[stranded-branch-audit] full receipt written to ${args[jsonOutIdx + 1]}`);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main();
}
