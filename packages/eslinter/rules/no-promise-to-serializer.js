import { ESLintUtils } from '@typescript-eslint/utils'
import { createRule } from '../utils.js'

// The rule's own default: `JSON.stringify` is always a known serializer,
// with or without the `serializers` option. A consuming package's
// `no-json-stringify` (a syntax-only, unconditional ban) already forbids
// `JSON.stringify` outright in most of this repo — this rule exists for
// what that ban doesn't cover: a future project serializer
// (`toJsonTreeString`, `ensureJsonTreeString` — referenced by
// `no-json-stringify`'s own message but not yet implemented anywhere in
// this repo) would need its own entry in `serializers` the day it lands.
const JSON_STRINGIFY = { object: 'JSON', method: 'stringify' }

/**
 * Find the configured serializer a call's callee matches, or `null`.
 * Supports both `Object.method(...)` (member calls, e.g. `JSON.stringify`)
 * and bare `method(...)` calls (a future helper import, e.g.
 * `toJsonTreeString(...)`) via an entry with no `object`.
 */
function matchingSerializer(callee, serializers) {
  if (callee.type === 'Identifier') {
    return serializers.find((s) => !s.object && s.method === callee.name) ?? null
  }
  if (callee.type === 'MemberExpression' && !callee.computed && callee.property.type === 'Identifier') {
    const objectName = callee.object.type === 'Identifier' ? callee.object.name : null
    const methodName = callee.property.name
    return (
      serializers.find((s) => s.method === methodName && (s.object ? s.object === objectName : true)) ?? null
    )
  }
  return null
}

/** `true` when `tsType` is (or, for a union, includes) a `Promise<...>`. A
 * string-based check on `typeToString` rather than a symbol-identity
 * comparison: robust across the union/generic shapes real code actually
 * produces (the approach the retired `no-bignumber-to-string` also took), and it
 * catches `Promise<T> | undefined`-shaped optional-await mistakes too, not
 * only a bare `Promise<T>`. */
function isPromiseType(tsType, checker) {
  const typeText = checker.typeToString(tsType)
  if (process.env.DEBUG_891) {
    console.error(`[891] typeText=${JSON.stringify(typeText)} isUnion=${tsType.isUnion?.()} flags=${tsType.flags} typesLen=${tsType.types?.length}`)
  }
  if (/^Promise</.test(typeText) || typeText === 'Promise<any>') return true
  if (tsType.isUnion?.()) {
    return tsType.types.some((member) => isPromiseType(member, checker))
  }
  return false
}

export default createRule({
  name: 'no-promise-to-serializer',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow passing a Promise-typed expression to a serializer (JSON.stringify and configured equivalents) — passing it as a call argument counts as "handled" for no-floating-promises/no-misused-promises, and JSON.stringify accepts `any`, so neither the lint gate nor the typecheck gate sees the mistake. The Promise itself serializes as "{}" instead of its awaited value.'
    },
    messages: {
      awaitBeforeSerializing:
        'This argument is a Promise, not its resolved value — {{callee}}() will serialize it as "{}" ' +
        'instead of the awaited result. Await it before passing it in.'
    },
    schema: [
      {
        type: 'object',
        properties: {
          serializers: {
            type: 'array',
            items: {
              type: 'object',
              properties: {
                object: {
                  type: 'string',
                  description: 'The receiver identifier, e.g. "JSON". Omit for a bare function call.'
                },
                method: { type: 'string' }
              },
              required: ['method'],
              additionalProperties: false
            },
            description:
              'Additional known serializers beyond JSON.stringify — e.g. {"method": "toJsonTreeString"} ' +
              'for a bare-call project helper, or {"object": "superjson", "method": "stringify"}.'
          }
        },
        additionalProperties: false
      }
    ]
  },
  defaultOptions: [{}],
  create(context, [rawOptions]) {
    // `defaultOptions: [{}]` types the option object as the empty `{}`; the
    // real shape is the one this rule's own `schema` above declares.
    /** @type {{ serializers?: string[] }} */
    const options = rawOptions
    const services = ESLintUtils.getParserServices(context)
    const checker = services.program.getTypeChecker()
    const serializers = [JSON_STRINGIFY, ...(options.serializers ?? [])]

    return {
      CallExpression(node) {
        const serializer = matchingSerializer(node.callee, serializers)
        if (!serializer) return

        for (const arg of node.arguments) {
          // A spread's element types are checked individually by whatever
          // consumes them; spreading a Promise into a serializer call is
          // not the shape this rule targets (and is vanishingly rare —
          // `JSON.stringify(...args)` is not a real pattern).
          if (arg.type === 'SpreadElement') continue

          let tsNode
          try {
            tsNode = services.esTreeNodeToTSNodeMap.get(arg)
          } catch {
            continue
          }
          const argType = checker.getTypeAtLocation(tsNode)
          if (!isPromiseType(argType, checker)) continue

          const calleeText = serializer.object ? `${serializer.object}.${serializer.method}` : serializer.method
          context.report({ node: arg, messageId: 'awaitBeforeSerializing', data: { callee: calleeText } })
        }
      }
    }
  }
})
