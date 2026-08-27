/**
 * The web shell's presentational data model (E6-S3, #808). It renders exactly
 * what its props describe — placement, ordering and roster membership are the
 * server's answer (mandate 3), never recomputed here.
 *
 * #751/R10: THE SPLIT MODEL IS GONE, and this file is where it used to live.
 * E6-S3 promised user-driven splits, and the audit filed them as unreachable
 * because the only controls left were on a `dev/tmux` page. The right answer
 * turned out not to be "wire the controls up": the tmux-shaped surface those
 * controls belonged to — `CompanyView`, `PaneGrid`, `SessionChrome`,
 * `WindowTabs`, `usePaneLayout`, `SlotPicker` — was deleted, and the web is
 * now a company rail, department columns, and one agent view. In that shape a
 * user-arranged pane tree has nothing to arrange: which agents you see is the
 * org's answer, and the department column IS the grouping a split used to
 * approximate by hand.
 *
 * So `SplitDirection` and `PaneLayoutNode` are deleted rather than kept
 * "until someone wires them up" — a type with no producer and no consumer is
 * not a feature in waiting, it is a claim the codebase makes about itself and
 * cannot honour. `PaneDescriptor.kind` loses its `'slot'` member with them:
 * a slot was a not-yet-assigned position in a split tree, and there are no
 * split trees. See DECISIONS.md, 2026-08-09.
 *
 * `WindowDescriptor` is deleted for the same reason (#751 knip sweep). The
 * live window model is `OrgWindowModel` in `types/OrgStore.ts`, which
 * `OrgStoreProvider` builds and the department column renders; the only code
 * that ever produced a `WindowDescriptor` was a pair of adapters in
 * `utils/OrgSelectors.ts` that nothing called. The note that used to hang off
 * its `name` field is worth keeping without the type: a window name is
 * rendered exactly as chiefd serves it, because the rule that rewrites `.`
 * and `:` exists only because TMUX refuses them, and it lives where the tmux
 * command is built — `chief-cli/src/actuate/` (`safe_window_name`) since
 * #751/P8. A browser has no such constraint, so a second copy here could only
 * disagree with the real actuator.
 */

export interface PaneDescriptor {
  /** The personId this pane renders. */
  paneId: string
  /** Person title — the pane's chip text. */
  title: string
  /** From the server; `null` (never absent) renders the neutral accent. */
  accentColor: string | null
  kind: 'person'
}
