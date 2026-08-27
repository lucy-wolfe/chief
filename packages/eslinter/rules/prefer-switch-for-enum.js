import { ESLintUtils } from '@typescript-eslint/utils'
import { createRule, isEnumLikeType } from '../utils.js'

export default createRule({
  name: 'prefer-switch-for-enum',
  meta: {
    type: 'suggestion',
    docs: {
      description:
        'Prefer switch statements over if-else chains when comparing enum or literal union values'
    },
    messages: {
      preferSwitch:
        'Use a switch statement instead of an if-else chain for enum or literal union comparisons. ' +
        'Switch statements provide exhaustiveness checking and are required by project convention.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const services = ESLintUtils.getParserServices(context)
    const checker = services.program.getTypeChecker()

    // Structurally compare two AST nodes (Identifier or MemberExpression)
    function nodesEqual(a, b) {
      if (a.type !== b.type) return false
      if (a.type === 'Identifier') return a.name === b.name
      if (a.type === 'MemberExpression') {
        return (
          !a.computed &&
          !b.computed &&
          nodesEqual(a.object, b.object) &&
          nodesEqual(a.property, b.property)
        )
      }
      return false
    }

    // Heuristic: PascalCase member access like `Status.Active` is likely an enum value
    function looksLikeEnumAccess(node) {
      return (
        node.type === 'MemberExpression' &&
        !node.computed &&
        node.object.type === 'Identifier' &&
        /^[A-Z]/.test(node.object.name)
      )
    }

    // Extract the subject (the variable being compared) from a === expression
    function extractSubject(test) {
      if (test.type !== 'BinaryExpression' || test.operator !== '===') return null

      const { left, right } = test

      // If right is a literal or enum-like access, left is the subject
      if (right.type === 'Literal' || looksLikeEnumAccess(right)) {
        return left
      }
      // If left is a literal or enum-like access, right is the subject
      if (left.type === 'Literal' || looksLikeEnumAccess(left)) {
        return right
      }

      // Fallback: Identifier vs MemberExpression — identifier is the subject
      if (left.type === 'Identifier' && right.type === 'MemberExpression') {
        return left
      }
      if (right.type === 'Identifier' && left.type === 'MemberExpression') {
        return right
      }

      return null
    }

    // Collect all test expressions in an if / else-if chain
    function collectChainTests(node) {
      const tests = []
      let current = node

      while (current && current.type === 'IfStatement') {
        tests.push(current.test)
        current = current.alternate
      }

      return tests
    }

    return {
      IfStatement(node) {
        // Only process the top-level if (skip else-if branches)
        const parent = node.parent
        if (parent && parent.type === 'IfStatement' && parent.alternate === node) return

        const tests = collectChainTests(node)
        if (tests.length < 2) return

        // Extract subjects from each branch
        const subjects = tests.map(extractSubject)
        if (subjects.some((s) => s === null)) return

        // Verify all branches compare the same subject
        const firstSubject = subjects[0]
        if (!subjects.every((s) => nodesEqual(s, firstSubject))) return

        // Check if the subject's type is an enum or literal union
        try {
          const tsNode = services.esTreeNodeToTSNodeMap.get(firstSubject)
          const type = checker.getTypeAtLocation(tsNode)

          if (isEnumLikeType(type)) {
            context.report({ node, messageId: 'preferSwitch' })
          }
        } catch {
          // If types are unavailable, silently skip
        }
      }
    }
  }
})
