import { createRule } from '../utils.js'

export default createRule({
  name: 'no-indexed-type-access',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow indexed type access (e.g. Foo["bar"]) on interfaces; import and reference the target type directly'
    },
    messages: {
      useDirectType:
        'Avoid indexed type access "{{typeText}}". Import and reference the target interface or type directly.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      TSIndexedAccessType(node) {
        // Only flag concrete literal indexed access (e.g. Foo['bar'])
        // Allow generic patterns like M[K], T[number], T[keyof T], T[typeof x]
        if (node.indexType.type !== 'TSLiteralType') return

        const sourceCode = context.sourceCode ?? context.getSourceCode()
        const typeText = sourceCode.getText(node)
        context.report({ node, messageId: 'useDirectType', data: { typeText } })
      }
    }
  }
})
