import { createRule } from '../utils.js'

// Enforce the store pattern: ALL direct database access (drizzle query building)
// must live in a store (a stores/ directory). Every other layer — controllers,
// services, jobs, helpers, utils, anything — must call a store method instead of
// touching the db. This keeps DB access in one testable, swappable layer per app
// (apps/api, apps/backend) and the business layers query-free.
//
// Exempt: stores/ (the data layer), and db/ + schema/ + migrations/ (where the
// drizzle client and the table definitions necessarily live).
//
// Forbidden everywhere else (in the apps where this rule is enabled):
//   1. a VALUE import from `drizzle-orm` (the query operators: eq, and, sql, …).
//      Type-only imports (`import type { PostgresJsDatabase }`) are allowed — they
//      carry no query code, just db-handle types.
//   2. calling the query builder on a `db`/`tx` handle: db.select/insert/update/
//      delete/execute/transaction (catches a table-only query with no operator).
const EXEMPT_DIR = /\/(stores|db|schema|migrations)\//
const DB_HANDLES = new Set(['db', 'tx', 'trx'])
const DB_METHODS = new Set(['select', 'insert', 'update', 'delete', 'execute', 'transaction'])

export default createRule({
  name: 'no-direct-db-outside-stores',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Forbid direct DB access (drizzle-orm imports / db query builder) outside a stores/ directory. Every layer (controllers, services, jobs, helpers) must go through a store.'
    },
    messages: {
      import:
        'Do not import from drizzle-orm outside a store. Move the query into a store (a stores/ directory) and call the store method here. (Type-only `import type` is allowed.)',
      query:
        'Direct db.{{method}}() query building is not allowed outside a store. Move it into a store (a stores/ directory) and call the store method here.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()
    if (EXEMPT_DIR.test(filename)) {
      return {}
    }

    return {
      ImportDeclaration(node) {
        const source = node.source.value
        if (
          typeof source !== 'string' ||
          (source !== 'drizzle-orm' && !source.startsWith('drizzle-orm/'))
        ) {
          return
        }
        // Allow type-only imports: `import type ...` or every specifier `type`.
        if (node.importKind === 'type') {
          return
        }
        const specifiers = node.specifiers
        const allTypeOnly =
          specifiers.length > 0 &&
          specifiers.every((s) => s.type === 'ImportSpecifier' && s.importKind === 'type')
        if (allTypeOnly) {
          return
        }
        context.report({ node, messageId: 'import' })
      },
      MemberExpression(node) {
        if (
          node.object &&
          node.object.type === 'Identifier' &&
          DB_HANDLES.has(node.object.name) &&
          node.property &&
          node.property.type === 'Identifier' &&
          DB_METHODS.has(node.property.name)
        ) {
          context.report({
            node,
            messageId: 'query',
            data: { method: node.property.name }
          })
        }
      }
    }
  }
})
