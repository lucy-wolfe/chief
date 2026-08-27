/**
 * What the company shell shows, given a tree and a selection.
 *
 * # Pure, because the layout rules are the product
 *
 * The shell has three states — nothing selected, a department selected, one
 * agent selected — and each answers a different question about a company. Those
 * rules are worth testing on their own, so they live here as functions over
 * data rather than inside a component where they can only be tested by
 * rendering one.
 *
 * # The rail is flat, and that is a decision
 *
 * A company's departments are a TREE, and the rail lists them flattened in
 * tree order with a depth per row. A nested rail hides a department inside a
 * collapsed parent, and an operator looking for a department they created a
 * minute ago should not have to guess which ancestor swallowed it. Depth keeps
 * the shape visible without hiding anything.
 */
import type { CompanyTree, DepartmentNode, TreePerson } from '@/types/ChiefApi'
import type { RailDepartment, ShellSelection, ShellView } from '@/types/CompanyShell'
import { isNullish } from '@/utils/Nullish'

/** Every department, in tree order, each with how deep it sits. */
export function railDepartments(tree: CompanyTree): RailDepartment[] {
  const rows: RailDepartment[] = []
  const walk = (node: DepartmentNode, depth: number): void => {
    rows.push({
      id: node.id,
      name: node.name,
      depth,
      state: node.state,
      headPersonId: node.headPersonId,
      // The people of THIS unit, not its subtree. A parent showing its
      // children's headcount reads as its own, and a department of one with
      // three sub-units would look like a department of ten.
      peopleCount: node.people.length
    })
    for (const child of node.children) walk(child, depth + 1)
  }
  for (const root of tree.departments) walk(root, 0)
  return rows
}

/** Everybody in the company, in tree order. */
export function railPeople(tree: CompanyTree): TreePerson[] {
  const people: TreePerson[] = []
  const walk = (node: DepartmentNode): void => {
    people.push(...node.people)
    for (const child of node.children) walk(child)
  }
  for (const root of tree.departments) walk(root)
  return people
}

function findDepartment(tree: CompanyTree, id: string): DepartmentNode | undefined {
  const walk = (node: DepartmentNode): DepartmentNode | undefined => {
    if (node.id === id) return node
    for (const child of node.children) {
      const found = walk(child)
      if (!isNullish(found)) return found
    }
    return undefined
  }
  for (const root of tree.departments) {
    const found = walk(root)
    if (!isNullish(found)) return found
  }
  return undefined
}

/** The department a person is assigned to, or nothing. */
export function departmentOf(tree: CompanyTree, personId: string): DepartmentNode | undefined {
  const walk = (node: DepartmentNode): DepartmentNode | undefined => {
    if (node.people.some((person) => person.id === personId)) return node
    for (const child of node.children) {
      const found = walk(child)
      if (!isNullish(found)) return found
    }
    return undefined
  }
  for (const root of tree.departments) {
    const found = walk(root)
    if (!isNullish(found)) return found
  }
  return undefined
}

/**
 * What to render for a selection.
 *
 * A selection that no longer exists resolves to `company` rather than an empty
 * pane: a department can be removed or a person offboarded while somebody is
 * looking at them, and a shell that kept showing an empty column would leave
 * an operator staring at a unit that is gone with no way back.
 */
export function shellView(tree: CompanyTree, selection: ShellSelection): ShellView {
  if (selection.kind === 'person') {
    const department = departmentOf(tree, selection.personId)
    if (isNullish(department)) return { kind: 'company', departments: railDepartments(tree) }
    const person = department.people.find((entry) => entry.id === selection.personId)
    if (isNullish(person)) return { kind: 'company', departments: railDepartments(tree) }
    // One agent, full width, and the department it belongs to — an operator
    // reading one agent still needs to know whose team it is on.
    return { kind: 'person', person, departmentId: department.id, departmentName: department.name }
  }

  if (selection.kind === 'department') {
    const department = findDepartment(tree, selection.departmentId)
    if (isNullish(department)) return { kind: 'company', departments: railDepartments(tree) }
    return {
      kind: 'department',
      departmentId: department.id,
      departmentName: department.name,
      state: department.state,
      headPersonId: department.headPersonId,
      // One column per person of THIS unit. Sub-department members are not
      // pulled in: they have their own column set under their own unit, and
      // flattening a subtree into one row of columns would make a two-person
      // department look like a floor of twenty.
      columns: department.people
    }
  }

  return { kind: 'company', departments: railDepartments(tree) }
}
