//! tmux layout geometry: pure arithmetic over a window's size and its panes.
//!
//! # Why this is client code
//!
//! A layout string is `checksum,WxH,x,y{cell,cell,…}` — tmux's own wire format
//! for "put these panes here". Nothing about it is a fact about a company, and
//! a browser rendering the same roster computes nothing resembling it. It is
//! the purest example of the split: chiefd said WHO, this file decides WHERE
//! on the glass.
//!
//! Ported from `chiefd-core/src/runtime/reconcile_plan.rs`'s layout math.
//! Ported means rewritten here; this crate links none of the backend crates.
//! The three tests below are that module's own, carried over unchanged.
//!
//! # The shape
//!
//! Panes lay out as a GRID: `ceil(sqrt(n))` columns over as many rows as that
//! needs, the rows balanced with the extra people on top. One person is one
//! cell, two are side by side, four are a 2x2, five are three over two. Every
//! cell is separated from its neighbour by exactly one column or row of
//! divider, which is what [`distributed_sizes`] subtracts before it splits.
//!
//! This USED to be one row of columns up to five people, wrapping only at six,
//! and that row is what the operator reported as a department click that did
//! not show the team. See [`people_body`] for the measurement it cost.
//!
//! # The rail
//!
//! A company window may reserve a full-height column on the left for the
//! operator's sidebar ([`Rail`]). It is a parameter of this function and not a
//! pane laid beside the result, because a tmux layout string is ABSOLUTE: it
//! enumerates every pane in the window with explicit geometry, and a pane the
//! string does not name is a pane the next `select-layout` fights. The people
//! panes then divide what is left of the width, by the same two rules above.
//! `rail: None` produces byte-identical output to a window with no rail, which
//! is what holds the shared placement goldens.
//!
//! # THE PANE IDS IN A LAYOUT STRING ARE DECORATIVE
//!
//! MEASURED against tmux 3.3a on 2026-08-14, because the opposite was assumed
//! by every function in this file and by every caller of them:
//!
//! ```text
//! window order: %0 %1 %2
//! applied:      120x40,0,0{70x40,0,0,2,24x40,71,0,1,24x40,96,0,0}
//! tmux gives:   %0 -> 70 cols,  %1 -> 24,  %2 -> 24
//! tmux dumps:   120x40,0,0{70x40,0,0,0,24x40,71,0,1,24x40,96,0,2}
//! ```
//!
//! The 70-column cell was given to `%2` BY NAME and tmux handed it to `%0`.
//! `select-layout` parses the cell TREE and then walks the window's own pane
//! list in order, assigning the Nth pane to the Nth cell; the trailing id in
//! each cell is read as part of the format and then overwritten on dump. So a
//! layout string cannot say WHICH pane goes where — only what the sequence of
//! boxes is.
//!
//! Everything here therefore emits cells in the caller's pane order and the
//! caller must supply the panes in the WINDOW'S OWN ORDER (`list-panes`), rail
//! first. Naming a pane in a cell is not a request tmux honours, and code that
//! reads as though it is will silently favour whoever happens to be first —
//! which is exactly the defect the retired focused layout shipped with: every
//! click enlarged the first person in the window, never the one clicked.
//!
//! **The rule outlives that function.** [`organization_tmux_layout`] has the
//! same property: its cells are all the same size, so a mis-ordered list is
//! invisible there — and that is precisely why the next person to build a
//! layout string here must be told, rather than left to discover it from a
//! screenshot of the wrong person full screen.

/// Why a layout could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LayoutError(String);

/// Is this a tmux pane id (`%7`)?
fn is_pane_id(candidate: &str) -> bool {
    candidate
        .strip_prefix('%')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Distribute `total` columns or rows across `count` cells, leaving a one-cell
/// gap between adjacent cells.
///
/// # Errors
/// [`LayoutError`] when the window cannot hold that many cells at all.
pub fn distributed_sizes(total: i64, count: i64) -> Result<Vec<i64>, LayoutError> {
    if count < 1 {
        return Ok(Vec::new());
    }
    let usable = total - count + 1;
    if usable < count {
        return Err(LayoutError(format!("Tmux window is too small for {count} columns")));
    }
    let base = usable / count;
    let remainder = usable % count;
    Ok((0..count).map(|index| base + i64::from(index < remainder)).collect())
}

/// Lay panes out in a single row of width `width`, starting at column
/// `origin` and at vertical offset `top`.
///
/// `origin` is what lets the people panes sit to the RIGHT of a rail: a tmux
/// layout cell carries absolute coordinates, so an inner row that assumed it
/// began at column 0 would draw every person underneath the rail.
fn layout_row(
    width: i64,
    height: i64,
    origin: i64,
    top: i64,
    pane_ids: &[&str],
) -> Result<String, LayoutError> {
    let count = i64::try_from(pane_ids.len()).unwrap_or(i64::MAX);
    let sizes = distributed_sizes(width, count)?;
    let mut left = origin;
    let mut cells: Vec<String> = Vec::with_capacity(pane_ids.len());
    for (index, pane_id) in pane_ids.iter().enumerate() {
        if !is_pane_id(pane_id) {
            return Err(LayoutError(format!("Invalid tmux pane id '{pane_id}'")));
        }
        let pane_width = sizes[index];
        cells.push(format!("{pane_width}x{height},{left},{top},{}", &pane_id[1..]));
        left += pane_width + 1;
    }
    Ok(if cells.len() == 1 {
        cells.into_iter().next().unwrap_or_default()
    } else {
        format!("{width}x{height},{origin},{top}{{{}}}", cells.join(","))
    })
}

/// tmux's layout checksum: a 16-bit rotate-and-add over the layout body.
fn layout_checksum(body: &str) -> String {
    let mut checksum: u16 = 0;
    for character in body.chars() {
        checksum = (checksum >> 1) | ((checksum & 1) << 15);
        let unit = u16::try_from(u32::from(character) & 0xffff).unwrap_or_default();
        checksum = checksum.wrapping_add(unit);
    }
    format!("{checksum:04x}")
}

/// The narrowest a rail is ever drawn.
///
/// tmux 3.3a accepts `resize-pane -x 1`, so this is a DRAWING decision and not
/// a tmux limit: four columns is what the two-arrow collapse control and a
/// one-glyph count need, and it is wide enough to be a click target rather than
/// a pixel. A rail is never zero columns, because a tmux pane cannot be.
pub const RAIL_COLLAPSED_COLUMNS: i64 = 4;

/// The operator's sidebar, as the layout sees it: one pane and its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rail<'a> {
    /// The tmux pane id the rail program runs in.
    pub pane_id: &'a str,
    /// How many columns it occupies, before clamping.
    pub columns: i64,
}

impl Rail<'_> {
    /// The width this rail is drawn at before any fit is considered.
    ///
    /// Floored at [`RAIL_COLLAPSED_COLUMNS`] and NOT capped: the requested
    /// width is the width the operator dragged the divider to, and a fixed
    /// fraction-of-the-glass cap would silently fight that drag on every
    /// converge pass. What bounds the rail instead is whether the people
    /// actually FIT beside it — see [`organization_tmux_layout`]'s collapsed
    /// retry, which is the one mechanism, applied only when it is needed.
    const fn floored(self) -> i64 {
        if self.columns < RAIL_COLLAPSED_COLUMNS {
            RAIL_COLLAPSED_COLUMNS
        } else {
            self.columns
        }
    }
}

/// Build a tmux-native layout string for one window's ordered panes, with an
/// optional full-height rail reserved on the left.
///
/// Pure given the window dimensions and the resolved concrete pane ids; the
/// caller supplies the live dimensions and the ids tmux minted.
///
/// `rail: None` is byte-identical to a window that has no rail at all, which is
/// what holds the shared placement goldens.
///
/// # The rail KEEPS ITS WIDTH, and this is where that is enforced
///
/// When the people cannot fit beside the rail at its requested width, the rail
/// used to be retried COLLAPSED — four columns — and that retry is what the
/// operator watched happen: they clicked a sleeping person, the panel showed,
/// the person arrived, and the sidebar they had sized became `Depa` / `Peop`,
/// permanently, because every later pass laid it the same way.
///
/// The retry is now a LADDER, not a cliff. It steps down one column at a time
/// and stops at the FIRST width that fits, so a squeeze costs the rail exactly
/// the columns it had to give up and never a single one more. A company still
/// always lays out — reaching the collapsed width is the last rung, not the
/// first alternative — and every narrowing says so in the log, because a
/// sidebar that silently changed size is precisely the bug this closes.
///
/// # Errors
/// [`LayoutError`] on non-positive dimensions, an empty pane set, a window too
/// small for the pane count even with the rail collapsed, or a malformed pane
/// id.
pub fn organization_tmux_layout(
    width: i64,
    height: i64,
    rail: Option<Rail<'_>>,
    pane_ids: &[&str],
) -> Result<String, LayoutError> {
    if width < 1 || height < 1 {
        return Err(LayoutError("Tmux layout requires positive integer dimensions".to_owned()));
    }
    if pane_ids.is_empty() {
        return Err(LayoutError("Tmux layout requires at least one pane".to_owned()));
    }
    let Some(rail) = rail else {
        let body = people_body(width, height, 0, pane_ids)?;
        return Ok(format!("{},{}", layout_checksum(&body), body));
    };
    if !is_pane_id(rail.pane_id) {
        return Err(LayoutError(format!("Invalid tmux pane id '{}'", rail.pane_id)));
    }
    let requested = rail.floored();
    let mut last = LayoutError("Tmux layout requires at least one pane".to_owned());
    let mut columns = requested;
    while columns >= RAIL_COLLAPSED_COLUMNS {
        match railed_body(width, height, columns, rail.pane_id, pane_ids) {
            Ok(body) => {
                if columns != requested {
                    tracing::warn!(
                        event = "layout.rail.narrowed",
                        requested,
                        applied = columns,
                        width,
                        panes = pane_ids.len(),
                        "the people did not fit beside the rail at the width the operator \
                         set; it was narrowed by the least that made them fit, and NOT \
                         collapsed"
                    );
                }
                return Ok(format!("{},{}", layout_checksum(&body), body));
            }
            Err(error) => last = error,
        }
        columns -= 1;
    }
    Err(last)
}

// TOMBSTONE: `focused_tmux_layout` and `FOCUS_MIN_READABLE_COLUMNS`, deleted
// 2026-08-14 with the 24-column compromise they existed for. THE WHOLE LAYOUT
// FAMILY IS CLOSED; do not bring back a narrower version of this.
//
// They laid `[rail | the clicked person | everybody else at 24 columns]`,
// because the standing ruling was that the rail must never disappear and a
// layout was the only way to keep it. The operator retired that compromise:
// "when I click on people the person I selected gets merged with the CEO" — the
// clicked person did get the wide cell, and 24 columns of somebody else beside
// them is exactly what "merged" meant. A layout string enumerates every pane in
// the window (module header), so it can only ever NARROW a bystander, never
// hide one. No amount of narrowing fixes that, which is why no successor to
// this function can work.
//
// `resize-pane -Z` replaced it and was rejected in turn — zoom is per-WINDOW
// and takes the RAIL off the glass with everybody else. What removes a pane
// from a window is `break-pane`/`join-pane` and nothing else, so a person shown
// alone is a person MOVED into a window of their own beside a rail of their own
// (`sidebar::effects::show_person`, `placement::FOCUS_WINDOW_ID`). That window
// is laid by `organization_tmux_layout` with exactly one person in it — this
// module needs no focused mode at all.
//
// What must NOT be re-learned from a screenshot: tmux fills a layout's cells
// from the window's own pane list BY POSITION and ignores the ids the string
// names. That measurement is in this module's header, it still governs
// `organization_tmux_layout`, and it is why every caller re-reads `list-panes`
// order before it builds a layout.

/// How many COLUMNS a grid of `count` people is drawn in.
///
/// The smallest square that holds them, `ceil(sqrt(count))`, computed by
/// integer growth so no float rounding can put a pane in a column that does not
/// exist. It is the same shape tmux's own `select-layout tiled` picks, so the
/// layout this product applies and the one an operator gets by re-tiling the
/// window with their own hands agree instead of fighting.
///
/// Two people stay SIDE BY SIDE — `ceil(sqrt(2))` is 2 — which is the one shape
/// that was already measured good on the glass and must not move.
fn grid_columns(count: usize) -> usize {
    let mut columns = 1;
    while columns * columns < count {
        columns += 1;
    }
    columns
}

/// The people half of a window: a GRID, laid out inside `width` columns
/// beginning at column `origin`.
///
/// # Why a grid, and not the row this used to be
///
/// It laid ONE ROW of columns for up to five people and wrapped only at six.
/// MEASURED 2026-08-18 on a 220-column glass with a 26-column rail: three live
/// people in one department got 64x53 panes, four get 48 wide, five get 38 — a
/// column of text narrower than a git diff, at fifty-three rows of height that
/// nothing fills. That is what the operator reported as a department click
/// that "does not show the team": the team WAS all there, in slivers.
///
/// A grid spends the window's two dimensions instead of one. The same five
/// people on the same glass become three columns over two rows, 64x26 — a pane
/// somebody can read.
///
/// # The rows are BALANCED, and the top one takes the extra
///
/// `count` people over `rows` rows, remainder to the top: five in three columns
/// is three over two. Chunking greedily by column count instead would give
/// seven people three, three and one, and a lone pane on the bottom row is
/// stretched to the full width — the "one person merged with everybody else"
/// shape this module's tombstone already refuses to draw.
///
/// # Errors
/// [`LayoutError`] when the window is too short to hold the rows the grid
/// needs, or too narrow to hold its columns. Failing closed is deliberate: a
/// layout that does not fit is drawn by tmux as something nobody chose.
fn people_body(
    width: i64,
    height: i64,
    origin: i64,
    pane_ids: &[&str],
) -> Result<String, LayoutError> {
    let count = pane_ids.len();
    let mut rows = count.div_ceil(grid_columns(count).max(1));
    // A ROW OF ONE IS NOT A ROW. Three people in two columns would be two on
    // top and ONE underneath — and a layout string enumerates every pane, so
    // that last one is stretched to the full width of the window while its two
    // colleagues share the row above. That is precisely the "the person I
    // selected gets merged with everybody else" shape this module's tombstone
    // refuses to draw. One row fewer costs three people nothing: they go side
    // by side at a third of the glass each, which is not a sliver.
    while rows > 1 && count / rows < 2 {
        rows -= 1;
    }
    if rows <= 1 {
        return layout_row(width, height, origin, 0, pane_ids);
    }
    let row_count = i64::try_from(rows).unwrap_or(i64::MAX);
    // Every row needs at least one line, and every divider between two of them
    // needs one more.
    if height < row_count * 2 - 1 {
        return Err(LayoutError(format!("Tmux window is too short for {rows} rows")));
    }
    let heights = distributed_sizes(height, row_count)?;
    let base = count / rows;
    let remainder = count % rows;
    let mut cells: Vec<String> = Vec::with_capacity(rows);
    let mut taken = 0;
    let mut top = 0;
    for (index, row_height) in heights.iter().enumerate() {
        let in_row = base + usize::from(index < remainder);
        cells.push(layout_row(width, *row_height, origin, top, &pane_ids[taken..taken + in_row])?);
        taken += in_row;
        top += row_height + 1;
    }
    Ok(format!("{width}x{height},{origin},0[{}]", cells.join(",")))
}

/// A window split horizontally into the rail cell and the people cell.
fn railed_body(
    width: i64,
    height: i64,
    columns: i64,
    rail_pane: &str,
    pane_ids: &[&str],
) -> Result<String, LayoutError> {
    let inner_width = width - columns - 1;
    if inner_width < 1 {
        return Err(LayoutError(format!(
            "Tmux window is too narrow for a {columns}-column sidebar"
        )));
    }
    let people = people_body(inner_width, height, columns + 1, pane_ids)?;
    let rail_cell = format!("{columns}x{height},0,0,{}", &rail_pane[1..]);
    Ok(format!("{width}x{height},0,0{{{rail_cell},{people}}}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{distributed_sizes, organization_tmux_layout, Rail, RAIL_COLLAPSED_COLUMNS};

    /// A rail of `columns` in pane `%9`, the id every rail test below uses.
    const fn rail(columns: i64) -> Rail<'static> {
        Rail { pane_id: "%9", columns }
    }

    fn is_hex4(value: &str) -> bool {
        value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    // The three tests below are `chiefd-core/src/runtime/reconcile_plan/
    // tests.rs`'s layout tests, carried over unchanged. Same vectors, same
    // assertions: identical geometry on both sides is the point.

    #[test]
    fn distributed_sizes_splits_with_a_one_cell_gap() {
        assert_eq!(distributed_sizes(10, 3).expect("fits"), vec![3, 3, 2]);
        assert_eq!(distributed_sizes(80, 1).expect("fits"), vec![80]);
        assert_eq!(distributed_sizes(5, 0).expect("no cells"), Vec::<i64>::new());
        assert!(distributed_sizes(2, 3).is_err(), "a window too small fails closed");
    }

    #[test]
    fn organization_tmux_layout_lays_a_grid() {
        let single = organization_tmux_layout(80, 24, None, &["%1"]).expect("one pane lays out");
        let checksum = single.split(',').next().expect("a checksum prefix");
        assert!(is_hex4(checksum), "layout is prefixed with a four-hex checksum");
        assert!(single.ends_with("80x24,0,0,1"));

        let two = organization_tmux_layout(80, 24, None, &["%1", "%2"]).expect("two lay out");
        assert!(!two.contains('['), "two people stay side by side, the one measured shape");

        let five = organization_tmux_layout(80, 24, None, &["%1", "%2", "%3", "%4", "%5"])
            .expect("five panes lay out");
        assert!(five.contains('['), "five people are a grid, NOT a single row of slivers");

        let six = organization_tmux_layout(80, 24, None, &["%1", "%2", "%3", "%4", "%5", "%6"])
            .expect("six panes lay out");
        assert!(six.contains('['), "six panes wrap into two balanced rows");
        assert!(is_hex4(six.split(',').next().expect("a checksum prefix")));
    }

    /// Every LEAF cell of a layout string as `(width, height, left, top, id)`.
    ///
    /// A leaf is `WxH,x,y,id`; a container is `WxH,x,y` followed by a brace. So
    /// the discriminator is whether the field after the coordinates is a bare
    /// integer, and that is the whole parser.
    fn cells(laid: &str) -> Vec<(i64, i64, i64, i64, i64)> {
        let fields: Vec<&str> = laid
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|field| !field.is_empty())
            .collect();
        let mut found = Vec::new();
        let mut index = 0;
        while index + 3 < fields.len() {
            let parsed = fields[index]
                .split_once('x')
                .and_then(|(width, height)| {
                    Some((width.parse::<i64>().ok()?, height.parse().ok()?))
                })
                .zip(fields[index + 1].parse::<i64>().ok())
                .zip(fields[index + 2].parse::<i64>().ok());
            let Some((((width, height), left), top)) = parsed else {
                index += 1;
                continue;
            };
            if let Ok(pane) = fields[index + 3].parse::<i64>() {
                found.push((width, height, left, top, pane));
                index += 4;
            } else {
                index += 3;
            }
        }
        found
    }

    /// THE REGRESSION. A department click puts every live member of that
    /// department into that department's window, and this is the SHAPE they get
    /// there — asserted as geometry for N people, not as "a layout was applied".
    ///
    /// The operator reported a department click as not showing the team. It was
    /// showing the team: `people_body` laid ONE ROW of columns for up to five
    /// people, so a real department on a real glass — 220 columns, 55 rows, a
    /// 26-column rail, measured on a live box 2026-08-18 — became strips 38 columns
    /// wide and 53 rows tall.
    #[test]
    fn a_department_of_four_or_five_is_a_grid_and_not_a_row_of_slivers() {
        // 220 - 26 (the rail) - 1 (its divider) leaves 193 columns of people.
        const PEOPLE_COLUMNS: i64 = 193;
        // The rail is `%99` here and NOT the `%9` the other tests use: this one
        // runs up to nine people, and `%9` would be indistinguishable from the
        // ninth of them in the parsed geometry.
        let rail = Rail { pane_id: "%99", columns: 26 };
        for (people, columns, rows) in
            [(2usize, 2i64, 1i64), (3, 3, 1), (4, 2, 2), (5, 3, 2), (6, 3, 2), (9, 3, 3)]
        {
            let ids: Vec<String> = (1..=people).map(|index| format!("%{index}")).collect();
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let laid = organization_tmux_layout(220, 55, Some(rail), &refs)
                .unwrap_or_else(|error| panic!("{people} people must lay out: {error}"));
            let person_cells: Vec<_> =
                cells(&laid).into_iter().filter(|cell| cell.4 != 99).collect();
            assert_eq!(person_cells.len(), people, "every live person gets a cell: {laid}");

            // COLUMNS AND ROWS read off the GEOMETRY rather than off the
            // string's punctuation: panes sharing a top edge are one row, and
            // the widest row is the column count. A single row of five answers
            // `(5, 1)` here, which is exactly the failure this test exists to
            // catch. Counting DISTINCT LEFT EDGES would not: a ragged last row
            // starts at its own offsets, so five people show four of them.
            let mut by_row: BTreeMap<i64, usize> = BTreeMap::new();
            for cell in &person_cells {
                *by_row.entry(cell.3).or_default() += 1;
            }
            let widest_row = by_row.values().copied().max().expect("a row");
            assert_eq!(
                (
                    i64::try_from(widest_row).expect("a small count"),
                    i64::try_from(by_row.len()).expect("a small count")
                ),
                (columns, rows),
                "{people} people are {columns} columns over {rows} rows: {laid}"
            );
            // AND NOBODY SITS A ROW OUT ALONE. A lone pane on its own row is
            // stretched to the window's whole width beside colleagues sharing
            // theirs, which is the merged shape the tombstone in this module
            // refuses.
            assert!(
                rows == 1 || by_row.values().all(|in_row| *in_row >= 2),
                "no row holds a single stretched pane: {laid}"
            );

            // NOBODY IS A SLIVER. Every pane takes its full share of the width
            // its row divides, and its full share of the height — one row of
            // five on this glass is 38 columns, and one column of five is 10
            // lines. Both are refused here.
            for (width, height, ..) in &person_cells {
                assert!(
                    *width >= PEOPLE_COLUMNS / columns - 1,
                    "a {width}-column pane is a sliver of {PEOPLE_COLUMNS}: {laid}"
                );
                assert!(*height >= 55 / rows - 1, "a {height}-line pane is a sliver: {laid}");
                assert!(
                    people == 1 || *width < PEOPLE_COLUMNS,
                    "no pane swallows the whole window while others share it: {laid}"
                );
            }

            // THE RAIL IS UNTOUCHED. A grid frees width rather than spending
            // it, so it can never push the rail down its narrowing ladder.
            assert!(laid.contains("{26x55,0,0,99,"), "the rail keeps its 26 columns: {laid}");
        }
    }

    /// The five-person department, pinned byte for byte.
    ///
    /// The test above proves the RULE; this proves the ANSWER, so a future
    /// change to the rule cannot quietly move the glass without somebody
    /// reading the new geometry and agreeing to it.
    #[test]
    fn five_people_beside_a_rail_lay_out_at_exactly_this_geometry() {
        let laid =
            organization_tmux_layout(220, 55, Some(rail(26)), &["%1", "%2", "%3", "%4", "%5"])
                .expect("five people lay out");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert_eq!(
            body,
            "220x55,0,0{26x55,0,0,9,193x55,27,0[193x27,27,0{64x27,27,0,1,64x27,92,0,2,\
             63x27,157,0,3},193x27,27,28{96x27,27,28,4,96x27,124,28,5}]}",
            "three over two, 64 columns by 27 lines each: {laid}"
        );
    }

    #[test]
    fn organization_tmux_layout_fails_closed_on_bad_input() {
        assert!(organization_tmux_layout(0, 24, None, &["%1"]).is_err());
        assert!(organization_tmux_layout(80, 24, None, &[]).is_err());
        assert!(
            organization_tmux_layout(80, 2, None, &["%1", "%2", "%3", "%4", "%5", "%6"]).is_err()
        );
        assert!(
            organization_tmux_layout(80, 24, None, &["1"]).is_err(),
            "a pane id must start with %"
        );
    }

    // --- the rail ---------------------------------------------------------

    #[test]
    fn a_rail_is_the_first_cell_and_the_people_begin_beyond_it() {
        let laid = organization_tmux_layout(80, 24, Some(rail(20)), &["%1", "%2"])
            .expect("a rail and two people lay out");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert_eq!(
            body, "80x24,0,0{20x24,0,0,9,59x24,21,0{29x24,21,0,1,29x24,51,0,2}}",
            "the rail is cell one at column 0; every person is offset past it"
        );
        assert!(is_hex4(laid.split(',').next().expect("a checksum prefix")));
    }

    #[test]
    fn a_rail_the_operator_dragged_wide_is_drawn_wide_while_it_still_fits() {
        let laid = organization_tmux_layout(90, 24, Some(rail(70)), &["%1"])
            .expect("one person still fits beside a wide rail");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert!(
            body.starts_with("90x24,0,0{70x24,0,0,9,19x24,71,0,1"),
            "a fraction-of-the-glass cap would fight the drag on every pass, got {body}"
        );
    }

    #[test]
    fn a_rail_is_never_narrower_than_the_collapse_control_needs() {
        let laid = organization_tmux_layout(80, 24, Some(rail(0)), &["%1"])
            .expect("a zero-column rail is floored, not refused");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert!(
            body.starts_with(&format!("80x24,0,0{{{RAIL_COLLAPSED_COLUMNS}x24,0,0,9,")),
            "a tmux pane cannot be zero columns, so the floor is the control's own width: {body}"
        );
    }

    #[test]
    fn a_rail_that_cannot_keep_its_width_gives_up_the_least_that_fits() {
        // THE SHRINK THE OPERATOR PHOTOGRAPHED. This used to jump straight to
        // the COLLAPSED width the moment the requested one did not fit: they
        // clicked a sleeping person, the loading panel showed, the person
        // arrived, and their sidebar became `Depa` / `Peop` — four columns —
        // and stayed that way, because every later pass laid it the same.
        //
        // The rail dragged to 25 of 30 columns, then five people open beside
        // it: as a GRID they are three columns over two rows, which needs five
        // of them, so the widest rail that fits is twenty-four. It gets
        // twenty-four, not four — and it used to get twenty, because the row
        // this replaced spent nine columns on the same five people. A grid
        // costs the rail LESS, which is the second reason it is the right
        // shape.
        let people = ["%1", "%2", "%3", "%4", "%5"];
        let laid = organization_tmux_layout(30, 24, Some(rail(25)), &people).expect("it lays out");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert!(
            body.starts_with("30x24,0,0{24x24,0,0,9,"),
            "narrowed by exactly what the fit demanded, and NOT collapsed: {body}"
        );
        assert!(
            organization_tmux_layout(30, 24, Some(rail(25)), &["%1"])
                .expect("one person fits")
                .contains("25x24,0,0,9"),
            "and a rail that fits is never touched at all"
        );
        // The collapsed width is the last rung, not the first alternative: a
        // company must still always lay out.
        let crowded = ["%1", "%2", "%3", "%4", "%5"];
        let tight =
            organization_tmux_layout(10, 24, Some(rail(25)), &crowded).expect("it still lays out");
        assert!(
            tight.contains(&format!("{RAIL_COLLAPSED_COLUMNS}x24,0,0,9,")),
            "a window with nothing left to give reaches the floor rather than refusing: \
             {tight}"
        );
    }

    #[test]
    fn a_window_that_cannot_hold_the_people_even_collapsed_still_fails_closed() {
        let people = ["%1", "%2", "%3", "%4", "%5", "%6"];
        assert!(
            organization_tmux_layout(8, 24, Some(rail(20)), &people).is_err(),
            "the collapsed retry is a fallback, never a licence to draw a lie"
        );
        assert!(
            organization_tmux_layout(4, 24, Some(rail(4)), &["%1"]).is_err(),
            "a window with no room beside the rail at all is refused"
        );
        assert!(
            organization_tmux_layout(80, 24, Some(Rail { pane_id: "9", columns: 20 }), &["%1"])
                .is_err(),
            "the rail's own pane id is validated like any other"
        );
    }

    #[test]
    fn a_railed_window_wraps_its_people_into_two_rows_by_the_same_rule() {
        let laid = organization_tmux_layout(
            120,
            24,
            Some(rail(20)),
            &["%1", "%2", "%3", "%4", "%5", "%6"],
        )
        .expect("six people lay out beside a rail");
        let body = laid.split_once(',').expect("a checksum prefix").1;
        assert!(body.contains('['), "six people still wrap into two rows: {body}");
        assert!(body.starts_with("120x24,0,0{20x24,0,0,9,99x24,21,0["), "got {body}");
    }
}
