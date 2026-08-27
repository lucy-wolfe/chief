//! The operator's terminal geometry — the one fact only this process holds.
//!
//! # Why this module exists
//!
//! A tmux session created detached and unsized is minted at the server's
//! `default-size`, 80x24. `actuate::interpret::apply_layout` then reads that
//! size back and pins an ABSOLUTE layout string to it, and an absolute layout
//! string is not merely a pane arrangement — measured on tmux 3.7, applying one
//! RESIZES the window to the layout's own dimensions. So an 80x24 fiction at
//! creation becomes an 80x24 window inside a 202x45 terminal, with tmux's
//! dotted dead space filling the remainder.
//!
//! The size that ends the fiction is the size of the terminal the operator
//! typed `chiefd` into, and this process is the only one that can see it. The
//! actuator cannot: it is a resident pane of the headless `chiefd-actuator-*`
//! session, so reading its own tty reproduces exactly the 80x24 we are trying
//! to remove.
//!
//! Nothing here fails. A caller with no terminal — a pipe, a CI runner, a cron
//! line — gets `None` and the tmux server keeps its own default, which is
//! today's behavior exactly.

use std::io::{stderr, stdin, stdout};
use std::os::fd::{AsFd, BorrowedFd};

/// The operator's terminal size as `(columns, rows)`.
///
/// All three standard streams are tried, because an operator who redirects one
/// of them still has a terminal on the other two. Opening `/dev/tty` would also
/// cover the case where all three are redirected, but this crate may not open
/// files at all (`apps/chiefd/clippy.toml`: file handles belong to the host
/// executor, README §5.6) — and that case does not arise for verbs whose whole
/// purpose is to hand a terminal over.
///
/// A zero in either dimension is not a size. A pty that has never been sized
/// answers `winsize` with zeros, and passing `-x 0` would be a fresh way to
/// produce the very defect this module exists to remove.
#[must_use]
pub(crate) fn operator_size() -> Option<(u16, u16)> {
    size_of(stdout().as_fd())
        .or_else(|| size_of(stderr().as_fd()))
        .or_else(|| size_of(stdin().as_fd()))
}

/// `TIOCGWINSZ` for one descriptor, through `rustix`.
///
/// `rustix::termios::Winsize` carries `u16` columns and rows on every target,
/// which is the reason this is not a hand-rolled `libc::ioctl`: `TIOCGWINSZ`
/// itself is a `c_ulong` on Linux and a `c_uint` on Darwin, and a hand-rolled
/// call is the classic way to write something that compiles on exactly one of
/// the two platforms this project must build on.
fn size_of(fd: BorrowedFd<'_>) -> Option<(u16, u16)> {
    let size = rustix::termios::tcgetwinsize(fd).ok()?;
    (size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

#[cfg(test)]
mod tests {
    use super::operator_size;

    /// The contract that matters for every non-interactive caller: asking is
    /// always safe. Under `cargo test` stdio is captured, so this exercises the
    /// no-terminal path end to end and proves it neither panics nor errors.
    #[test]
    fn asking_for_a_size_never_fails() {
        match operator_size() {
            None => {}
            Some((columns, rows)) => {
                assert!(columns > 0, "a reported size is never zero-width");
                assert!(rows > 0, "a reported size is never zero-height");
            }
        }
    }
}
