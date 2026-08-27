import { createRule } from '../utils.js'

// Exported types and interfaces must live in a /types/ directory.
// Unexported (file-local) types are fine anywhere.
export default createRule({
  name: 'no-exported-type-outside-types-dir',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Exported types and interfaces must be defined in a /types/ directory'
    },
    messages: {
      moveToTypes:
        'Exported type "{{ name }}" must be defined in a /types/ directory. ' +
        'Keep it file-local (unexported) or move it to the appropriate /types/ directory.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()

    // Already in a types directory — no violation
    if (/\/types\//.test(filename)) return {}

    // Skip .d.ts declaration files
    if (filename.endsWith('.d.ts')) return {}

    return {
      ExportNamedDeclaration(node) {
        const decl = node.declaration
        if (!decl) return

        if (
          decl.type === 'TSTypeAliasDeclaration' ||
          decl.type === 'TSInterfaceDeclaration' ||
          decl.type === 'TSEnumDeclaration'
        ) {
          context.report({
            node: decl,
            messageId: 'moveToTypes',
            data: { name: decl.id.name }
          })
        }
      }
    }
  }
})
