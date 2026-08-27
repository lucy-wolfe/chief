import { createRule } from '../utils.js'

export default createRule({
  name: 'enforce-handle-action',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Require public async methods returning Promise<Response> in controllers to wrap their body with handleAction'
    },
    messages: {
      missingHandleAction:
        'Controller method "{{ methodName }}" returns Promise<Response> but does not use handleAction. ' +
        'Wrap the method body with handleAction({ context, action, title }).'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      MethodDefinition(node) {
        // Only check public (non-private, non-protected) async methods
        if (node.accessibility === 'private' || node.accessibility === 'protected') return
        if (!node.value || node.value.type !== 'FunctionExpression') return
        if (!node.value.async) return

        // Check if return type annotation is Promise<Response>
        const returnType = node.value.returnType
        if (!returnType) return

        const typeAnnotation = returnType.typeAnnotation
        if (!typeAnnotation) return

        // Match Promise<Response>
        if (
          typeAnnotation.type !== 'TSTypeReference' ||
          typeAnnotation.typeName.type !== 'Identifier' ||
          typeAnnotation.typeName.name !== 'Promise'
        ) {
          return
        }

        // `typeParameters` was the pre-v6 alias for this field and is gone
        // from the AST types; reading it produced `undefined` at runtime and
        // widened the whole expression to `any`, which is what hid the
        // `EntityName` narrowing bug above.
        const typeParams = typeAnnotation.typeArguments
        const firstParam = typeParams?.params[0]
        if (
          !typeParams ||
          typeParams.params.length !== 1 ||
          firstParam.type !== 'TSTypeReference' ||
          firstParam.typeName.type !== 'Identifier' ||
          firstParam.typeName.name !== 'Response'
        ) {
          return
        }

        // Now check that the method body contains a call to handleAction
        const body = node.value.body
        if (!body || !body.body || body.body.length === 0) return

        const hasHandleAction = containsHandleActionCall(body)
        if (!hasHandleAction) {
          const methodName =
            node.key.type === 'Identifier' ? node.key.name : '<computed>'
          context.report({
            node: node.key,
            messageId: 'missingHandleAction',
            data: { methodName }
          })
        }
      }
    }

    function containsHandleActionCall(node) {
      if (!node || typeof node !== 'object') return false

      if (
        node.type === 'CallExpression' &&
        node.callee &&
        node.callee.type === 'Identifier' &&
        node.callee.name === 'handleAction'
      ) {
        return true
      }

      for (const key of Object.keys(node)) {
        if (key === 'parent') continue
        const child = node[key]
        if (Array.isArray(child)) {
          for (const item of child) {
            if (item && typeof item === 'object' && item.type && containsHandleActionCall(item)) {
              return true
            }
          }
        } else if (child && typeof child === 'object' && child.type) {
          if (containsHandleActionCall(child)) return true
        }
      }

      return false
    }
  }
})
