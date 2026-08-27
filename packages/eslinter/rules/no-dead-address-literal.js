import { createRule } from '../utils.js'

export default createRule({
  name: 'no-dead-address-literal',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow zero/dead/burn address literals; use DEAD_ADDRESSES_SET from @tribes-terminal/core/shared'
    },
    messages: {
      noDeadAddress:
        'Do not use zero/dead address literals as defaults or fallbacks. Use DEAD_ADDRESSES_SET from @tribes-terminal/core/shared for address filtering, or handle the null case explicitly.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = (context.filename ?? '').replaceAll('\\', '/')

    // Allow usage in the constants file where DEAD_ADDRESSES_SET is defined
    const isConstantsFile = filename.endsWith('/shared/common/constants.ts')
    if (isConstantsFile) {
      return {}
    }

    const DEAD_ADDRESSES = new Set([
      '0x0000000000000000000000000000000000000000',
      '0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
      '0x000000000000000000000000000000000000dead'
    ])

    return {
      Literal(node) {
        if (typeof node.value !== 'string') return
        const lower = node.value.toLowerCase()
        if (DEAD_ADDRESSES.has(lower)) {
          context.report({
            node,
            messageId: 'noDeadAddress'
          })
        }
      }
    }
  }
})
