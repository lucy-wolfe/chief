'use client'

/**
 * The Contract's named entry point (`hooks/UseOrgStore.ts` / `useOrgStore()`,
 * E6-S7 #812). The store class and its provider live in
 * `providers/OrgStoreProvider.tsx`; this module reads the provider's
 * exported context directly rather than re-exporting its hooks
 * (`lucy/no-barrel-re-export`).
 */
import { useContext, useEffect, useSyncExternalStore } from 'react'

import { OrgStoreContext } from '@/providers/OrgStoreProvider'
import type { OrgStoreApi } from '@/types/OrgStore'

export function useOrgStore(): OrgStoreApi {
  const context = useContext(OrgStoreContext)
  if (!context) throw new Error('useOrgStore must be used within OrgStoreProvider')
  return useSyncExternalStore(
    context.store.subscribe,
    context.store.getSnapshot,
    context.store.getSnapshot
  )
}

/** Registers a pane as currently mounted: the mailbox store for `personId`
 * joins the doc subscription and its footer count is fetched once
 * immediately. Unregisters on unmount. */
export function useOrgPaneMount(personId: string): void {
  const context = useContext(OrgStoreContext)
  if (!context) throw new Error('useOrgPaneMount must be used within OrgStoreProvider')
  useEffect(() => {
    context.registerMountedPerson(personId)
    return () => context.unregisterMountedPerson(personId)
  }, [context, personId])
}
