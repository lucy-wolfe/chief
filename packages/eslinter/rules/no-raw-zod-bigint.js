import { createRule } from '../utils.js'

// Disallow raw z.bigint() — use BigintSchema or BigNumberSchema instead.
//
// z.bigint() does not handle string-to-bigint coercion, which means API
// payloads serialised as JSON (where bigints become strings) will fail
// validation at runtime.  BigintSchema already wraps z.bigint() with a
// string transform, so consumers should always prefer it.
export default createRule({
  name: 'no-raw-zod-bigint',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow z.bigint(); use BigintSchema or BigNumberSchema from @tribes-terminal/foundation instead'
    },
    messages: {
      useBigintSchema:
        'Do not use z.bigint() directly. Import and use BigintSchema (or BigNumberSchema) from @tribes-terminal/foundation instead.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const zodLocalNames = new Set()

    return {
      ImportDeclaration(node) {
        if (node.source.value !== 'zod') return
        for (const spec of node.specifiers) {
          if (
            spec.type === 'ImportSpecifier' &&
            spec.imported.type === 'Identifier' &&
            spec.imported.name === 'z'
          ) {
            zodLocalNames.add(spec.local.name)
          } else if (spec.type === 'ImportDefaultSpecifier') {
            zodLocalNames.add(spec.local.name)
          } else if (spec.type === 'ImportNamespaceSpecifier') {
            zodLocalNames.add(spec.local.name)
          }
        }
      },
      CallExpression(node) {
        const { callee } = node
        if (
          callee.type === 'MemberExpression' &&
          callee.object.type === 'Identifier' &&
          zodLocalNames.has(callee.object.name) &&
          callee.property.type === 'Identifier' &&
          callee.property.name === 'bigint'
        ) {
          context.report({ node, messageId: 'useBigintSchema' })
        }
      }
    }
  }
})
