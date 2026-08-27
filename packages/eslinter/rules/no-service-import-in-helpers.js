import { createRule } from '../utils.js'

export default createRule({
  name: 'no-service-import-in-helpers',
  meta: {
    type: 'problem',
    docs: {
      description: 'Forbid importing from services/ in helpers/ or utils/ files'
    },
    messages: {
      forbidden:
        'Helpers and utils must not import from services/. The dependency direction is Service → Helper → Utils.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()
    const isHelperOrUtil =
      /\/helpers\//.test(filename) || /\/utils\//.test(filename)

    if (!isHelperOrUtil) return {}

    return {
      ImportDeclaration(node) {
        const source = node.source.value
        if (
          typeof source === 'string' &&
          /\/services\//.test(source)
        ) {
          context.report({ node, messageId: 'forbidden' })
        }
      }
    }
  }
})
