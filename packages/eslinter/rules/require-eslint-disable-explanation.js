import { createRule } from '../utils.js'

export default createRule({
  name: 'require-eslint-disable-explanation',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Require block-based eslint-disable with a substantive explanation comment'
    },
    messages: {
      noInlineDisable:
        'Inline eslint-disable comments are not allowed. ' +
        'Use block-based /* eslint-disable rule-name */ with an explanation comment, ' +
        'followed by /* eslint-enable rule-name */.',
      missingRuleName:
        'Blanket /* eslint-disable */ without a rule name is not allowed. ' +
        'Specify which rule(s) to disable.',
      missingExplanation:
        'eslint-disable block must be followed by an explanation comment ' +
        '(at least 10 characters) before the disabled code.',
      missingMatchingEnable:
        'Every /* eslint-disable {{ruleName}} */ must have a matching ' +
        '/* eslint-enable {{ruleName}} */.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const INLINE_DISABLE =
      /^\s*eslint-disable(?:-next-line|-line)/
    const BLOCK_DISABLE =
      /^\s*eslint-disable(?:\s+|$)/
    const BLOCK_ENABLE =
      /^\s*eslint-enable(?:\s+|$)/

    function extractRuleNames(text, pattern) {
      const afterKeyword = text.replace(pattern, '').trim()
      if (!afterKeyword) return []
      return afterKeyword
        .split(',')
        .map((r) => r.trim())
        .filter(Boolean)
    }

    return {
      Program() {
        const sourceCode =
          context.sourceCode ?? context.getSourceCode()
        const comments = sourceCode.getAllComments()

        const disableComments = []
        const enableComments = []

        for (const comment of comments) {
          const text = comment.value.trim()

          // 1. Ban inline disables
          if (INLINE_DISABLE.test(text)) {
            context.report({
              loc: comment.loc,
              messageId: 'noInlineDisable'
            })
            continue
          }

          // Collect block disables
          if (
            comment.type === 'Block' &&
            BLOCK_DISABLE.test(text)
          ) {
            disableComments.push(comment)
          }

          // Collect block enables
          if (
            comment.type === 'Block' &&
            BLOCK_ENABLE.test(text)
          ) {
            enableComments.push(comment)
          }
        }

        for (const disable of disableComments) {
          const text = disable.value.trim()
          const ruleNames = extractRuleNames(
            text,
            /^\s*eslint-disable\s*/
          )

          // 2. Require rule name
          if (ruleNames.length === 0) {
            context.report({
              loc: disable.loc,
              messageId: 'missingRuleName'
            })
            continue
          }

          // 3. Require explanation comment BEFORE the disabled code
          const disableEnd = disable.loc.end.line

          // Find the first code token after the disable comment
          const tokenAfter = sourceCode.getTokenAfter(disable, {
            includeComments: false
          })
          const firstCodeLine = tokenAfter
            ? tokenAfter.loc.start.line
            : Infinity

          let hasExplanation = false
          let explanationLength = 0

          for (const c of comments) {
            if (c === disable) continue
            if (c.loc.start.line <= disableEnd) continue
            // Stop at comments on or after the first code token
            if (c.loc.start.line >= firstCodeLine) break
            if (c.type === 'Block' && BLOCK_ENABLE.test(c.value.trim())) break
            if (c.type === 'Block' && BLOCK_DISABLE.test(c.value.trim())) break

            const cText = c.value.trim()
            if (cText && !BLOCK_DISABLE.test(cText) && !BLOCK_ENABLE.test(cText)) {
              explanationLength += cText.length
              if (explanationLength >= 10) {
                hasExplanation = true
                break
              }
            }
          }

          if (!hasExplanation) {
            context.report({
              loc: disable.loc,
              messageId: 'missingExplanation'
            })
          }

          // 4. Require matching enable
          for (const ruleName of ruleNames) {
            const hasEnable = enableComments.some((en) => {
              const enRules = extractRuleNames(
                en.value.trim(),
                /^\s*eslint-enable\s*/
              )
              return (
                enRules.includes(ruleName) &&
                en.loc.start.line > disable.loc.start.line
              )
            })

            if (!hasEnable) {
              context.report({
                loc: disable.loc,
                messageId: 'missingMatchingEnable',
                data: { ruleName }
              })
            }
          }
        }
      }
    }
  }
})
