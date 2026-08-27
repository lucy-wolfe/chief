import { createRule } from '../utils.js'

// Require the two-argument form new URL(path, base) instead of embedding
// the base URL into a template literal or string concatenation.
export default createRule({
  name: 'enforce-url-constructor-two-args',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Require new URL(path, base) two-argument form instead of single-argument string concatenation'
    },
    messages: {
      useTwoArgs:
        'Use the two-argument form new URL(path, base) instead of concatenating the URL in a single string.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      NewExpression(node) {
        if (
          node.callee.type !== 'Identifier' ||
          node.callee.name !== 'URL'
        ) {
          return
        }

        // Only check single-argument calls
        if (node.arguments.length !== 1) return

        const arg = node.arguments[0]

        // Flag template literals that mix expressions with static text
        if (
          arg.type === 'TemplateLiteral' &&
          arg.expressions.length > 0
        ) {
          const hasNonEmptyQuasi = arg.quasis.some(
            (q) => q.value.raw.length > 0
          )
          if (hasNonEmptyQuasi) {
            context.report({ node, messageId: 'useTwoArgs' })
          }
          return
        }

        // Flag string concatenation via +
        if (
          arg.type === 'BinaryExpression' &&
          arg.operator === '+'
        ) {
          context.report({ node, messageId: 'useTwoArgs' })
        }
      }
    }
  }
})
