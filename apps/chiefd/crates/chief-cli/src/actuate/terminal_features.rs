//! The one tmux server rule that grants browser terminals exact RGB output.

/// A tmux shell predicate that accepts only the exact canonical RGB row.
pub const BROWSER_RGB_PRESENT: &str =
    "tmux show-options -s -v terminal-features | grep -Fxq 'xterm*:RGB'";
/// The server-scoped tmux command that appends the canonical RGB row.
pub const BROWSER_RGB_APPEND: &str = "set-option -as terminal-features ,xterm*:RGB";

/// Append the idempotent browser RGB rule to an existing tmux command queue.
pub fn push_browser_rgb_feature(argv: &mut Vec<String>) {
    argv.extend([
        ";".to_owned(),
        "if-shell".to_owned(),
        BROWSER_RGB_PRESENT.to_owned(),
        String::new(),
        BROWSER_RGB_APPEND.to_owned(),
    ]);
}
