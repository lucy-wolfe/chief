// ESM ESLint plugin providing custom rules for this monorepo
import noJsonStringify from './rules/no-json-stringify.js'
import noRawNullCheck from './rules/no-raw-null-check.js'
import noPassThroughAliasExport from './rules/no-pass-through-alias-export.js'
import noOptionalNullable from './rules/no-optional-nullable.js'
import noBarrelReExport from './rules/no-barrel-re-export.js'
import noGenericFilenames from './rules/no-generic-filenames.js'
import enforceWebClientServiceSuffix from './rules/enforce-web-client-service-suffix.js'
import noFetchOutsideScopedHelpers from './rules/no-fetch-outside-scoped-helpers.js'
import noIndexedTypeAccess from './rules/no-indexed-type-access.js'
import noConsoleUsage from './rules/no-console-usage.js'
import noInlineZodInfer from './rules/no-inline-zod-infer.js'
import noProcessEnv from './rules/no-process-env.js'
import noAsyncInUtils from './rules/no-async-in-utils.js'
import noDefaultInEnumSwitch from './rules/no-default-in-enum-switch.js'
import preferSwitchForEnum from './rules/prefer-switch-for-enum.js'
import enforceHandleAction from './rules/enforce-handle-action.js'
import noRawZodBigint from './rules/no-raw-zod-bigint.js'
import noServiceImportInHelpers from './rules/no-service-import-in-helpers.js'
import requireEslintDisableExplanation from './rules/require-eslint-disable-explanation.js'
import noDeadAddressLiteral from './rules/no-dead-address-literal.js'
import enforceUrlConstructorTwoArgs from './rules/enforce-url-constructor-two-args.js'
import noExportedTypeOutsideTypesDir from './rules/no-exported-type-outside-types-dir.js'
import noEmptyFile from './rules/no-empty-file.js'
import enforceAsJsonResponse from './rules/enforce-as-json-response.js'
import noResponseReturnInServices from './rules/no-response-return-in-services.js'
import enforceTestFileLocation from './rules/enforce-test-file-location.js'
import enforceTestImportAlias from './rules/enforce-test-import-alias.js'
import exactPackageJsonDependencyVersions from './rules/exact-package-json-dependency-versions.js'
import noV8Ignore from './rules/no-v8-ignore.js'
import noNodeEnvDefault from './rules/no-node-env-default.js'
import noDirectDbOutsideStores from './rules/no-direct-db-outside-stores.js'
import noPublicHostBind from './rules/no-public-host-bind.js'
import noPromiseToSerializer from './rules/no-promise-to-serializer.js'
import noUnknownCallbackReturn from './rules/no-unknown-callback-return.js'
import noUnboundedSpawnInTest from './rules/no-unbounded-spawn-in-test.js'

export default {
  rules: {
    'no-json-stringify': noJsonStringify,
    'no-raw-null-check': noRawNullCheck,
    'no-pass-through-alias-export': noPassThroughAliasExport,
    'no-optional-nullable': noOptionalNullable,
    'no-barrel-re-export': noBarrelReExport,
    'no-generic-filenames': noGenericFilenames,
    'enforce-web-client-service-suffix': enforceWebClientServiceSuffix,
    'no-fetch-outside-scoped-helpers': noFetchOutsideScopedHelpers,
    'no-indexed-type-access': noIndexedTypeAccess,
    'no-console-usage': noConsoleUsage,
    'no-inline-zod-infer': noInlineZodInfer,
    'no-process-env': noProcessEnv,
    'no-async-in-utils': noAsyncInUtils,
    'no-default-in-enum-switch': noDefaultInEnumSwitch,
    'prefer-switch-for-enum': preferSwitchForEnum,
    'enforce-handle-action': enforceHandleAction,
    'no-raw-zod-bigint': noRawZodBigint,
    'no-service-import-in-helpers': noServiceImportInHelpers,
    'require-eslint-disable-explanation': requireEslintDisableExplanation,
    'no-dead-address-literal': noDeadAddressLiteral,
    'enforce-url-constructor-two-args': enforceUrlConstructorTwoArgs,
    'no-exported-type-outside-types-dir': noExportedTypeOutsideTypesDir,
    'no-empty-file': noEmptyFile,
    'enforce-as-json-response': enforceAsJsonResponse,
    'no-response-return-in-services': noResponseReturnInServices,
    'enforce-test-file-location': enforceTestFileLocation,
    'enforce-test-import-alias': enforceTestImportAlias,
    'exact-package-json-dependency-versions': exactPackageJsonDependencyVersions,
    'no-v8-ignore': noV8Ignore,
    'no-node-env-default': noNodeEnvDefault,
    'no-direct-db-outside-stores': noDirectDbOutsideStores,
    'no-public-host-bind': noPublicHostBind,
    'no-promise-to-serializer': noPromiseToSerializer,
    'no-unknown-callback-return': noUnknownCallbackReturn,
    'no-unbounded-spawn-in-test': noUnboundedSpawnInTest
  }
}
