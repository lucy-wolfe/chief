import { createRule } from '../utils.js'

export default createRule({
  name: 'no-optional-nullable',
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow zod optional/nullable; prefer nullish'
    },
    messages: {
      useNullish: 'Avoid .optional()/.nullable(). Use .nullish() instead.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      CallExpression(node) {
        if (node.arguments.length !== 0) return
        const callee = node.callee
        if (callee.type !== 'MemberExpression' || callee.computed) return
        if (callee.property.type !== 'Identifier') return

        if (callee.property.name === 'optional' || callee.property.name === 'nullable') {
          context.report({ node: callee.property, messageId: 'useNullish' })
        }
      }
    }
  }
})
