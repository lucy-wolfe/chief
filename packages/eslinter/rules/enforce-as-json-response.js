import { createRule } from '../utils.js'

// Require asJsonResponse() from @/utils/cloudflare for all JSON responses.
// Forbid Response.json() and new Response() with JSON content-type.
export default createRule({
  name: 'enforce-as-json-response',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Forbid Response.json() and manual new Response with JSON; use asJsonResponse() instead'
    },
    messages: {
      useAsJsonResponse:
        'Do not use Response.json() or construct JSON responses manually. ' +
        'Use asJsonResponse() from @/utils/cloudflare instead.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()

    // Allow the asJsonResponse implementation itself
    if (filename.endsWith('/utils/cloudflare.ts')) return {}

    // Allow Durable Object private helpers that replicate the pattern
    if (filename.endsWith('/durableobjects/Cache.ts')) return {}

    return {
      // Flag Response.json(...)
      CallExpression(node) {
        if (
          node.callee.type === 'MemberExpression' &&
          node.callee.object.type === 'Identifier' &&
          node.callee.object.name === 'Response' &&
          node.callee.property.type === 'Identifier' &&
          node.callee.property.name === 'json'
        ) {
          context.report({ node, messageId: 'useAsJsonResponse' })
        }
      },

      // Flag new Response(toJsonTreeString(...)) or new Response(ensureJsonTreeString(...))
      NewExpression(node) {
        if (
          node.callee.type !== 'Identifier' ||
          node.callee.name !== 'Response'
        ) {
          return
        }
        if (node.arguments.length === 0) return

        const firstArg = node.arguments[0]
        if (
          firstArg.type === 'CallExpression' &&
          firstArg.callee.type === 'Identifier' &&
          (firstArg.callee.name === 'toJsonTreeString' ||
            firstArg.callee.name === 'ensureJsonTreeString' ||
            firstArg.callee.name === 'JSON')
        ) {
          context.report({ node, messageId: 'useAsJsonResponse' })
        }

        // Also flag JSON.stringify
        if (
          firstArg.type === 'CallExpression' &&
          firstArg.callee.type === 'MemberExpression' &&
          firstArg.callee.object.type === 'Identifier' &&
          firstArg.callee.object.name === 'JSON' &&
          firstArg.callee.property.type === 'Identifier' &&
          firstArg.callee.property.name === 'stringify'
        ) {
          context.report({ node, messageId: 'useAsJsonResponse' })
        }
      }
    }
  }
})
