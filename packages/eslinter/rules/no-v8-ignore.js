import { createRule } from '../utils.js'

export default createRule({
  name: 'no-v8-ignore',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow v8 coverage ignore comments (/* v8 ignore file */, /* v8 ignore start */, /* v8 ignore next */) ' +
        'without a substantive explanation. Write tests instead of skipping coverage.'
    },
    messages: {
      noV8IgnoreFile:
        '/* v8 ignore file */ requires a substantive explanation comment (at least 10 characters) ' +
        'immediately before or after the directive. Write tests instead of excluding coverage.',
      noV8IgnoreBlock:
        'v8 ignore comments require a substantive explanation comment ' +
        '(at least 10 characters) immediately before or after the directive.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const V8_IGNORE = /^\s*v8 ignore\s+(file|start|next|stop)/

    return {
      Program() {
        const sourceCode = context.sourceCode ?? context.getSourceCode()
        const comments = sourceCode.getAllComments()

        for (let i = 0; i < comments.length; i++) {
          const comment = comments[i]
          const text = comment.value.trim()

          if (!V8_IGNORE.test(text)) continue

          const isFileIgnore = /^\s*v8 ignore file/.test(text)
          const hasExplanation = checkExplanation(comments, i)

          if (!hasExplanation) {
            context.report({
              loc: comment.loc,
              messageId: isFileIgnore ? 'noV8IgnoreFile' : 'noV8IgnoreBlock'
            })
          }
        }
      }
    }

    function checkExplanation(comments, ignoreIndex) {
      const ignoreLine = comments[ignoreIndex].loc.end.line

      // Check comment immediately before
      if (ignoreIndex > 0) {
        const prev = comments[ignoreIndex - 1]
        if (prev.loc.end.line >= ignoreLine - 1) {
          const prevText = prev.value.trim()
          if (!V8_IGNORE.test(prevText) && prevText.length >= 10) {
            return true
          }
        }
      }

      // Check comment immediately after
      if (ignoreIndex < comments.length - 1) {
        const next = comments[ignoreIndex + 1]
        if (next.loc.start.line <= ignoreLine + 1) {
          const nextText = next.value.trim()
          if (!V8_IGNORE.test(nextText) && nextText.length >= 10) {
            return true
          }
        }
      }

      return false
    }
  }
})
