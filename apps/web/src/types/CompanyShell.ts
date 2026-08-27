/** Public types for the company shell.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */
import type { ReactNode } from 'react'

import type { TreePerson } from '@/types/ChiefApi'

/** One row of the department rail, flattened out of the tree. */
export interface RailDepartment {
  readonly id: string
  readonly name: string
  /** How deep in the tree, so the rail can show shape without hiding rows. */
  readonly depth: number
  readonly state: 'active' | 'paused'
  readonly headPersonId: string
  /** People of THIS unit, never its subtree. */
  readonly peopleCount: number
}

/** What the operator has clicked. */
export type ShellSelection =
  | { readonly kind: 'company' }
  | { readonly kind: 'department'; readonly departmentId: string }
  | { readonly kind: 'person'; readonly personId: string }

/** What to render.
 *
 * Three states, three questions: what does this company look like, what is
 * this department doing, what is this one agent saying. */
export type ShellView =
  | { readonly kind: 'company'; readonly departments: readonly RailDepartment[] }
  | {
      readonly kind: 'department'
      readonly departmentId: string
      readonly departmentName: string
      readonly state: 'active' | 'paused'
      readonly headPersonId: string
      /** One column per person, side by side. */
      readonly columns: readonly TreePerson[]
    }
  | {
      readonly kind: 'person'
      readonly person: TreePerson
      readonly departmentId: string
      readonly departmentName: string
    }

/** What the rail needs to draw itself and report a click. */
export interface CompanyRailProps {
  readonly departments: readonly RailDepartment[]
  readonly people: readonly TreePerson[]
  readonly selection: ShellSelection
  /** Who is hosted right now, from the host's own converged roster. */
  readonly running: ReadonlySet<string>
  onSelect(selection: ShellSelection): void
}

/** What the shell's main area needs.
 *
 * `renderAgent` and `renderOverview` are the caller's: the shell decides SHAPE
 * and has no opinion about what an agent pane or an overview is. Wiring the
 * pane into the layout would make every layout test drag a conversation, a
 * stream and a transport behind it; wiring the overview in would drag the org
 * store, the api client and five staffing verbs behind it for the same reason.
 *
 * `renderOverview` takes no arguments deliberately. The company branch of
 * `ShellView` carries `RailDepartment` rows — a flattened display projection —
 * and the overview needs the unflattened `DepartmentNode` forest, which does
 * not belong in a type whose job is describing a layout. The
 * caller already holds all of it. */
export interface CompanyShellViewProps {
  readonly view: ShellView
  renderAgent(person: TreePerson): ReactNode
  /** The company branch's actionable content: the structure rail. The
   * branch used to be one sentence telling the operator to pick something
   * else, which is the whole company view doing nothing on first paint. */
  renderOverview(): ReactNode
}
