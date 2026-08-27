import { ESLintUtils } from '@typescript-eslint/utils'
import { createRule, isEnumLikeType } from '../utils.js'

export default createRule({
  name: 'no-default-in-enum-switch',
  meta: {
    type: 'suggestion',
    docs: {
      description:
        'Disallow default case in switch statements over enum or literal union types'
    },
    messages: {
      noDefault:
        'Do not use a default case in switch statements over enum or literal union types. ' +
        'Handle each case explicitly to leverage exhaustiveness checking.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const services = ESLintUtils.getParserServices(context)
    const checker = services.program.getTypeChecker()

    return {
      SwitchStatement(node) {
        const defaultCase = node.cases.find((c) => c.test === null)
        if (!defaultCase) return

        try {
          const tsNode = services.esTreeNodeToTSNodeMap.get(node.discriminant)
          const type = checker.getTypeAtLocation(tsNode)

          if (isEnumLikeType(type)) {
            context.report({ node: defaultCase, messageId: 'noDefault' })
          }
        } catch {
          // If types are unavailable, silently skip
        }
      }
    }
  }
})
