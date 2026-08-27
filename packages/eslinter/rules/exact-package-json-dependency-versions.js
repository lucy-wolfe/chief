
import { createRule } from '../utils.js'

const DEPENDENCY_OBJECT_KEYS = new Set([
  'dependencies',
  'devDependencies',
  'peerDependencies',
  'optionalDependencies'
])

/**
 * @param {*} keyNode
 * @returns {string | null}
 */
function getJsonPropertyKeyName(keyNode) {
  if (!keyNode) return null
  switch (keyNode.type) {
    case 'JSONLiteral':
      if (typeof keyNode.value === 'string') return keyNode.value
      if (typeof keyNode.value === 'number') return String(keyNode.value)
      return null
    case 'JSONIdentifier':
      return keyNode.name
    default:
      return null
  }
}

/**
 * @param {string} version
 * @returns {boolean}
 */
function isExactPinnedSemver(version) {
  return /^\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u.test(
    version
  )
}

/**
 * @param {string} version
 * @returns {boolean} true when the version should be reported */
function violatesExactVersionPolicy(version) {
  if (version.startsWith('workspace:')) return false
  if (version.startsWith('file:')) return false
  if (version.startsWith('link:')) return false
  if (version.startsWith('git+')) return false
  if (version.startsWith('http://') || version.startsWith('https://')) return false
  if (version.startsWith('^') || version.startsWith('~')) return true
  if (/^(>=|<=|>|<)/u.test(version)) return true
  if (version.includes('||')) return true
  if (/\s+-\s+/u.test(version)) return true
  if (version === 'latest' || version === 'next' || version === 'canary') return true
  if (/\.x(\.|$)/iu.test(version)) return true
  if (version.includes('*')) return true
  return !isExactPinnedSemver(version)
}

/**
 * @param {*} propNode
 * @returns {boolean}
 */
function isVersionFieldInDependencyContexts(propNode) {
  if (propNode.type !== 'JSONProperty') return false
  let current = propNode.parent
  while (current) {
    if (current.type === 'JSONProperty') {
      const key = getJsonPropertyKeyName(current.key)
      if (key !== null && DEPENDENCY_OBJECT_KEYS.has(key)) return true
      if (key === 'overrides') return true
    }
    current = current.parent
  }
  return false
}

export default createRule({
  name: 'exact-package-json-dependency-versions',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Require pinned dependency versions in package.json (no ^, ~, or other semver ranges)'
    },
    messages: {
      rangeNotAllowed:
      'Use an exact pinned version for "{{key}}" (no ^, ~, ranges, or tag aliases like latest). See exact-package-json-dependency-versions rule.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()
    if (!filename.endsWith('package.json')) return {}

    return {
      JSONProperty(node) {
        // `@typescript-eslint/utils`'s `RuleListener` types every visitor key
        // outside the TS/ESTree node map as `(node: never) => void`, and
        // `JSONProperty` comes from `jsonc-eslint-parser` — so every field
        // read off this parameter was a type error the moment anything
        // actually checked this file. The two helpers above already take `*`
        // for exactly this reason; this restates it at the seam rather than
        // pulling a second parser's AST types into a package that does not
        // depend on it.
        /** @type {*} */
        const propNode = node
        if (propNode.value.type !== 'JSONLiteral') return
        if (typeof propNode.value.value !== 'string') return
        if (!isVersionFieldInDependencyContexts(propNode)) return

        const version = propNode.value.value
        if (!violatesExactVersionPolicy(version)) return

        const depKey = getJsonPropertyKeyName(propNode.key) ?? '(unknown)'

        context.report({
          node: propNode.value,
          messageId: 'rangeNotAllowed',
          data: { key: depKey }
        })
      }
    }
  }
})
