import { createRule } from '../utils.js'

export default createRule({
  name: 'enforce-web-client-service-suffix',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow bare Service suffixes in apps/web; use ClientService for files and classes'
    },
    messages: {
      useClientServiceFilename:
        'Rename "{{basename}}" to use the ClientService suffix in apps/web.',
      useClientServiceClass:
        'Rename class "{{className}}" to use the ClientService suffix in apps/web.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename ?? '').replaceAll('\\', '/')
    const isWebSourceFile = filename.includes('/apps/web/src/')
    if (!isWebSourceFile) {
      return {}
    }

    const basename = filename.split('/').pop() ?? ''
    const extensionMatch = basename.match(/\.[^.]+$/)
    const stem = extensionMatch ? basename.slice(0, -extensionMatch[0].length) : basename

    function hasBareServiceSuffix(value) {
      return value.endsWith('Service') && !value.endsWith('ClientService')
    }

    return {
      Program(node) {
        if (!hasBareServiceSuffix(stem)) {
          return
        }

        context.report({
          node,
          messageId: 'useClientServiceFilename',
          data: { basename }
        })
      },
      ClassDeclaration(node) {
        if (node.id === null || !hasBareServiceSuffix(node.id.name)) {
          return
        }

        context.report({
          node: node.id,
          messageId: 'useClientServiceClass',
          data: { className: node.id.name }
        })
      },
      ClassExpression(node) {
        if (node.id === null || !hasBareServiceSuffix(node.id.name)) {
          return
        }

        context.report({
          node: node.id,
          messageId: 'useClientServiceClass',
          data: { className: node.id.name }
        })
      }
    }
  }
})
