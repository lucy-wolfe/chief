// Shared utilities for eslinter rules
import { ESLintUtils } from '@typescript-eslint/utils'

export const createRule = ESLintUtils.RuleCreator((name) => `https://tribes-terminal.dev/eslint/${name}`)

// Checks if a TypeScript type is an enum or literal union type
// suitable for exhaustive switch matching (e.g. `Status.Active | Status.Inactive`
// or `'active' | 'inactive'`). Allows nullish members in the union.
export function isEnumLikeType(tsType) {
  if (!tsType.isUnion || !tsType.isUnion()) return false

  const types = tsType.types
  if (types.length < 2) return false

  // TypeScript TypeFlags constants
  const STRING_LITERAL = 128
  const NUMBER_LITERAL = 256
  const ENUM_LITERAL = 1024
  const UNDEFINED = 32768
  const NULL = 65536

  let literalCount = 0

  for (const t of types) {
    const flags = t.flags
    const isLiteral =
      (flags & ENUM_LITERAL) !== 0 || (flags & STRING_LITERAL) !== 0 || (flags & NUMBER_LITERAL) !== 0

    if (isLiteral) {
      literalCount++
    } else if ((flags & UNDEFINED) === 0 && (flags & NULL) === 0) {
      // Non-literal, non-nullish member — not an enum-like type
      return false
    }
  }

  return literalCount >= 2
}
