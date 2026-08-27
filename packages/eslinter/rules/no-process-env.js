import { createRule } from '../utils.js'

export default createRule({
  name: 'no-process-env',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow direct process.env access; centralize env variables in apps/*/src/common/env.ts'
    },
    messages: {
      noProcessEnv:
        'Do not access process.env directly. Define the variable in src/common/env.ts and import it from there.',
      noProcessEnvInCore:
        'Do not access process.env in packages/core. Accept configuration as function/constructor parameters instead.'
    },
    schema: [
      {
        type: 'object',
        properties: {
          allowedPaths: {
            type: 'array',
            items: { type: 'string' },
            description:
              'File path substrings where process.env access is allowed (e.g. "/src/common/")'
          }
        },
        additionalProperties: false
      }
    ]
  },
  defaultOptions: [{}],
  create(context) {
    const filename = (context.filename ?? '').replaceAll('\\', '/')
    // `defaultOptions: [{}]` types the option object as the empty `{}`; the
    // real shape is the one this rule's own `schema` above declares.
    /** @type {{ allowedPaths?: string[] }} */
    const options = context.options[0] ?? {}
    const allowedPaths = options.allowedPaths ?? []

    const isAllowed = allowedPaths.some((p) => filename.includes(p))
    if (isAllowed) {
      return {}
    }

    const isCore = filename.includes('/packages/core/')

    return {
      MemberExpression(node) {
        if (node.object.type !== 'Identifier' || node.object.name !== 'process') return
        if (node.property.type !== 'Identifier' || node.property.name !== 'env') return

        context.report({
          node,
          messageId: isCore ? 'noProcessEnvInCore' : 'noProcessEnv'
        })
      }
    }
  }
})
