import path from 'node:path'

import type { NextConfig } from 'next'

const nextConfig: NextConfig = {
  poweredByHeader: false,
  devIndicators: false,
  /**
   * Pi's libraries are loaded by Node, never bundled.
   *
   * `@earendil-works/pi-ai` reaches Node's builtins through a COMPUTED
   * specifier — `dist/env-api-keys.js` does `dynamicImport("node:" + "fs")`
   * at module scope, deliberately, so browser bundlers cannot follow it. A
   * server bundler cannot resolve it either, so Turbopack replaced each one
   * with a stub that throws `Cannot find module as expression is too dynamic`.
   * Those three calls are fired at import time with no `.catch`, so merely
   * LOADING the module raised three unhandled rejections — and Node's default
   * for an unhandled rejection is to exit. The dev server answered a request
   * and then died, which reached the operator as a `502` on the first screen
   * with eight identical stack traces and nothing about our own code in them.
   *
   * Listing the packages here leaves them as ordinary `require`s resolved by
   * Node at runtime, where a computed specifier is just an import. This is the
   * cause, not the symptom: nothing about the packages is wrong, and patching
   * them to appease a bundler would fork a dependency over a bundling choice.
   */
  serverExternalPackages: [
    '@earendil-works/pi-agent-core',
    '@earendil-works/pi-ai',
    '@earendil-works/pi-coding-agent',
    // The organization extensions import `pi-tui` for their terminal card
    // renderers. This host drops those renderers, but the import is at module
    // scope, so the package is still loaded — by Node, for the same reason as
    // its three siblings above.
    '@earendil-works/pi-tui'
  ],
  /**
   * `@chief/piing` ships its Pi extensions as SOURCE, and that is deliberate.
   *
   * chiefd materializes `packages/piing/extensions/*.ts` into each person's Pi
   * home verbatim; a tmux pane loads those exact files. `server/ExtensionTools`
   * imports the same files so the web host and the tmux pane derive their tool
   * set from ONE registration rather than from two builds of it. Source in a
   * workspace package is what `transpilePackages` is for.
   */
  transpilePackages: ['@chief/piing'],
  // Monorepo tracing root: apps/web lives two levels below the repo root
  // (apps/web/next.config.ts), so Next must be told where the workspace
  // actually ends for file tracing / output standalone bundling to resolve
  // sibling workspace packages (e.g. @chief/chiefing) correctly.
  outputFileTracingRoot: path.join(import.meta.dirname, '../../')
}

export default nextConfig
