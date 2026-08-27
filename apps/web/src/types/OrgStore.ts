/**
 * The company view's (E6-S7, #812) selector output shapes. Everything here
 * is a projection of what apps/api already served — placement, ordering and
 * roster membership are never recomputed locally (mandate 3). Colocated
 * here, not with the components, per `lucy/no-exported-type-outside-types-dir`.
 */
import type { CompanySummary, DepartmentNode } from '@/types/ChiefApi'
import type { SseChannelState } from '@/types/Sse'

/** -> S3 `WindowDescriptor` plus the panes it should render. */
export interface OrgWindowModel {
  windowId: string
  name: string
  headAccentColor: string | null
  panes: readonly OrgPaneModel[]
}

export interface OrgPaneModel {
  personId: string
  title: string
  name: string
  accentColor: string | null
  running: boolean
}

export interface PersonFooterModel {
  /** 🎯 */
  /** 🧭 */
  /** 📬 — `undefined` until the first mailbox read for this person; the
   * segment renders only when this is a positive number (never `0`, never
   * `undefined`). */
  pendingMailboxCount: number | undefined
}

export interface OrgStoreApi {
  ready: boolean
  /** Exactly the order apps/api served, with people-less departments
   * dropped. Never sorted, never re-parented here: placement is apps/api's
   * answer (mandate 3). */
  windows: readonly OrgWindowModel[]
  /** The department forest exactly as apps/api served it — including
   * departments with nobody in them, which `windows` deliberately drops
   * (a tmux window with no panes is not a window). The structure editor
   * needs those: an empty department is still somewhere to hire into. */
  departments: readonly DepartmentNode[]
  company: CompanySummary | undefined
  footerFor(personId: string): PersonFooterModel
  channel: SseChannelState
}

/** `providers/OrgStoreProvider.tsx`'s internal context shape — exported here
 * (not from the provider module) so `hooks/UseOrgStore.ts`, the Contract's
 * documented entry point, can implement `useOrgStore`/`useOrgPaneMount`
 * itself against this type rather than re-exporting the provider's own
 * hooks (`lucy/no-barrel-re-export`). */
export interface OrgStoreProviderInternalApi {
  store: {
    subscribe(listener: () => void): () => void
    getSnapshot(): OrgStoreApi
  }
  registerMountedPerson(personId: string): void
  unregisterMountedPerson(personId: string): void
}
