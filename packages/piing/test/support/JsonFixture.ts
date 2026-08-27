/**
 * Test-only JSON fixture writer, centralizing the one `JSON.stringify` this
 * package's tests need (writing arbitrary fixture files: models.json,
 * settings.json, auth.json, …). `toJsonTreeString`/`ensureJsonTreeString`
 * from `@tribes-terminal/foundation` (the rule's suggested replacement) is
 * not a dependency of this repo. Tracked by #833/#842: once
 * `no-json-stringify` gains an `allowedPaths` option this file's disable is
 * replaced with that config instead.
 */
import { writeFileSync } from 'node:fs'

export function writeJsonFixture(path: string, contents: unknown): void {
  /* eslint-disable lucy/no-json-stringify */
  // See this file's header: no `@tribes-terminal/foundation` in this repo;
  // this is a plain-data test fixture writer, not production formatting.
  writeFileSync(path, JSON.stringify(contents))
  /* eslint-enable lucy/no-json-stringify */
}
