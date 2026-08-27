/**
 * #937: `bun test tests`'s own preload (`setup-durable-store.ts`) statically
 * imports `apps/cli/src/legacy/foundation/paths.ts`, which imports
 * `@chief/piing` -- and `@chief/piing`'s `package.json` `exports` point at
 * `./dist/index.js`, which does not exist until that package is built. A
 * static import failing at MODULE LOAD TIME cannot be caught by a try/catch
 * inside the failing preload itself (or any file it statically imports) --
 * the throw happens before that file's own code runs at all, exactly the
 * shape #937 found live: `Cannot find module '@chief/piing'`, a bare
 * resolution error with no build instruction, printed once and then EVERY
 * SINGLE FILE under `tests/` (347 of them, at the time this was found)
 * reports the identical failure -- masking whatever each file's own real
 * status is, including the eight files #937 is actually about, which throw
 * a SECOND, unrelated "Cannot find module '../src/...'" the moment this
 * layer clears.
 *
 * This preload runs FIRST (see bunfig.toml) and does the ONE thing a
 * statically-failing later preload cannot do for itself: check whether the
 * workspace packages `apps/cli/src/legacy/**` transitively needs are built,
 * BEFORE anything else in the suite tries to import them, and fail with the
 * build command rather than a bare resolution error -- the same diagnostic-
 * degradation fix `setup-durable-store.ts`'s own `missingBinary()` already
 * applies to the Rust binary, extended to the TS package half of the same
 * "hard requirement, unbuilt today" shape.
 *
 * Deliberately checks by RESOLVING the package's own declared entry point
 * (`Bun.resolveSync`), never by hand-checking `packages/<name>/dist`
 * existence -- resolution is the actual question ("can anything that
 * imports this package succeed"), and hand-checking a directory would
 * silently stop matching the moment a package's `exports` map changes
 * shape without this file being updated to match.
 */
const REQUIRED_PACKAGES = ["@chief/piing", "@chief/chiefing"];

function missingWorkspaceBuild(packageName: string, cause: unknown): never {
  throw new Error(
    `[tests/setup-workspace-build-preflight.ts] cannot resolve '${packageName}' -- its dist/ output is missing or stale.\n` +
      `Underlying error: ${cause instanceof Error ? cause.message : String(cause)}\n\n` +
      "The test suite's own preload (setup-durable-store.ts) statically imports code that " +
      `depends on '${packageName}'; an unbuilt workspace package fails EVERY file under tests/ ` +
      "with an identical, unrelated-looking resolution error rather than each file's own real status.\n\n" +
      "Build workspace packages first:\n\n" +
      "    bun run build --filter='./packages/*'\n\n" +
      "(bun run test:unit builds dependencies for you automatically via turbo's task graph; " +
      "a bare `bun test tests` does not.)\n",
  );
}

for (const packageName of REQUIRED_PACKAGES) {
  try {
    Bun.resolveSync(packageName, import.meta.dir);
  } catch (error) {
    missingWorkspaceBuild(packageName, error);
  }
}
