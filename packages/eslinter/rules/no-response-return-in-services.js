import { createRule } from '../utils.js'

// Services must not return Response or Promise<Response>.
// Services return typed data; controllers/handleAction wrap in HTTP responses.
// WebSocket upgrades are the only exception — use eslint-disable with explanation.
export default createRule({
  name: 'no-response-return-in-services',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Service methods must not return Response or Promise<Response>; return typed data instead'
    },
    messages: {
      noResponseReturn:
        'Service methods must not return Response or Promise<Response>. ' +
        'Return typed data and let the controller/handleAction wrap it. ' +
        'If this is a WebSocket upgrade, add an eslint-disable comment explaining why.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename || context.getFilename()).replaceAll('\\', '/')

    // Only apply to files in services/ directories
    if (!filename.includes('/services/')) return {}

    function isResponseType(annotation) {
      if (!annotation) return false

      // Response
      if (
        annotation.type === 'TSTypeReference' &&
        annotation.typeName &&
        annotation.typeName.type === 'Identifier' &&
        annotation.typeName.name === 'Response'
      ) {
        return true
      }

      // Promise<Response>
      if (
        annotation.type === 'TSTypeReference' &&
        annotation.typeName &&
        annotation.typeName.type === 'Identifier' &&
        annotation.typeName.name === 'Promise' &&
        annotation.typeArguments &&
        annotation.typeArguments.params &&
        annotation.typeArguments.params.length === 1
      ) {
        const inner = annotation.typeArguments.params[0]
        if (
          inner.type === 'TSTypeReference' &&
          inner.typeName &&
          inner.typeName.type === 'Identifier' &&
          inner.typeName.name === 'Response'
        ) {
          return true
        }
      }

      return false
    }

    return {
      MethodDefinition(node) {
        const fn = node.value
        if (fn && fn.returnType && fn.returnType.typeAnnotation) {
          if (isResponseType(fn.returnType.typeAnnotation)) {
            context.report({ node: fn.returnType, messageId: 'noResponseReturn' })
          }
        }
      }
    }
  }
})
