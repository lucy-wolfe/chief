//! The canonical tmux window size for one operator action.
//!
//! Detached windows can keep an old server size while the attached client is
//! wider. This module owns both the source rule and the complete repair pair.

/// The full tmux grid of one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) columns: u32,
    pub(crate) rows: u32,
}

impl Geometry {
    pub(crate) fn new(columns: u32, rows: u32) -> Option<Self> {
        (columns > 0 && rows > 0).then_some(Self { columns, rows })
    }

    fn parse(columns: &str, rows: &str) -> Option<Self> {
        let columns = columns.trim().parse().ok().filter(|value| *value > 0)?;
        let rows = rows.trim().parse().ok().filter(|value| *value > 0)?;
        Some(Self { columns, rows })
    }
}

/// Read the active managed window, then the stable first managed window when
/// the active window cannot provide a valid size.
pub(crate) fn capture<F>(session: &str, mut run: F) -> Option<Geometry>
where
    F: FnMut(&[String]) -> Option<String>,
{
    let active = vec![
        "display-message".to_owned(),
        "-p".to_owned(),
        "-t".to_owned(),
        session.to_owned(),
        "-F".to_owned(),
        "#{window_width}\t#{window_height}\t#{@organization_window_id}".to_owned(),
    ];
    if let Some(output) = run(&active) {
        if let Some(geometry) = parse_active(&output) {
            return Some(geometry);
        }
    }

    let managed = vec![
        "list-windows".to_owned(),
        "-t".to_owned(),
        session.to_owned(),
        "-F".to_owned(),
        "#{window_index}\t#{window_width}\t#{window_height}\t#{@organization_window_id}".to_owned(),
    ];
    run(&managed).and_then(|output| parse_first_managed(&output))
}

fn parse_active(output: &str) -> Option<Geometry> {
    output.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let geometry = Geometry::parse(fields.next()?, fields.next()?)?;
        (!fields.next().unwrap_or_default().trim().is_empty()).then_some(geometry)
    })
}

fn parse_first_managed(output: &str) -> Option<Geometry> {
    output.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let _index = fields.next()?;
        let geometry = Geometry::parse(fields.next()?, fields.next()?)?;
        (!fields.next().unwrap_or_default().trim().is_empty()).then_some(geometry)
    })
}

/// Parse a direct `width<TAB>height` probe.
pub(crate) fn parse_size(output: &str) -> Option<Geometry> {
    let mut fields = output.lines().next()?.split('\t');
    Geometry::parse(fields.next()?, fields.next()?)
}

/// Build the only permitted window resize sequence.
///
/// Managed company windows remain manual after this sequence. This prevents a
/// client SIGWINCH from publishing tmux's proportional split before Chief can
/// apply the final rail/body layout for the new viewport.
pub(crate) fn normalization_argv(
    window: &str,
    current: Option<Geometry>,
    canonical: Geometry,
) -> Option<Vec<String>> {
    normalization_sequence(window, current, canonical, None)
}

/// Build one geometry or layout repair inside the same tmux publication.
///
/// A window resize changes the split tree before any client can draw it. The
/// final absolute layout must therefore precede the explicit manual ownership
/// stamp in this same sequence. An already-resized active window still needs
/// the layout comparison: matching geometry does not mean its rail split is
/// correct. A matching window in automatic mode still needs the ownership
/// stamp.
pub(crate) fn normalization_with_layout_argv(
    window: &str,
    current: Option<Geometry>,
    canonical: Geometry,
    current_layout: Option<&str>,
    current_mode: Option<&str>,
    layout: &str,
) -> Option<Vec<String>> {
    if current == Some(canonical)
        && current_layout.is_some_and(|current| current == layout)
        && current_mode.is_some_and(|mode| mode == "manual")
    {
        return None;
    }
    let mut argv = Vec::new();
    if current != Some(canonical) {
        argv.extend([
            "resize-window".to_owned(),
            "-t".to_owned(),
            window.to_owned(),
            "-x".to_owned(),
            canonical.columns.to_string(),
            "-y".to_owned(),
            canonical.rows.to_string(),
            ";".to_owned(),
        ]);
    }
    argv.extend([
        "select-layout".to_owned(),
        "-t".to_owned(),
        window.to_owned(),
        layout.to_owned(),
        ";".to_owned(),
        "set-option".to_owned(),
        "-w".to_owned(),
        "-t".to_owned(),
        window.to_owned(),
        "window-size".to_owned(),
        "manual".to_owned(),
    ]);
    Some(argv)
}

fn normalization_sequence(
    window: &str,
    current: Option<Geometry>,
    canonical: Geometry,
    layout: Option<&str>,
) -> Option<Vec<String>> {
    if current == Some(canonical) {
        return None;
    }
    let mut argv = vec![
        "resize-window".to_owned(),
        "-t".to_owned(),
        window.to_owned(),
        "-x".to_owned(),
        canonical.columns.to_string(),
        "-y".to_owned(),
        canonical.rows.to_string(),
        ";".to_owned(),
    ];
    if let Some(layout) = layout {
        argv.extend([
            "select-layout".to_owned(),
            "-t".to_owned(),
            window.to_owned(),
            layout.to_owned(),
            ";".to_owned(),
        ]);
    }
    argv.extend([
        "set-option".to_owned(),
        "-w".to_owned(),
        "-t".to_owned(),
        window.to_owned(),
        "window-size".to_owned(),
        "manual".to_owned(),
    ]);
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_managed_window_wins_over_the_first_window() {
        let mut replies = [Some("240\t55\tquant".to_owned())].into_iter();
        assert_eq!(
            capture("org-acme_", |_| replies.next().flatten()),
            Some(Geometry { columns: 240, rows: 55 })
        );
    }

    #[test]
    fn first_managed_window_is_the_fallback_for_an_unreadable_active_window() {
        let mut replies = [
            Some("0\t0\tquant".to_owned()),
            Some("0\t166\t42\texecutive\n1\t240\t55\tquant".to_owned()),
        ]
        .into_iter();
        assert_eq!(
            capture("org-acme_", |_| replies.next().flatten()),
            Some(Geometry { columns: 166, rows: 42 })
        );
    }

    #[test]
    fn a_166_to_240_mismatch_repairs_and_keeps_manual_in_one_sequence() {
        let argv = normalization_argv(
            "@7",
            Some(Geometry { columns: 166, rows: 42 }),
            Geometry { columns: 240, rows: 55 },
        )
        .expect("the mismatch needs repair");
        assert_eq!(
            argv,
            [
                "resize-window",
                "-t",
                "@7",
                "-x",
                "240",
                "-y",
                "55",
                ";",
                "set-option",
                "-w",
                "-t",
                "@7",
                "window-size",
                "manual",
            ]
        );
    }

    #[test]
    fn matching_geometry_is_a_no_op() {
        let geometry = Geometry { columns: 240, rows: 55 };
        assert_eq!(normalization_argv("@7", Some(geometry), geometry), None);
    }

    #[test]
    fn final_layout_precedes_manual_ownership_in_the_same_resize_sequence() {
        let argv = normalization_with_layout_argv(
            "@7",
            Some(Geometry { columns: 348, rows: 62 }),
            Geometry { columns: 240, rows: 56 },
            Some("wrong-layout"),
            Some("latest"),
            "final-layout",
        )
        .expect("the mismatch needs repair");
        assert_eq!(
            argv,
            [
                "resize-window",
                "-t",
                "@7",
                "-x",
                "240",
                "-y",
                "56",
                ";",
                "select-layout",
                "-t",
                "@7",
                "final-layout",
                ";",
                "set-option",
                "-w",
                "-t",
                "@7",
                "window-size",
                "manual",
            ]
        );
    }

    #[test]
    fn matching_geometry_with_a_wrong_layout_still_repairs_the_visible_frame() {
        let geometry = Geometry { columns: 348, rows: 59 };
        let argv = normalization_with_layout_argv(
            "@7",
            Some(geometry),
            geometry,
            Some("348x59,0,0{80x59,0,0,3,267x59,81,0,4}"),
            Some("latest"),
            "348x59,0,0{26x59,0,0,3,321x59,27,0,4}",
        )
        .expect("the wrong active layout still needs repair");
        assert_eq!(argv.first().map(String::as_str), Some("select-layout"));
        assert!(argv.iter().any(|word| word == "window-size"));
        assert!(argv.iter().any(|word| word == "manual"));
    }

    #[test]
    fn matching_geometry_and_layout_publish_nothing() {
        let geometry = Geometry { columns: 348, rows: 59 };
        let layout = "348x59,0,0{26x59,0,0,3,321x59,27,0,4}";
        assert_eq!(
            normalization_with_layout_argv(
                "@7",
                Some(geometry),
                geometry,
                Some(layout),
                Some("manual"),
                layout,
            ),
            None
        );
    }
}
