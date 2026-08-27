import { createRule } from '../utils.js'

export default createRule({
  name: 'no-node-env-default',
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow defaulting NODE_ENV to any value (especially "development")'
    },
    messages: {
      noNodeEnvDefault:
        'Do not default NODE_ENV to any value. Use ensureString(process.env.NODE_ENV, "NODE_ENV not set") to ensure it is always provided.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      VariableDeclarator(node) {
        // Check if this is a NODE_ENV declaration
        if (node.id.type !== 'Identifier' || node.id.name !== 'NODE_ENV') return
        if (!node.init) return

        // Check for pattern: process.env.NODE_ENV ?? 'value'
        if (node.init.type === 'LogicalExpression' && node.init.operator === '??') {
          const left = node.init.left
          if (
            left.type === 'MemberExpression' &&
            left.object.type === 'MemberExpression' &&
            left.object.object.type === 'Identifier' &&
            left.object.object.name === 'process' &&
            left.object.property.type === 'Identifier' &&
            left.object.property.name === 'env' &&
            left.property.type === 'Identifier' &&
            left.property.name === 'NODE_ENV'
          ) {
            context.report({
              node: node.init,
              messageId: 'noNodeEnvDefault'
            })
          }
        }

        // Check for pattern: process.env.NODE_ENV || 'value'
        if (node.init.type === 'LogicalExpression' && node.init.operator === '||') {
          const left = node.init.left
          if (
            left.type === 'MemberExpression' &&
            left.object.type === 'MemberExpression' &&
            left.object.object.type === 'Identifier' &&
            left.object.object.name === 'process' &&
            left.object.property.type === 'Identifier' &&
            left.object.property.name === 'env' &&
            left.property.type === 'Identifier' &&
            left.property.name === 'NODE_ENV'
          ) {
            context.report({
              node: node.init,
              messageId: 'noNodeEnvDefault'
            })
          }
        }
      }
    }
  }
})
