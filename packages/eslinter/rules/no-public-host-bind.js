import { createRule } from '../utils.js'

// Forbid binding a server to a public interface with an inline host literal.
//
// Dev machines can be publicly reachable (cloud VMs with public IPv6), so a
// '0.0.0.0' / '::' bind during `bun run dev` is a public exposure. The
// sanctioned pattern is the env-derived HTTP_HOST constant in @/common/env
// ('0.0.0.0' only when NODE_ENV === 'production', '127.0.0.1' otherwise), so
// the wildcard literal never appears at a bind site.
//
// Detection is intentionally conservative: it flags only inline public-host
// string literals passed to listen()/serve()-style calls. A host smuggled in
// through a variable is missed; red-lighting HTTP_HOST itself is not.

const PUBLIC_HOSTS = new Set(['0.0.0.0', '::', '[::]', '::0', '0:0:0:0:0:0:0:0'])

// Returns the public-host string when a node is an inline literal for one.
function publicHostLiteral(node) {
  if (!node) return null
  if (node.type === 'Literal' && typeof node.value === 'string' && PUBLIC_HOSTS.has(node.value)) {
    return node.value
  }
  if (node.type === 'TemplateLiteral' && node.expressions.length === 0) {
    const cooked = node.quasis[0].value.cooked
    if (PUBLIC_HOSTS.has(cooked)) return cooked
  }
  return null
}

// Returns the string name of a non-spread object property key, or null.
function propKeyName(prop) {
  if (prop.type !== 'Property') return null
  const key = prop.key
  if (key.type === 'Identifier') return key.name
  if (key.type === 'Literal') return String(key.value)
  return null
}

// Finds a `host`/`hostname`/`ip` property bound to a public-host literal.
function publicHostProperty(node) {
  if (node.type !== 'ObjectExpression') return null
  for (const prop of node.properties) {
    const name = propKeyName(prop)
    if (name !== 'host' && name !== 'hostname' && name !== 'ip') continue
    if (publicHostLiteral(prop.value)) return prop.value
  }
  return null
}

export default createRule({
  name: 'no-public-host-bind',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Forbid binding a server to a public interface (0.0.0.0 / ::) with an inline host literal; bind the env-derived HTTP_HOST from @/common/env instead.'
    },
    messages: {
      noPublicHostBind:
        "Never bind a server to a public interface with an inline '0.0.0.0' / '::' literal — on a publicly reachable dev machine that exposes the port to the internet. Bind the env-derived HTTP_HOST from @/common/env ('0.0.0.0' only when NODE_ENV === 'production', '127.0.0.1' otherwise)."
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    function checkArgs(args) {
      for (const arg of args) {
        const direct = publicHostLiteral(arg)
        if (direct) return arg
        const viaProperty = arg.type === 'ObjectExpression' ? publicHostProperty(arg) : null
        if (viaProperty) return viaProperty
      }
      return null
    }

    return {
      CallExpression(node) {
        const callee = node.callee
        if (node.arguments.length === 0) return

        // `.listen(port, host)` / `.listen({ host })`
        const isListen =
          callee.type === 'MemberExpression' &&
          !callee.computed &&
          callee.property.type === 'Identifier' &&
          callee.property.name === 'listen'

        // `Bun.serve({ hostname })` or bare `serve({ hostname })`
        const isBunServe =
          callee.type === 'MemberExpression' &&
          !callee.computed &&
          callee.object.type === 'Identifier' &&
          callee.object.name === 'Bun' &&
          callee.property.type === 'Identifier' &&
          callee.property.name === 'serve'
        const isBareServe = callee.type === 'Identifier' && callee.name === 'serve'

        if (!isListen && !isBunServe && !isBareServe) return

        const offender = checkArgs(node.arguments)
        if (offender) {
          context.report({ node: offender, messageId: 'noPublicHostBind' })
        }
      },
      NewExpression(node) {
        // `new WebSocketServer({ host })`
        if (node.callee.type !== 'Identifier' || node.callee.name !== 'WebSocketServer') return
        const offender = checkArgs(node.arguments)
        if (offender) {
          context.report({ node: offender, messageId: 'noPublicHostBind' })
        }
      }
    }
  }
})
