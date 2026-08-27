import { createRule } from '../utils.js'

// #855: a sweep (#847) found and fixed every subprocess-spawning test that
// raced a tight deadline, and correctly reported the set closed -- that was
// true when it swept. An hour later new code (#798) produced a fourth
// instance the sweep could not have seen, because it did not exist yet. A
// sweep is a snapshot; this rule is the structural form, so a new instance
// cannot arrive unnoticed.
//
// TWO MECHANISMS, BOTH COVERED, DIFFERENT REMEDY (per the issue):
// - BLOCKING (execFileSync, spawnSync, Bun.spawnSync): these block the
//   event loop, so nothing -- not the enclosing test's own timeout, not a
//   package-level testTimeout -- can preempt a genuinely wedged child. The
//   deadline must live on the spawn's OWN options object, checked purely
//   locally regardless of nesting.
// - ASYNC (exec, execFile, fork, spawn, Bun.spawn, `new RpcClient(...)`, and
//   a reviewed registry of plain function calls documented to spawn --
//   currently `startCompanyDaemon` (`@chief/testing`)):
//   nothing blocks; the test runner CAN preempt an async hang, so the
//   deadline must live on the enclosing it()/test() call instead. A spawn
//   is frequently wrapped in a named helper function CALLED from the test
//   rather than written inline (e.g. `FakeRpcChild.test.ts`'s
//   `clientWithScript()`) -- direct lexical-nesting alone would false-
//   positive on every one of those, so this rule also traces one level of
//   named-function indirection within the same file: if the spawn's
//   nearest named-function ancestor is itself called from a timed
//   it()/test() anywhere in the file, that satisfies the requirement.
//   This is call-GRAPH tracing within one file, not type-flow analysis --
//   deliberately bounded to stay a rule people keep enabled rather than
//   disable after a false positive.
//
// EXPORTED functions are exempt from the "no enclosing test found" report
// (found via a real false positive, not hypothesized): `NodeSpawnPort.ts`'s
// exported `nodeSpawnPort` factory is called from TWO different test files
// (`CompanyController.test.ts`, `CompanyLifecycleService.test.ts`), each
// wrapping it in their OWN local helper before a timed `it()` ever calls
// that. Tracing across files, or more than one level of local indirection,
// is out of reach for a same-file syntactic rule -- flagging an exported
// factory's callers as "definitely uncovered" would be confidently wrong,
// not conservatively cautious, so this rule treats them as unknown instead.
//
// KNOWN LIMITATION (a false NEGATIVE, not a false positive): tracing stops
// at one level of named-function indirection, and only within the same
// file (and is skipped entirely for exported functions, per above). A
// spawn wrapped two calls deep in NON-exported helpers, or in a helper
// imported from another file, is invisible to this rule. Hooks
// (`beforeAll`/`beforeEach`) are not treated as timed contexts -- none of
// the currently-known spawning tests use them; a spawn placed there is
// either lexically direct (caught) or falls through to `asyncOutsideTest`
// (a loud false positive is safer than silently trusting an unverified
// shape -- fix by moving the spawn into the test body it belongs to, or
// extend this rule if that becomes a real pattern).
//
// NOT A GUARANTEE OF CONVERGENCE: this is a static presence check. #860
// records a suite that fails at EXACTLY its deadline regardless of the
// deadline's value under adverse concurrency (pipe-buffer starvation, not
// slowness) -- a deadline stops a hang from being silent, it does not
// prove the work completes. "Has a deadline" and "converges" are different
// claims; this rule only makes the first one checkable.

const BLOCKING_CHILD_PROCESS_FNS = new Set(['execFileSync', 'spawnSync'])
const ASYNC_CHILD_PROCESS_FNS = new Set(['execFile', 'exec', 'fork', 'spawn'])
const CHILD_PROCESS_SOURCES = new Set(['node:child_process', 'child_process'])
const RPC_CLIENT_SOURCES = new Set(['@earendil-works/pi-coding-agent'])

// A REVIEWED REGISTRY of plain function calls (not `new` construction)
// documented to spawn a real subprocess internally -- add an entry when a
// new such dependency joins, matching the discipline
// scripts/test/sql-only-state.test.mjs's ALLOWLIST already uses in this
// repo, and the analogous NewExpression check RPC_CLIENT_SOURCES above
// uses for `new RpcClient(...)`. Not name-sniffing: only names imported
// from one of these exact sources count.
const KNOWN_SPAWNING_FUNCTIONS = new Map([
  // packages/testing/src/CompanyDaemon.ts: spawns a real `chiefd run
  // --serve-only` subprocess -- it opens a company database, mints two
  // keypairs and mounts the whole route table, so a suite that boots it needs
  // a deadline. `@/CompanyDaemon` is the internal alias packages/testing's own
  // tests import it through; `@chief/testing` is the public barrel every
  // consumer outside this package uses. The registry once also carried
  // `startDocstoreDaemon` (`@/DocstoreDaemon`); the docstore-only daemon mode
  // and its harness are deleted, so the entry went with them.
  ['@/CompanyDaemon', new Set(['startCompanyDaemon'])],
  ['@chief/testing', new Set(['startCompanyDaemon'])]
])

function hasTimeoutProperty(objectExpression) {
  return objectExpression.properties.some((property) => {
    if (property.type !== 'Property' || property.computed) return false
    const key = property.key
    const name = key.type === 'Identifier' ? key.name : key.type === 'Literal' ? key.value : undefined
    return name === 'timeout'
  })
}

function callHasOwnTimeoutOption(callExpression) {
  return callExpression.arguments.some(
    (arg) => arg.type === 'ObjectExpression' && hasTimeoutProperty(arg)
  )
}

function isTestOrItRoot(node) {
  return node.type === 'Identifier' && (node.name === 'it' || node.name === 'test')
}

/** `it(...)`, `test(...)`, and chained forms (`it.only(...)`,
 * `test.concurrent.skip(...)`, etc.) all root at an `it`/`test` identifier
 * through a chain of MemberExpressions. */
function isTestCallCallee(callee) {
  if (callee.type === 'Identifier') return isTestOrItRoot(callee)
  let node = callee
  while (node.type === 'MemberExpression') node = node.object
  return isTestOrItRoot(node)
}

function findEnclosingTestCall(node) {
  let current = node.parent
  while (current) {
    if (current.type === 'CallExpression' && isTestCallCallee(current.callee)) return current
    current = current.parent
  }
  return undefined
}

/** vitest/bun:test: `it(name, fn, timeout)` (a bare number) or
 * `it(name, fn, { timeout })`. A named identifier (e.g.
 * `SPAWN_DEADLOCK_TIMEOUT_MS`) is trusted at face value -- resolving its
 * literal value would need type-flow analysis this rule deliberately
 * avoids; a named constant at this position is already the sanctioned
 * convention (see MainDispatch.test.ts), not a loophole. */
function testCallHasExplicitTimeout(testCallExpression) {
  const third = testCallExpression.arguments[2]
  if (!third) return false
  if (third.type === 'Literal' && typeof third.value === 'number') return true
  if (third.type === 'Identifier') return true
  if (third.type === 'ObjectExpression') return hasTimeoutProperty(third)
  return false
}

/** The nearest enclosing named function -- a `function name() {}`
 * declaration, or a `const name = () => {}` / `const name = function () {}`
 * assignment -- if any, walking up from `node`. Also reports whether that
 * function is directly exported (`export function name() {}` /
 * `export const name = () => {}`): an exported function is a shared
 * harness factory that other FILES may call (e.g.
 * `apps/api/test/harness/NodeSpawnPort.ts`'s `nodeSpawnPort`, consumed by
 * two different `*.test.ts` files) -- tracing across files is out of this
 * rule's reach by design, so an exported function's callers are treated as
 * unknown rather than "definitely uncovered." */
function nearestNamedFunctionAncestor(node) {
  let current = node.parent
  while (current) {
    if (current.type === 'FunctionDeclaration' && current.id) {
      const exported = current.parent?.type === 'ExportNamedDeclaration'
      return { name: current.id.name, exported }
    }
    if (
      (current.type === 'FunctionExpression' || current.type === 'ArrowFunctionExpression') &&
      current.parent?.type === 'VariableDeclarator' &&
      current.parent.id.type === 'Identifier'
    ) {
      const declaration = current.parent.parent
      const exported = declaration?.type === 'VariableDeclaration' && declaration.parent?.type === 'ExportNamedDeclaration'
      return { name: current.parent.id.name, exported }
    }
    current = current.parent
  }
  return undefined
}

export default createRule({
  name: 'no-unbounded-spawn-in-test',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Require an explicit deadline on every subprocess spawn inside a test -- a blocking spawn (execFileSync/spawnSync/Bun.spawnSync) needs its own timeout option (nothing else can preempt it); an async spawn (exec/execFile/fork/spawn/Bun.spawn/RpcClient) needs the enclosing it()/test() -- directly or through one level of named-function indirection -- to carry one.'
    },
    messages: {
      blockingNeedsOwnTimeout:
        '{{name}}() blocks the event loop -- nothing can preempt a wedged child, so the deadline must live on this call\'s own options object (`timeout: <ms>`), not on the enclosing test.',
      asyncNeedsTestTimeout:
        '{{name}}() spawns asynchronously and its enclosing it()/test() has no explicit timeout -- add a third argument (a number of ms, or `{ timeout }`) so a genuine hang fails loudly instead of running out the test runner\'s default budget.',
      asyncOutsideTest:
        '{{name}}() spawns asynchronously outside any it()/test() this rule can trace (directly, or through one level of a named helper function called from a timed test) -- if this runs during a test, give the enclosing test an explicit timeout.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const childProcessLocalNames = new Map() // local name -> 'blocking' | 'async'
    const rpcClientLocalNames = new Set()
    const knownSpawningFunctionLocalNames = new Set()
    const pendingAsyncSpawns = [] // { node, name, containingFunctionName }
    const timedCalledNames = new Set() // named-function calls seen inside a timed test

    function recordCallUsage(node) {
      if (node.callee.type !== 'Identifier') return
      const enclosingTest = findEnclosingTestCall(node)
      if (!enclosingTest) return
      if (testCallHasExplicitTimeout(enclosingTest)) {
        timedCalledNames.add(node.callee.name)
      }
    }

    function checkAsyncSpawn(node, name) {
      const testCall = findEnclosingTestCall(node)
      if (testCall) {
        if (!testCallHasExplicitTimeout(testCall)) {
          context.report({ node, messageId: 'asyncNeedsTestTimeout', data: { name } })
        }
        return
      }
      // Not directly nested -- defer to Program:exit, once every call site
      // in the file has been recorded, to check the one-level indirection.
      pendingAsyncSpawns.push({ node, name, containingFunction: nearestNamedFunctionAncestor(node) })
    }

    return {
      ImportDeclaration(node) {
        const source = node.source.value
        const isChildProcess = CHILD_PROCESS_SOURCES.has(source)
        const isRpcClient = RPC_CLIENT_SOURCES.has(source)
        const knownSpawningNames = KNOWN_SPAWNING_FUNCTIONS.get(source)
        if (!isChildProcess && !isRpcClient && !knownSpawningNames) return
        for (const specifier of node.specifiers) {
          if (specifier.type !== 'ImportSpecifier') continue
          const importedName =
            specifier.imported.type === 'Identifier' ? specifier.imported.name : specifier.imported.value
          if (isChildProcess) {
            if (BLOCKING_CHILD_PROCESS_FNS.has(importedName)) {
              childProcessLocalNames.set(specifier.local.name, 'blocking')
            } else if (ASYNC_CHILD_PROCESS_FNS.has(importedName)) {
              childProcessLocalNames.set(specifier.local.name, 'async')
            }
          } else if (isRpcClient && importedName === 'RpcClient') {
            rpcClientLocalNames.add(specifier.local.name)
          } else if (knownSpawningNames?.has(importedName)) {
            knownSpawningFunctionLocalNames.add(specifier.local.name)
          }
        }
      },

      CallExpression(node) {
        recordCallUsage(node)
        const callee = node.callee

        // Bun.spawn / Bun.spawnSync
        if (
          callee.type === 'MemberExpression' &&
          !callee.computed &&
          callee.object.type === 'Identifier' &&
          callee.object.name === 'Bun' &&
          callee.property.type === 'Identifier'
        ) {
          if (callee.property.name === 'spawnSync') {
            if (!callHasOwnTimeoutOption(node)) {
              context.report({ node, messageId: 'blockingNeedsOwnTimeout', data: { name: 'Bun.spawnSync' } })
            }
            return
          }
          if (callee.property.name === 'spawn') {
            checkAsyncSpawn(node, 'Bun.spawn')
          }
          return
        }

        if (callee.type !== 'Identifier') return

        if (knownSpawningFunctionLocalNames.has(callee.name)) {
          checkAsyncSpawn(node, callee.name)
          return
        }

        const kind = childProcessLocalNames.get(callee.name)
        if (!kind) return

        if (kind === 'blocking') {
          if (!callHasOwnTimeoutOption(node)) {
            context.report({ node, messageId: 'blockingNeedsOwnTimeout', data: { name: callee.name } })
          }
          return
        }
        checkAsyncSpawn(node, callee.name)
      },

      NewExpression(node) {
        if (node.callee.type !== 'Identifier' || !rpcClientLocalNames.has(node.callee.name)) return
        checkAsyncSpawn(node, node.callee.name)
      },

      'Program:exit'() {
        for (const { node, name, containingFunction } of pendingAsyncSpawns) {
          if (containingFunction?.exported) continue
          if (containingFunction && timedCalledNames.has(containingFunction.name)) continue
          context.report({ node, messageId: 'asyncOutsideTest', data: { name } })
        }
      }
    }
  }
})
