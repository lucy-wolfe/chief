import { createRule } from '../utils.js'

// Flag .ts/.tsx files that contain no actual code (only comments,
// whitespace, or nothing at all). These are dead stubs that should
// be deleted.
export default createRule({
  name: 'no-empty-file',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow .ts/.tsx files with no actual code (only comments or whitespace)'
    },
    messages: {
      emptyFile:
        'This file contains no code (only comments or whitespace). Delete it.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()

    // Only check .ts / .tsx files
    if (!/\.tsx?$/.test(filename)) return {}

    // Skip .d.ts declaration files (often auto-generated)
    if (filename.endsWith('.d.ts')) return {}

    return {
      'Program:exit'(node) {
        // If the AST body has any statements, the file has code
        if (node.body.length > 0) return

        context.report({
          node,
          messageId: 'emptyFile'
        })
      }
    }
  }
})
