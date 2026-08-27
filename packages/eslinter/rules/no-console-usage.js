import { createRule } from '../utils.js'

export default createRule({
  name: 'no-console-usage',
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow direct console usage; use the shared logger helpers instead'
    },
    messages: {
      useLogger:
        'Avoid direct console usage. Use the shared logger (`logger` or `createLogger`) instead.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename ?? '').replaceAll('\\', '/')
    const isLoggerImplementation =
      filename.endsWith('/packages/core/src/shared/utils/Logging.ts') ||
      filename.endsWith('/packages/foundation/src/utils/Logging.ts')

    if (isLoggerImplementation) {
      return {}
    }

    return {
      MemberExpression(node) {
        if (node.object.type !== 'Identifier' || node.object.name !== 'console') {
          return
        }

        if (node.property.type !== 'Identifier') {
          return
        }

        context.report({
          node,
          messageId: 'useLogger'
        })
      }
    }
  }
})
