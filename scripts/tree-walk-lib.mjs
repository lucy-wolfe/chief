// The one definition of "directories a repo tree-walk must never descend into".
//
// # Why this file exists
//
// Every guard that walks the repo from its root used to hand-maintain its own
// skip set, and the sets were near-identical: `.git`, `node_modules`, `target`,
// `dist`, `.turbo`, `.next`, `coverage`. That is the duplicated-predicate shape
// this repo keeps getting bitten by — one guard gets a fix and the next guard,
// written from an older template, does not.
//
// It got bitten. **Agent worktrees live under `.claude/worktrees/<name>/`, and
// each one is a FULL second checkout of the repo** — production TypeScript,
// Rust, workflows and all. No guard's skip set named `.claude`, so any seat with
// a live worktree turned five guards red on every machine at once, each failure
// naming a path inside somebody else's in-progress branch that the reader did
// not recognise and could not fix. Two of them did not merely report a wrong
// finding: they died with `ENOENT` on a dangling path inside a nested checkout.
//
// That last part is why this is a WALK exclusion and not a result filter. A
// guard that collects everything and filters afterwards still crashes during
// the collection. The directory has to be skipped before anything descends into
// it or stats a path inside it.
//
// # Why CI never caught it
//
// CI checks out a clean tree and has no worktrees, so all five of these guards
// are permanently green there. The signal was only ever broken locally, which is
// the worst place for it to break: `CLAUDE.md`'s standing rule is that a correct
// guard nobody can run before pushing produces exactly the same outcome as a
// broken one. Five guards red for a reason nobody can act on is how a whole
// suite stops being read, and how a real red rides through with the noise.

/**
 * Directory basenames no repo tree-walk may descend into.
 *
 * Two kinds, and the distinction is worth keeping in mind when adding to it:
 *
 * - **Not source.** `node_modules`, `target`, `dist`, `.turbo`, `.next`,
 *   `coverage` — build output and vendored dependencies. Walking them is slow
 *   and every finding inside is about code this repo did not write.
 * - **Not THIS checkout.** `.git`, and `.claude`, which contains
 *   `worktrees/<name>/` — other branches, mid-edit, belonging to other agents.
 *   A finding in there is true of somebody else's work in progress and false of
 *   the tree the reader is standing in.
 *
 * `.claude` is excluded by the directory that CONTAINS the worktrees rather
 * than by `worktrees`, because `.claude/` holds nothing a source walk should
 * ever reach — settings, skills and a status line — and matching the bare name
 * `worktrees` would also swallow any legitimately-named directory elsewhere in
 * the tree.
 *
 * MODULE-PRIVATE, and handed out only as a fresh `Set` by [`skipSet`]. The
 * first draft exported `Object.freeze(new Set([...]))`, which is not frozen at
 * all — a `Set`'s entries live in internal slots rather than in properties, so
 * `.add()` still works and any guard could have silently widened the exclusion
 * for the whole suite. Keeping the shared value here and copying it per caller
 * removes that possibility instead of documenting it.
 *
 * @type {readonly string[]}
 */
const NOT_THIS_CHECKOUT = Object.freeze([
  '.git',
  '.claude',
  'node_modules',
  'target',
  'dist',
  '.turbo',
  '.next',
  'coverage'
])

/**
 * The set of directory basenames a walk must skip, plus this guard's own.
 *
 * Test the ENTRY NAME with it during the walk, before descending into the
 * directory or stating anything inside it — see the module note on `ENOENT`.
 * A fresh set per call, so one guard's additions can never reach another's.
 *
 * @param {Iterable<string>} [also] extra basenames this particular guard skips
 * @returns {Set<string>}
 */
export function skipSet(also = []) {
  return new Set([...NOT_THIS_CHECKOUT, ...also])
}
