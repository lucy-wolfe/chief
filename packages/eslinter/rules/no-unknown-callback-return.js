import { createRule } from '../utils.js'

// #866: a callback-seam field typed `=> unknown` (or `=> any`) launders
// promise-ness past every type-aware fence — no-floating-promises and
// no-misused-promises both follow the DECLARED type, not the runtime value,
// so an async function assigned to such a field can be called without
// `await` and have its rejection silently dropped, invisible to both rules
// by construction. This is deliberately the narrow, syntax-only guard: it
// flags the shape (a function-typed property/method signature whose return
// type is exactly `unknown`/`any`) without attempting to determine whether
// the return value is actually discarded anywhere — that would need real
// type-flow analysis, and a guard that needs type-flow analysis to avoid
// false positives is a guard that gets disabled. A genuinely opaque-handle
// return (e.g. a `setInterval`-shaped seam whose handle is consumed, not
// discarded) is a legitimate false positive here; suppress it with a
// one-line `eslint-disable-next-line` explaining why, same as any other
// lucy rule exception in this repo.

function returnsUnknownOrAny(functionTypeNode) {
  const returnType = functionTypeNode.returnType?.typeAnnotation
  if (!returnType) return false
  return returnType.type === 'TSUnknownKeyword' || returnType.type === 'TSAnyKeyword'
}

export default createRule({
  name: 'no-unknown-callback-return',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow `=> unknown` / `=> any` as the return type of a callback-seam field (an interface/type-literal property or method typed as a function) — it launders promise-ness past no-floating-promises/no-misused-promises. Use `void`, `void | Promise<...>`, or an explicit type instead.'
    },
    messages: {
      narrowReturnType:
        'This callback-seam field returns `{{keyword}}`, which hides promise-ness from no-floating-promises/no-misused-promises. ' +
        'Use `void` for a genuinely synchronous seam, or `void | Promise<...>` for one that may be async.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      // A property or method signature inside an interface or an inline
      // object type literal, e.g. `reconcile: (slug: string) => unknown` or
      // `reconcile(slug: string): unknown`.
      TSPropertySignature(node) {
        if (node.typeAnnotation?.typeAnnotation?.type !== 'TSFunctionType') return
        const fnType = node.typeAnnotation.typeAnnotation
        if (!returnsUnknownOrAny(fnType)) return
        context.report({
          node: fnType.returnType.typeAnnotation,
          messageId: 'narrowReturnType',
          data: {
            keyword: fnType.returnType.typeAnnotation.type === 'TSAnyKeyword' ? 'any' : 'unknown'
          }
        })
      },
      TSMethodSignature(node) {
        if (!returnsUnknownOrAny(node)) return
        context.report({
          node: node.returnType.typeAnnotation,
          messageId: 'narrowReturnType',
          data: {
            keyword: node.returnType.typeAnnotation.type === 'TSAnyKeyword' ? 'any' : 'unknown'
          }
        })
      },
      // A standalone function-type alias used as a callback seam, e.g.
      // `type Reconciler = (slug: string) => unknown`.
      TSTypeAliasDeclaration(node) {
        if (node.typeAnnotation.type !== 'TSFunctionType') return
        if (!returnsUnknownOrAny(node.typeAnnotation)) return
        context.report({
          node: node.typeAnnotation.returnType.typeAnnotation,
          messageId: 'narrowReturnType',
          data: {
            keyword:
              node.typeAnnotation.returnType.typeAnnotation.type === 'TSAnyKeyword'
                ? 'any'
                : 'unknown'
          }
        })
      }
    }
  }
})
