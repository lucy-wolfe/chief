/**
 * Pure `CompanyTree` → `OrgWindowModel[]` projection (E6-S7, #812). No I/O,
 * no placement decision: apps/api's `GET /companies/:companyKey/tree` has already
 * applied `manifest.departmentOrder`/`manifest.peopleOrder` and resolved
 * placement (E5-S4's `CompanyDirectoryService`) — this module only walks
 * the served tree in the order it arrived and drops departments that
 * arrived with no people (mandate 3).
 */
import type { DepartmentNode } from '@/types/ChiefApi'
import type { OrgPaneModel, OrgWindowModel } from '@/types/OrgStore'

/** Depth-first pre-order over the served tree — the flat window order a
 * recursive department hierarchy renders as tmux-equivalent tabs. A node
 * with zero people is dropped as a window (no window renders empty), but
 * its children are still walked: a department with no people of its own can
 * still be the parent of one that does. */
export function buildWindows(
  roots: readonly DepartmentNode[],
  runningOverrides: ReadonlyMap<string, boolean>
): OrgWindowModel[] {
  // A FOREST, not one root: apps/api serves `departments` as an array. It is
  // one element today (the executive root), and taking the array is what makes
  // that a fact about the data rather than an assumption in this file.
  const windows: OrgWindowModel[] = []
  for (const root of roots) walk(root, runningOverrides, windows)
  return windows
}

function walk(
  node: DepartmentNode,
  runningOverrides: ReadonlyMap<string, boolean>,
  out: OrgWindowModel[]
): void {
  if (node.people.length > 0) {
    const headPane = node.people.find((person) => person.id === node.headPersonId)
    out.push({
      windowId: node.id,
      name: node.name,
      headAccentColor: headPane?.accent ?? null,
      panes: node.people.map((person): OrgPaneModel => ({
        personId: person.id,
        title: person.title,
        name: person.name,
        accentColor: person.accent ?? null,
        // The tree carries placement and identity; whether a person is
        // RUNNING is `/people`'s answer, and this map is it. A person the
        // roster has not reported on yet is not running.
        running: runningOverrides.get(person.id) ?? false
      }))
    })
  }
  for (const child of node.children) walk(child, runningOverrides, out)
}
