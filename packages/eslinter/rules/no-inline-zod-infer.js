import { createRule } from '../utils.js'

export default createRule({
  name: 'no-inline-zod-infer',
  meta: {
    type: 'problem',
    docs: {
      description:
        'Disallow z.infer<typeof ...> in function signatures; export a named type next to the schema instead'
    },
    messages: {
      noInlineZodInfer:
        'Do not use z.infer<typeof ...> in function signatures. Export a named type next to the schema and use that instead.'
    },
    schema: []
  },
  defaultOptions: [],
  create(context) {
    return {
      TSTypeReference(node) {
        // Match z.infer qualified name
        if (node.typeName.type !== 'TSQualifiedName') return
        if (node.typeName.left.type !== 'Identifier' || node.typeName.left.name !== 'z') return
        if (node.typeName.right.type !== 'Identifier' || node.typeName.right.name !== 'infer')
          return

        // Allow in type alias declarations: type Foo = z.infer<typeof FooSchema>
        if (node.parent.type === 'TSTypeAliasDeclaration') return

        // Only flag z.infer<typeof ...> — allow z.infer<TGenericParam> in generic classes
        // `typeParameters` was the pre-v6 alias for this field and is gone
        // from the AST types: the `??` arm could only ever be `undefined`.
        const typeArgs = node.typeArguments
        if (
          !typeArgs ||
          typeArgs.params.length === 0 ||
          typeArgs.params[0].type !== 'TSTypeQuery'
        ) {
          return
        }

        // Check if ancestor chain includes a function parameter or return type annotation.
        // Annotated explicitly: inferring from `node.parent` alone gives the narrower
        // "parent of a TSTypeReference" union, which the `current = current.parent`
        // walk below immediately widens past.
        /** @type {import('@typescript-eslint/utils').TSESTree.Node | undefined} */
        let current = node.parent
        let isInFunctionSignature = false
        while (current) {
          // Return type annotation
          if (current.type === 'TSTypeAnnotation') {
            const annotationParent = current.parent
            if (!annotationParent) break

            // Return type of function/method/arrow
            if (
              annotationParent.type === 'FunctionDeclaration' ||
              annotationParent.type === 'FunctionExpression' ||
              annotationParent.type === 'ArrowFunctionExpression' ||
              annotationParent.type === 'TSDeclareFunction' ||
              annotationParent.type === 'TSMethodSignature' ||
              annotationParent.type === 'TSFunctionType'
            ) {
              isInFunctionSignature = true
              break
            }

            // Parameter type annotation
            if (annotationParent.type === 'Identifier' || annotationParent.type === 'AssignmentPattern') {
              const paramCandidate =
                annotationParent.type === 'AssignmentPattern' ? annotationParent.parent : annotationParent
              if (paramCandidate.parent) {
                const grandParent = paramCandidate.parent
                if (
                  grandParent.type === 'FunctionDeclaration' ||
                  grandParent.type === 'FunctionExpression' ||
                  grandParent.type === 'ArrowFunctionExpression' ||
                  grandParent.type === 'TSDeclareFunction' ||
                  grandParent.type === 'TSMethodSignature' ||
                  grandParent.type === 'TSFunctionType'
                ) {
                  isInFunctionSignature = true
                  break
                }
              }
            }
            break
          }
          current = current.parent
        }

        if (isInFunctionSignature) {
          context.report({ node, messageId: 'noInlineZodInfer' })
        }
      }
    }
  }
})
