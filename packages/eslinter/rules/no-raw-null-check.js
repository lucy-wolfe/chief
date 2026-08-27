import { createRule } from '../utils.js'

export default createRule({
  name: 'no-raw-null-check',
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow raw null/undefined checks; prefer a centralized isNullish() helper'
    },
    messages: {
      useIsNullish: 'Avoid raw null/undefined checks. Use isNullish(value) or !isNullish(value) instead.'
    },
    schema: [
      {
        type: 'object',
        properties: {
          allowedPaths: {
            type: 'array',
            items: { type: 'string' },
            description:
              'File path substrings where raw null/undefined checks are allowed (e.g. the file implementing isNullish() for a package that has no @tribes-terminal/foundation equivalent)'
          }
        },
        additionalProperties: false
      }
    ]
  },
  defaultOptions: [{}],
  create(context) {
    const filename = context.filename ?? ''
    // `defaultOptions: [{}]` types the option object as the empty `{}`; the
    // real shape is the one this rule's own `schema` above declares.
    /** @type {{ allowedPaths?: string[] }} */
    const options = context.options[0] ?? {}
    const allowedPaths = options.allowedPaths ?? []

    // These two are the terminal-repo isNullish() implementations this rule
    // was ported alongside (E0-S3); kept verbatim like every other ported
    // allowlist, even though neither path exists in this repo yet.
    const isIsNullishImpl =
      filename.endsWith('/packages/core/src/shared/utils/lang.ts') ||
      filename.endsWith('/packages/foundation/src/utils/lang.ts') ||
      allowedPaths.some((p) => filename.includes(p))
    if (isIsNullishImpl) {
      return {}
    }

    function isUndefinedLiteral(node) {
      if (node.type === 'Identifier' && node.name === 'undefined') {
        return true
      }
      return (
        node.type === 'UnaryExpression' &&
        node.operator === 'void' &&
        node.argument.type === 'Literal' &&
        node.argument.value === 0
      )
    }

    function isNullishLiteral(node) {
      if (node.type === 'Literal' && node.value === null) {
        return true
      }
      return isUndefinedLiteral(node)
    }

    return {
      BinaryExpression(node) {
        const isEqualityOperator =
          node.operator === '===' ||
          node.operator === '!==' ||
          node.operator === '==' ||
          node.operator === '!='
        if (!isEqualityOperator) return

        if (isNullishLiteral(node.left) || isNullishLiteral(node.right)) {
          context.report({ node, messageId: 'useIsNullish' })
        }
      }
    }
  }
})
