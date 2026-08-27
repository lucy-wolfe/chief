import '@/app/globals.css'

import type { Metadata } from 'next'
import type { ReactElement, ReactNode } from 'react'

import { ApiSessionProvider } from '@/providers/ApiSessionProvider'
import { CompanyEventsProvider } from '@/providers/CompanyEventsProvider'

export const metadata: Metadata = {
  title: 'Chief',
  description: 'The web tmux — a browser client for a Chief company, live over SSE.'
  // No `icons` key, deliberately. `app/icon.svg` next to this file is Next's
  // own metadata-file convention: it is served at `/icon.svg` and the matching
  // `<link rel="icon">` is emitted into every page's head automatically. The
  // app previously shipped no favicon at all and browsers logged a 404 for
  // `/favicon.ico`; a hand-written `<link>` or an `icons` entry here would be a
  // second, drift-prone declaration of a path the framework already owns.
}

export default function RootLayout({ children }: { children: ReactNode }): ReactElement {
  return (
    <html lang="en">
      <body>
        {/* Mounted HERE, not per route. `/c/[companyKey]` — the one route whose
            whole job is live events — mounted `OrgStoreProvider` without it
            and threw "must be used within CompanyEventsProvider" on every
            company page load. It reads only the session this layout already
            provides, so there is nothing a route can supply that this cannot,
            and no route can forget it. The dev playground still injects its
            own fixture deps inside this one, which wins by being nearer. */}
        <ApiSessionProvider>
          <CompanyEventsProvider>{children}</CompanyEventsProvider>
        </ApiSessionProvider>
      </body>
    </html>
  )
}
