import { createRule } from '../utils.js'
import path from 'path'

// Enforce that .test.ts/.test.tsx files live under a `test/` directory
// (sibling to `src/`) and mirror the `src/` directory structure.
export default createRule({
  name: 'enforce-test-file-location',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Test files (.test.ts/.test.tsx) must live under a test/ directory (sibling to src/), not inside src/.'
    },
    messages: {
      testInsideSrc:
        'Test file "{{fileName}}" must not live inside src/. Move it to the mirrored path under test/. Expected: {{expected}}',
      testOutsideTestDir:
        'Test file "{{fileName}}" must live under a test/ directory that is a sibling to src/.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    const filename = context.filename || context.getFilename()

    if (!/\.test\.tsx?$/.test(filename)) return {}

    return {
      Program(node) {
        const normalized = filename.replace(/\\/g, '/')

        // Check if the file is inside a src/ directory
        const srcIndex = normalized.lastIndexOf('/src/')
        if (srcIndex !== -1) {
          const projectRoot = normalized.substring(0, srcIndex)
          const relativePath = normalized.substring(srcIndex + '/src/'.length)
          const expected = path.posix.join(projectRoot, 'test', relativePath)

          context.report({
            node,
            messageId: 'testInsideSrc',
            data: {
              fileName: path.basename(filename),
              expected
            }
          })
          return
        }

        // Check the file is inside a test/ directory
        const testIndex = normalized.lastIndexOf('/test/')
        if (testIndex === -1) {
          context.report({
            node,
            messageId: 'testOutsideTestDir',
            data: {
              fileName: path.basename(filename)
            }
          })
        }
      }
    }
  }
})
