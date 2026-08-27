import { createRule } from '../utils.js'

export default createRule({
  name: 'no-fetch-outside-scoped-helpers',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow fetch usage outside configurable helpers directories; use helper functions instead'
    },
    messages: {
      useHelper:
        'Do not call fetch outside of helpers/. Create or use a helper function in the helpers/ directory instead.'
    },
    schema: [
      {
        type: 'array',
        items: {
          type: 'object',
          properties: {
            scopePath: {
              type: 'string',
              description:
                'Only apply to files whose path includes this substring. If omitted, applies to all files in this config.'
            },
            helpersPath: {
              type: 'string',
              description: 'Substring that identifies the allowed helpers directory.'
            }
          },
          required: ['helpersPath'],
          additionalProperties: false
        },
        minItems: 1
      }
    ]
  },
  defaultOptions: [[]],
  create(context) {
    const scopes = context.options[0] ?? []
    if (scopes.length === 0) {
      return {}
    }

    const filename = (context.filename ?? '').replaceAll('\\', '/')

    const matchedScope = scopes.find((scope) => {
      if (scope.scopePath && !filename.includes(scope.scopePath)) {
        return false
      }
      return true
    })

    if (!matchedScope) {
      return {}
    }

    if (matchedScope.helpersPath && filename.includes(matchedScope.helpersPath)) {
      return {}
    }

    function isFetchIdentifier(node) {
      return node.type === 'Identifier' && node.name === 'fetch'
    }

    function isFetchMemberExpression(node) {
      if (node.type !== 'MemberExpression' || node.computed) {
        return false
      }
      if (node.property.type !== 'Identifier' || node.property.name !== 'fetch') {
        return false
      }
      if (node.object.type !== 'Identifier') {
        return false
      }

      switch (node.object.name) {
        case 'globalThis':
        case 'window':
          return true
      }

      return false
    }

    return {
      CallExpression(node) {
        const callee = node.callee
        const isDisallowedFetchCall =
          isFetchIdentifier(callee) || isFetchMemberExpression(callee)

        if (!isDisallowedFetchCall) {
          return
        }

        context.report({ node: callee, messageId: 'useHelper' })
      }
    }
  }
})
