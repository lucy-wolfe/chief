import { createRule } from '../utils.js'

// Enforce that `@test/*` imports are only used inside test/ files,
// and that test files don't use relative imports to reach into test/.
export default createRule({
  name: 'enforce-test-import-alias',
  meta: {
    type: 'problem',
    docs: {
      description:
        'The @test/* import alias may only be used in test files (under test/). Source files must never import from @test/*.'
    },
    messages: {
      testAliasInSrc:
        '@test/* imports are not allowed in source files. Only test files (under test/) may import from @test/*.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename || context.getFilename()).replace(/\\/g, '/')
    const isTestFile = filename.includes('/test/')

    if (isTestFile) return {}

    return {
      ImportDeclaration(node) {
        if (node.source.value.startsWith('@test/')) {
          context.report({ node: node.source, messageId: 'testAliasInSrc' })
        }
      },
      ImportExpression(node) {
        if (
          node.source.type === 'Literal' &&
          typeof node.source.value === 'string' &&
          node.source.value.startsWith('@test/')
        ) {
          context.report({ node: node.source, messageId: 'testAliasInSrc' })
        }
      }
    }
  }
})
