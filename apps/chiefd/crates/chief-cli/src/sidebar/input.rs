//! Reading the operator's mouse out of the bytes a thin client forwards.
//!
//! # Why the BRAIN decodes and the client does not
//!
//! A thin client that parsed its own input would hold a decoder, a key table
//! and — because a terminal escape sequence arrives in however many pieces the
//! kernel felt like — a partial-sequence buffer. That is state, and state in a
//! renderer is the thing Stage 3 exists to delete: two processes with two
//! decoders can disagree about what the operator did, and only one of them is
//! holding the [`super::View`] the answer is hit-tested against.
//!
//! So the client forwards raw bytes ([`super::wire::ToBrain::Input`]) and this
//! is the only decoder in the product. It is deliberately SMALL: the rail has
//! six gestures and one key, so the whole grammar is SGR mouse reporting plus
//! the letter `q`.
//!
//! # The grammar, and why it is SGR and nothing else
//!
//! `Glass::take` asks for `?1000h ?1002h ?1003h ?1006h` — the last of those is
//! SGR extended reporting (DEC private mode 1006), which is what makes every
//! event arrive as printable decimal rather than as the original X10 encoding's
//! byte-per-coordinate (which cannot express a column past 223). Every event is
//! therefore:
//!
//! ```text
//!   ESC [ < Cb ; Cx ; Cy M     a press, or a motion/wheel report
//!   ESC [ < Cb ; Cx ; Cy m     a release
//! ```
//!
//! `Cb` carries the button in its low two bits, MOTION in bit 5 (32) and WHEEL
//! in bit 6 (64). Coordinates are ONE-BASED and pane-relative — tmux delivers
//! them relative to the pane that asked for reporting — so a column is
//! `Cx - 1` and a row is `Cy - 1`, exactly the coordinates [`super::click`]
//! hit-tests.
//!
//! # What an unrecognised sequence does
//!
//! It is CONSUMED and dropped, never re-scanned. The frame boundary is the
//! terminating `M`/`m`, so a report this build has no gesture for (a middle
//! click, a right click, bare motion) leaves the decoder positioned exactly
//! where the next event starts. Anything that is not an escape sequence at all
//! is a single byte, dropped the same way. The decoder cannot desynchronise on
//! input it does not understand, which is the property that lets it stay this
//! small.

/// One thing the operator did, as the brain acts on it.
///
/// Deliberately not crossterm's `Event`: this is the CLOSED set of gestures the
/// rail has, so a reader can see the whole vocabulary in one place, and a
/// gesture nobody handles cannot be silently introduced by a library upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The left button went down on this pane cell.
    Click {
        /// Zero-based pane column.
        column: usize,
        /// Zero-based pane row, ready for [`super::click`].
        row: usize,
    },
    /// The wheel turned up over this pane row.
    ScrollUp {
        /// Zero-based pane row.
        row: usize,
    },
    /// The wheel turned down over this pane row.
    ScrollDown {
        /// Zero-based pane row.
        row: usize,
    },
    /// `q` — the operator asked this rail to close.
    Quit,
}

/// Bits of `Cb`, named. See the module doc.
const MOTION: u32 = 32;
const WHEEL: u32 = 64;
const BUTTON: u32 = 3;

/// Every gesture in `bytes`, in order.
///
/// `bytes` is one socket read: it may hold several events, or a fragment of
/// one. A fragment yields nothing and is not an error — the caller feeds the
/// next read into [`Decoder::feed`] and the event arrives then.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Bytes seen and not yet resolved into an event.
    pending: Vec<u8>,
}

impl Decoder {
    /// A decoder with nothing buffered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one socket read and take every whole gesture out of it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Input> {
        self.pending.extend_from_slice(bytes);
        let mut found = Vec::new();
        loop {
            match self.take_one() {
                Taken::Event(input) => found.push(input),
                Taken::Consumed => {}
                Taken::Incomplete => return found,
            }
        }
    }

    /// Resolve as much of the buffer as can be resolved right now.
    fn take_one(&mut self) -> Taken {
        let Some(&first) = self.pending.first() else {
            return Taken::Incomplete;
        };
        if first != 0x1b {
            self.pending.remove(0);
            // `q` and nothing else. There are no other keyboard commands: the
            // `C-M-r` doorbell that used to make a rail re-read the shared
            // selection is deleted with the bus it rang.
            return if first == b'q' { Taken::Event(Input::Quit) } else { Taken::Consumed };
        }
        // An escape that is not `ESC [ <` is not a mouse report. Wait for
        // enough bytes to tell, then drop the escape alone: dropping MORE
        // would eat the start of whatever follows.
        match (self.pending.get(1), self.pending.get(2)) {
            (Some(b'['), Some(b'<')) => {}
            (None, _) | (Some(b'['), None) => return Taken::Incomplete,
            _ => {
                self.pending.remove(0);
                return Taken::Consumed;
            }
        }
        // The report ends at its terminator. Bounded, because an escape
        // sequence that never terminates would otherwise buffer for the life
        // of the session; past the bound the escape is dropped and the decoder
        // resynchronises on the next one.
        let end = self
            .pending
            .iter()
            .position(|byte| *byte == b'M' || *byte == b'm')
            .filter(|end| *end <= MAX_REPORT_BYTES);
        let Some(end) = end else {
            if self.pending.len() > MAX_REPORT_BYTES {
                self.pending.remove(0);
                return Taken::Consumed;
            }
            return Taken::Incomplete;
        };
        let terminator = self.pending.get(end).copied().unwrap_or(b'M');
        let body: Vec<u8> = self.pending.drain(..=end).skip(3).collect();
        let report = String::from_utf8(body).ok();
        let parsed = report.as_deref().and_then(|body| parse_report(body, terminator));
        parsed.map_or(Taken::Consumed, Taken::Event)
    }
}

/// The longest an SGR report can honestly be: three decimal fields, two
/// separators and the terminator, with room to spare.
const MAX_REPORT_BYTES: usize = 32;

/// What one pass over the buffer achieved.
enum Taken {
    /// A gesture came out.
    Event(Input),
    /// Bytes were consumed and meant nothing this build acts on.
    Consumed,
    /// Nothing more can be decided until more bytes arrive.
    Incomplete,
}

/// Turn one report body — `Cb;Cx;Cy` without its terminator — into a gesture.
///
/// `None` for every report this rail has no gesture for: a middle or right
/// button, bare pointer motion (which changes nothing the rail draws), and any
/// field that will not parse.
fn parse_report(body: &str, terminator: u8) -> Option<Input> {
    let mut fields = body.trim_end_matches(['M', 'm']).split(';');
    let code: u32 = fields.next()?.trim().parse().ok()?;
    let cell: u32 = fields.next()?.trim().parse().ok()?;
    let line: u32 = fields.next()?.trim().parse().ok()?;
    // ONE-BASED ON THE WIRE. Row 1 is the pane's first row, which is the row
    // `click` calls 0 — the same subtraction crossterm made before the client
    // stopped parsing.
    let column = usize::try_from(cell.saturating_sub(1)).ok()?;
    let row = usize::try_from(line.saturating_sub(1)).ok()?;
    if code & WHEEL != 0 {
        return match code & BUTTON {
            0 => Some(Input::ScrollUp { row }),
            1 => Some(Input::ScrollDown { row }),
            _ => None,
        };
    }
    if code & MOTION != 0 || terminator == b'm' {
        return None;
    }
    if code & BUTTON != 0 {
        return None;
    }
    Some(Input::Click { column, row })
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Input};

    /// The exact bytes `chief bench click` injects, which are the exact bytes
    /// tmux delivers for a real left click. If this drifts, every number the
    /// harness prints is about a gesture nobody makes.
    #[test]
    fn the_harnesss_own_click_decodes_as_a_click_on_that_row() {
        let mut decoder = Decoder::new();
        let events = decoder.feed(b"\x1b[<0;2;8M\x1b[<0;2;8m");
        assert_eq!(events, vec![Input::Click { column: 1, row: 7 }]);
    }

    /// ONE-BASED ON THE WIRE, ZERO-BASED IN THE HIT TEST. Getting this wrong
    /// selects the row above the one under the operator's pointer, which is
    /// the defect class `render`/`click` are two halves of.
    #[test]
    fn the_first_pane_row_is_row_zero() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"\x1b[<0;1;1M"), vec![Input::Click { column: 0, row: 0 }]);
    }

    #[test]
    fn the_wire_column_is_one_based_and_the_hit_test_column_is_zero_based() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"\x1b[<0;4;8M"), vec![Input::Click { column: 3, row: 7 }]);
    }

    /// The wheel, both ways, and never as a click.
    #[test]
    fn the_wheel_scrolls_and_is_never_read_as_a_button() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"\x1b[<64;2;5M"), vec![Input::ScrollUp { row: 4 }]);
        assert_eq!(decoder.feed(b"\x1b[<65;2;5M"), vec![Input::ScrollDown { row: 4 }]);
    }

    /// The unified tree has no internal drag gesture. Motion is ignored.
    #[test]
    fn a_held_drag_is_a_gesture_and_bare_motion_is_not() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"\x1b[<32;2;9M"), Vec::new());
        assert_eq!(decoder.feed(b"\x1b[<35;2;9M"), Vec::new(), "hovering is not a gesture");
    }

    /// Only the LEFT button. A right click on the rail must not select a row.
    #[test]
    fn the_other_buttons_are_not_the_selection_button() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"\x1b[<1;2;3M"), Vec::new());
        assert_eq!(decoder.feed(b"\x1b[<2;2;3M"), Vec::new());
    }

    /// A socket read is not an event. Fed one byte at a time, the decoder must
    /// still yield exactly one click.
    #[test]
    fn a_report_split_across_reads_still_yields_one_event() {
        let bytes = b"\x1b[<0;2;12M";
        let mut decoder = Decoder::new();
        let mut all = Vec::new();
        for byte in bytes {
            all.extend(decoder.feed(&[*byte]));
        }
        assert_eq!(all, vec![Input::Click { column: 1, row: 11 }]);
    }

    /// A sequence this build has no gesture for is CONSUMED, and the event
    /// after it still arrives. A decoder that re-scanned would find the same
    /// junk for ever; one that dropped too much would eat the next click.
    #[test]
    fn an_unknown_sequence_never_costs_the_click_that_follows_it() {
        let mut decoder = Decoder::new();
        let events = decoder.feed(b"\x1b[A\x1bOP\x1b[<0;2;4M");
        assert_eq!(events, vec![Input::Click { column: 1, row: 3 }]);
    }

    /// An escape that never terminates must not buffer for the life of the
    /// session, and must not swallow the next real gesture.
    #[test]
    fn an_endless_escape_resynchronises_rather_than_growing() {
        let mut decoder = Decoder::new();
        let junk = vec![b'0'; 64];
        decoder.feed(b"\x1b[<");
        decoder.feed(&junk);
        let events = decoder.feed(b"\x1b[<0;2;2M");
        assert_eq!(events, vec![Input::Click { column: 1, row: 1 }]);
    }

    /// `q` closes the rail, and it is the only key that does anything.
    #[test]
    fn q_is_the_one_key_with_a_meaning() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(b"q"), vec![Input::Quit]);
        assert_eq!(decoder.feed(b"abcxyz\r\n"), Vec::new());
    }
}
