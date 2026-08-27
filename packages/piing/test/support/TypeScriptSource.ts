/**
 * Test-only helpers for source-level guards over this package's TypeScript.
 *
 * Several guards assert that a shape is ABSENT from a source file. A guard of
 * that kind has one systematic failure mode: the tombstone comment explaining
 * why the shape was removed quotes the shape, so the guard reads its own
 * explanation as a violation, and the fix people reach for is to weaken the
 * assertion. Stripping commentary first is the fix that does not weaken it.
 */

/**
 * Block comments and whole-line `//` comments removed. Trailing comments and
 * the contents of string literals are deliberately left alone: this only ever
 * DELETES commentary, and can never turn a line of code into a different line
 * of code (a naive `//`-to-end-of-line strip would truncate any line
 * containing a `http://` URL, which is how this class of helper usually goes
 * subtly wrong).
 */
export function withoutComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n')
}
