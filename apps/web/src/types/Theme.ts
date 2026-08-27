/**
 * Names of the `--chief-*` CSS custom properties declared in
 * `src/app/globals.css`, ported from the tmux styling constants in
 * `src/organization/org-tmux.ts:616-638`. A union type (not an enum — see
 * `lucy/no-default-in-enum-switch` / `lucy/prefer-switch-for-enum`) so later
 * stories can look a token name up exhaustively.
 */
export type ChiefThemeToken =
  | '--chief-status-bg'
  | '--chief-tab-inactive-bg'
  | '--chief-index-inactive-bg'
  | '--chief-index-active-bg'
  | '--chief-status-left-bg'
  | '--chief-pane-border'
  | '--chief-neutral-accent'
