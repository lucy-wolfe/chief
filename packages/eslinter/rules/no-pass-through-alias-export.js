import { createRule } from '../utils.js'

export default createRule({
  name: 'no-pass-through-alias-export',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow alias import pass-through exports; import from defining module at call site'
    },
    messages: {
      noPassThroughAliasExport:
        'Avoid pass-through alias export for "{{exportedName}}". Import "{{exportedName}}" directly from its defining module instead of importing "{{localName}}" and re-exporting it via const.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename ?? ''
    const isAllowedEntrypoint =
      filename.endsWith('/packages/core/src/shared/index.ts') ||
      filename.endsWith('/packages/core/src/client/index.ts') ||
      filename.endsWith('/packages/core/src/lucy/index.ts') ||
      filename.endsWith('/packages/core/src/server/index.ts') ||
      filename.endsWith('/apps/api/src/index.ts')
    if (isAllowedEntrypoint) {
      return {}
    }

    const aliasedImportLocals = new Map()

    return {
      ImportDeclaration(node) {
        for (const specifier of node.specifiers) {
          if (
            specifier.type === 'ImportSpecifier' &&
            specifier.imported.type === 'Identifier' &&
            specifier.local.type === 'Identifier' &&
            specifier.imported.name !== specifier.local.name
          ) {
            aliasedImportLocals.set(specifier.local.name, specifier.imported.name)
          }
        }
      },
      ExportNamedDeclaration(node) {
        const declaration = node.declaration
        if (!declaration || declaration.type !== 'VariableDeclaration') return
        if (declaration.kind !== 'const') return

        for (const declarator of declaration.declarations) {
          if (
            declarator.id.type !== 'Identifier' ||
            !declarator.init ||
            declarator.init.type !== 'Identifier'
          ) {
            continue
          }

          const importedName = aliasedImportLocals.get(declarator.init.name)
          if (!importedName) continue
          if (declarator.id.name !== importedName) continue

          context.report({
            node: declarator,
            messageId: 'noPassThroughAliasExport',
            data: {
              exportedName: declarator.id.name,
              localName: declarator.init.name
            }
          })
        }
      }
    }
  }
})
