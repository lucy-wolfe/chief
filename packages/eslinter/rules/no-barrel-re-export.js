import { createRule } from '../utils.js'

export default createRule({
  name: 'no-barrel-re-export',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow re-export statements and imported binding export forwards outside approved entrypoints'
    },
    messages: {
      noBarrelReExport:
        'Avoid re-exporting from "{{source}}". Import directly from the defining module at each call site.'
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
      filename.endsWith('/packages/foundation/src/index.ts') ||
      filename.endsWith('/packages/voicing/src/index.ts') ||
      filename.endsWith('/packages/launching/src/index.ts') ||
      filename.endsWith('/packages/sandboxing/src/index.ts') ||
      filename.endsWith('/packages/sandboxing/src/shared/index.ts') ||
      filename.endsWith('/packages/billing/src/shared/index.ts') ||
      filename.endsWith('/packages/billing/src/server/index.ts') ||
      filename.endsWith('/apps/api/src/index.ts') ||
      // chief-repo package barrels (this ported rule's allowlist only ever
      // carried terminal-repo paths — every chief workspace package needs its
      // own entry here as it lands a real `src/index.ts` barrel; E0-S5/#756).
      filename.endsWith('/packages/piing/src/index.ts') ||
      filename.endsWith('/packages/piing/src/extensionruntime/index.ts') ||
      // @chief/chiefing's single barrel (E0-S4, #755) — the package's whole
      // public surface is re-exported from here by contract.
      filename.endsWith('/packages/chiefing/src/index.ts') ||
      // The copied Pi extension surface is a second, intentionally narrow
      // chiefing barrel. Its relative-import closure is materialized outside
      // this package, so consumers cannot import its defining modules directly.
      filename.endsWith('/packages/chiefing/src/extensionruntime/index.ts') ||
      // @chief/testing's single barrel (E9-S1, #833).
      filename.endsWith('/packages/testing/src/index.ts')
    if (isAllowedEntrypoint) {
      return {}
    }

    const importedLocalNames = new Set()

    return {
      ImportDeclaration(node) {
        for (const specifier of node.specifiers) {
          if (specifier.local.type === 'Identifier') {
            importedLocalNames.add(specifier.local.name)
          }
        }
      },
      ExportAllDeclaration(node) {
        context.report({
          node,
          messageId: 'noBarrelReExport',
          data: { source: String(node.source.value) }
        })
      },
      ExportNamedDeclaration(node) {
        if (node.source && node.specifiers.length > 0) {
          context.report({
            node,
            messageId: 'noBarrelReExport',
            data: { source: String(node.source.value) }
          })
          return
        }

        const hasImportedBindingSpecifier = node.specifiers.some((specifier) => {
          if (specifier.type !== 'ExportSpecifier') {
            return false
          }
          if (specifier.local.type !== 'Identifier') {
            return false
          }

          return importedLocalNames.has(specifier.local.name)
        })

        if (hasImportedBindingSpecifier) {
          context.report({
            node,
            messageId: 'noBarrelReExport',
            data: { source: 'an imported binding' }
          })
        }
      }
    }
  }
})
