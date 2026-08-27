import { createRule } from '../utils.js'

export default createRule({
  name: 'no-async-in-utils',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow async functions in utils/ directories. Utils must be synchronous and pure.'
    },
    messages: {
      noAsync:
        'Async functions are not allowed in utils/ directories. ' +
        'Move async logic to helpers/ (thin I/O adapters) or services/ (business logic).'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename ?? '').replaceAll('\\', '/')
    if (!filename.includes('/utils/') || filename.includes('/test/')) {
      return {}
    }

    function reportIfAsync(node) {
      if (node.async) {
        context.report({ node, messageId: 'noAsync' })
      }
    }

    return {
      FunctionDeclaration: reportIfAsync,
      FunctionExpression: reportIfAsync,
      ArrowFunctionExpression: reportIfAsync
    }
  }
})
