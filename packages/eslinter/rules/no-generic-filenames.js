import { createRule } from '../utils.js'

export default createRule({
  name: 'no-generic-filenames',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow generic filenames like types.ts, utils.ts, helpers.ts; use feature-scoped names'
    },
    messages: {
      noGenericFilename:
        'Rename "{{basename}}" to a feature-scoped name (e.g. scheduling.ts, pricing.ts).'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename ?? ''
    const basename = filename.split('/').pop() ?? ''
    const BANNED = ['types.ts', 'utils.ts', 'helpers.ts']

    if (BANNED.includes(basename)) {
      return {
        Program(node) {
          context.report({
            node,
            messageId: 'noGenericFilename',
            data: { basename }
          })
        }
      }
    }

    return {}
  }
})
