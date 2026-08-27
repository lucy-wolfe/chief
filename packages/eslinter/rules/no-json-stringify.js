import { createRule } from '../utils.js'

export default createRule({
  name: 'no-json-stringify',
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow JSON.stringify(); prefer a centralized toJsonTreeString() helper'
    },
    messages: {
      useToJsonTreeString: 'Avoid JSON.stringify(). Use toJsonTreeString() or ensureJsonTreeString() instead.'
    },
    schema: [
      {
        type: 'object',
        properties: {
          allowedPaths: {
            type: 'array',
            items: { type: 'string' },
            description:
              'File path substrings where JSON.stringify() is allowed (e.g. the file implementing toJsonTreeString() for a package that has no @tribes-terminal/foundation equivalent)'
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

    if (allowedPaths.some((p) => filename.includes(p))) {
      return {}
    }

    return {
      CallExpression(node) {
        const callee = node.callee
        if (
          callee.type === 'MemberExpression' &&
          !callee.computed &&
          callee.object.type === 'Identifier' &&
          callee.object.name === 'JSON' &&
          callee.property.type === 'Identifier' &&
          callee.property.name === 'stringify'
        ) {
          context.report({ node: callee, messageId: 'useToJsonTreeString' })
        }
      }
    }
  }
})
