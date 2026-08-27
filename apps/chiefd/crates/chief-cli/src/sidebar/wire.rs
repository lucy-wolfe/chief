//! The wire between the session BRAIN and its thin clients.
//!
//! # What travels, and in which direction
//!
//! The thin rail sends raw input and size; the focused sleeping card sends one
//! button action. Whole frames, company descriptions, and button acceptance
//! travel back:
//!
//! * **up — raw stdin bytes.** The client parses nothing. It reads whatever
//!   tmux hands its pty and forwards it verbatim, so a client holds no decoder,
//!   no key table and no mouse state, and cannot disagree with the brain about
//!   what the operator did.
//! * **up — the pane's SIZE.** The one fact that is not in the byte stream:
//!   SIGWINCH is a signal, not a sequence, so a client that forwarded only
//!   bytes could never tell the brain how tall the rail it is blitting into
//!   has become. herdr's client sends the same message for the same reason.
//! * **up — one Wake Up action.** The card names its final pane and person;
//!   the brain validates both before it asks ChiefD.
//! * **down — a FRAME**, as the exact ANSI bytes to write to the pane, with the
//!   gesture it answers.
//!
//! # Every frame is WHOLE, and that is what makes the mailbox sound
//!
//! A ratatui diff is only meaningful applied to the screen it was diffed
//! against, so a transport that may DROP a frame cannot carry diffs. The brain
//! therefore repaints every cell of every frame (the session brain's poisoned
//! buffer), which makes each frame self-contained — and a self-contained frame
//! can be replaced in flight by a newer one with nothing lost. That is exactly
//! what [`Mailbox`] does, and it is the reason a slow client can never hold the
//! brain up.
//!
//! It costs nothing at rest: a frame identical to the last one sent is not sent
//! at all (herdr drops identical frames server-side for the same reason), so
//! hovering the pointer across the rail — the one event that arrives at pointer
//! speed — moves zero bytes.
//!
//! # The correlator crosses HERE now
//!
//! `gesture_id` used to travel between processes as the third field of the
//! session's `SELECTION` tmux option, which Stage 3 deletes along with the rest
//! of the cross-process bus. It rides [`ToClient::Frame`]'s own `gesture` field
//! instead: the brain
//! mints the id when it decodes the mouse event and stamps it on the frame that
//! answers it, and the client — which has flushed those bytes to its own pty
//! and is therefore the only process that can honestly say the operator can SEE
//! them — writes `sidebar.frame.painted` naming that id. The id IS the click's
//! wall clock in microseconds ([`super::gesture`]), so the client needs no
//! second field to state the elapsed time.

use std::collections::VecDeque;

/// The version of this protocol.
///
/// A running rail is a SEPARATE PROCESS from the brain and `bun run release`
/// replaces the binary under a live session, so the two ends can be different
/// builds. A brain that is handed a version it does not speak drops the client
/// rather than decoding its bytes as something else; the pane dies, converge
/// mints a fresh rail, and the new one runs the new binary.
pub const PROTOCOL: u8 = 3;

/// The most one wire frame may carry.
///
/// A whole repaint of a 300x100 rail is on the order of 100 KB, so this is an
/// order of magnitude of headroom and a backstop against a length field that
/// has been corrupted rather than a budget anybody plans against.
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Fixed framing overhead: one kind byte and a big-endian `u32` length.
const HEADER_BYTES: usize = 5;

/// What a client tells the brain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToBrain {
    /// The first message on every connection: who this client is and how big
    /// its pane is.
    Hello {
        /// This client's protocol version. See [`PROTOCOL`].
        protocol: u8,
        /// The client's own tmux pane, from `TMUX_PANE`. The brain needs it to
        /// aim a `display-message` at the pane the operator is looking at.
        pane: String,
        /// Columns of the pane, right now.
        columns: u16,
        /// Rows of the pane, right now.
        rows: u16,
    },
    /// Bytes read from the client's stdin, verbatim.
    Input(Vec<u8>),
    /// The pane changed size. Not derivable from the byte stream.
    Resize {
        /// Columns of the pane, now.
        columns: u16,
        /// Rows of the pane, now.
        rows: u16,
    },
    /// Name the company: every department and every person, as `(id, name)`.
    ///
    /// The one QUESTION on this wire, and it has exactly one caller —
    /// `chief bench click`, which needs to turn a row of the rail the operator
    /// can point at ("Quant") into the id the glass is then checked against
    /// (`quant`). Those are two different questions and answering both with one
    /// string is how a department click gets graded against a window it did not
    /// open.
    ///
    /// It asks the BRAIN rather than chiefd because the harness must keep
    /// working against a company whose daemon is wedged or dead — which is the
    /// one experiment that matters most — and because the brain is the
    /// authority on what the rail is drawing. It replaces a read of the
    /// company snapshot the actuator used to publish into a tmux option.
    Describe,
    /// The focused sleeping card's button was activated.
    WakePerson {
        /// Protocol version, because this connection has no rail `Hello`.
        protocol: u8,
        /// The final focus body that owns the card.
        pane: String,
        /// The sleeping person shown by that card.
        person: String,
    },
}

/// What the brain pushes to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToClient {
    /// One whole frame, as the bytes to write to the pane.
    Frame {
        /// The gesture this frame answers, or `None` for a frame nobody asked
        /// for (the company changed, a client attached, a resize).
        gesture: Option<u64>,
        /// The ANSI to write. Whole, never a diff — see the module doc.
        bytes: Vec<u8>,
    },
    /// The answer to [`ToBrain::Describe`].
    Company(Named),
    /// The session brain accepted the card's one wake action.
    WakeAccepted {
        /// The person whose wake action the brain accepted.
        person: String,
    },
    /// The session brain rejected the card action after live pane validation.
    WakeRejected {
        /// The person whose card no longer owns the requested pane.
        person: String,
    },
}

/// Every department and every person, as `(id, display name)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Named {
    /// Departments, in the company's canonical order.
    pub departments: Vec<(String, String)>,
    /// People, in the company's canonical order.
    pub people: Vec<(String, String)>,
}

/// Why a wire read could not continue.
///
/// Every variant is fatal to the connection. There is no recovery from a
/// mis-framed stream: the length prefix is how the reader knows where the next
/// message starts, so a frame that will not parse means the reader no longer
/// knows where it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// A length field larger than [`MAX_PAYLOAD_BYTES`].
    #[error("a wire frame declared {declared} bytes, past the {MAX_PAYLOAD_BYTES}-byte ceiling")]
    TooLarge {
        /// What the length field said.
        declared: usize,
    },
    /// A kind byte this build does not know.
    #[error("a wire frame carried kind {kind}, which this build does not speak")]
    UnknownKind {
        /// The byte that was read.
        kind: u8,
    },
    /// A payload that does not decode as its kind says it should.
    #[error("a wire frame of kind {kind} carried a payload this build cannot read")]
    Malformed {
        /// The kind whose payload would not parse.
        kind: u8,
    },
}

const KIND_HELLO: u8 = 1;
const KIND_INPUT: u8 = 2;
const KIND_RESIZE: u8 = 3;
const KIND_FRAME: u8 = 4;
const KIND_DESCRIBE: u8 = 5;
const KIND_COMPANY: u8 = 6;
const KIND_WAKE_PERSON: u8 = 7;
const KIND_WAKE_ACCEPTED: u8 = 8;
const KIND_WAKE_REJECTED: u8 = 9;

/// Frame one message for the wire.
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + payload.len());
    out.push(kind);
    // Truncation is impossible: every caller's payload is bounded by
    // `MAX_PAYLOAD_BYTES`, which is far inside `u32`. The cast is written
    // saturating rather than `as` so a future caller cannot make it wrap.
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

impl ToBrain {
    /// The bytes that carry this message.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello { protocol, pane, columns, rows } => {
                let mut payload = vec![*protocol];
                payload.extend_from_slice(&columns.to_be_bytes());
                payload.extend_from_slice(&rows.to_be_bytes());
                payload.extend_from_slice(pane.as_bytes());
                frame(KIND_HELLO, &payload)
            }
            Self::Input(bytes) => frame(KIND_INPUT, bytes),
            Self::Resize { columns, rows } => {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&columns.to_be_bytes());
                payload.extend_from_slice(&rows.to_be_bytes());
                frame(KIND_RESIZE, &payload)
            }
            Self::Describe => frame(KIND_DESCRIBE, &[]),
            Self::WakePerson { protocol, pane, person } => {
                let mut payload = vec![*protocol];
                payload.extend_from_slice(pane.as_bytes());
                payload.push(0);
                payload.extend_from_slice(person.as_bytes());
                frame(KIND_WAKE_PERSON, &payload)
            }
        }
    }

    /// Read one message back out of a framed payload.
    fn decode(kind: u8, payload: Vec<u8>) -> Result<Self, WireError> {
        match kind {
            KIND_HELLO => {
                let (&protocol, rest) =
                    payload.split_first().ok_or(WireError::Malformed { kind })?;
                let (columns, rest) = read_u16(rest).ok_or(WireError::Malformed { kind })?;
                let (rows, rest) = read_u16(rest).ok_or(WireError::Malformed { kind })?;
                let pane =
                    String::from_utf8(rest.to_vec()).map_err(|_| WireError::Malformed { kind })?;
                Ok(Self::Hello { protocol, pane, columns, rows })
            }
            KIND_INPUT => Ok(Self::Input(payload)),
            KIND_RESIZE => {
                let (columns, rest) = read_u16(&payload).ok_or(WireError::Malformed { kind })?;
                let (rows, _) = read_u16(rest).ok_or(WireError::Malformed { kind })?;
                Ok(Self::Resize { columns, rows })
            }
            KIND_DESCRIBE => Ok(Self::Describe),
            KIND_WAKE_PERSON => {
                let (&protocol, rest) =
                    payload.split_first().ok_or(WireError::Malformed { kind })?;
                let Some(split) = rest.iter().position(|byte| *byte == 0) else {
                    return Err(WireError::Malformed { kind });
                };
                let pane = String::from_utf8(rest[..split].to_vec())
                    .map_err(|_| WireError::Malformed { kind })?;
                let person = String::from_utf8(rest[split + 1..].to_vec())
                    .map_err(|_| WireError::Malformed { kind })?;
                Ok(Self::WakePerson { protocol, pane, person })
            }
            other => Err(WireError::UnknownKind { kind: other }),
        }
    }
}

impl ToClient {
    /// The bytes that carry this message.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Frame { gesture, bytes } => {
                // ZERO IS NOT A GESTURE, which is already `GestureId`'s own
                // rule — so the absent case needs no extra field.
                let mut payload = Vec::with_capacity(8 + bytes.len());
                payload.extend_from_slice(&gesture.unwrap_or(0).to_be_bytes());
                payload.extend_from_slice(bytes);
                frame(KIND_FRAME, &payload)
            }
            Self::Company(named) => {
                let payload = serde_json::to_vec(named).unwrap_or_default();
                frame(KIND_COMPANY, &payload)
            }
            Self::WakeAccepted { person } => frame(KIND_WAKE_ACCEPTED, person.as_bytes()),
            Self::WakeRejected { person } => frame(KIND_WAKE_REJECTED, person.as_bytes()),
        }
    }

    /// Read one message back out of a framed payload.
    fn decode(kind: u8, payload: Vec<u8>) -> Result<Self, WireError> {
        match kind {
            KIND_FRAME => {
                let raw = payload.get(..8).ok_or(WireError::Malformed { kind })?;
                let mut id = [0_u8; 8];
                id.copy_from_slice(raw);
                let gesture = u64::from_be_bytes(id);
                Ok(Self::Frame {
                    gesture: (gesture != 0).then_some(gesture),
                    bytes: payload.get(8..).unwrap_or_default().to_vec(),
                })
            }
            KIND_COMPANY => serde_json::from_slice(&payload)
                .map(Self::Company)
                .map_err(|_| WireError::Malformed { kind }),
            KIND_WAKE_ACCEPTED => String::from_utf8(payload)
                .map(|person| Self::WakeAccepted { person })
                .map_err(|_| WireError::Malformed { kind }),
            KIND_WAKE_REJECTED => String::from_utf8(payload)
                .map(|person| Self::WakeRejected { person })
                .map_err(|_| WireError::Malformed { kind }),
            other => Err(WireError::UnknownKind { kind: other }),
        }
    }
}

/// Read a big-endian `u16` off the front, and what is left.
fn read_u16(bytes: &[u8]) -> Option<(u16, &[u8])> {
    let raw = bytes.get(..2)?;
    Some((u16::from_be_bytes([*raw.first()?, *raw.get(1)?]), bytes.get(2..)?))
}

/// The stream side of the wire: bytes in, whole messages out.
///
/// A socket read returns whatever happens to be in the kernel's buffer, which
/// is neither one message nor a whole one. This accumulates and yields only
/// complete frames, so no caller anywhere has to know that a message can arrive
/// in halves.
#[derive(Debug, Default)]
pub struct Frames {
    buffer: VecDeque<u8>,
}

impl Frames {
    /// A reader with nothing in it yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add whatever the socket just handed us.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend(bytes.iter().copied());
    }

    /// The next whole message a client sent, or `None` while one is still
    /// arriving.
    ///
    /// # Errors
    /// [`WireError`] for a frame this build cannot read. Fatal: see the type.
    pub fn next_to_brain(&mut self) -> Result<Option<ToBrain>, WireError> {
        match self.next_frame()? {
            Some((kind, payload)) => ToBrain::decode(kind, payload).map(Some),
            None => Ok(None),
        }
    }

    /// The next whole message the brain sent, or `None` while one is still
    /// arriving.
    ///
    /// # Errors
    /// [`WireError`] for a frame this build cannot read. Fatal: see the type.
    pub fn next_to_client(&mut self) -> Result<Option<ToClient>, WireError> {
        match self.next_frame()? {
            Some((kind, payload)) => ToClient::decode(kind, payload).map(Some),
            None => Ok(None),
        }
    }

    /// Split one whole frame off the front, if there is one.
    fn next_frame(&mut self) -> Result<Option<(u8, Vec<u8>)>, WireError> {
        if self.buffer.len() < HEADER_BYTES {
            return Ok(None);
        }
        let kind = *self.buffer.front().unwrap_or(&0);
        let mut length = [0_u8; 4];
        for (at, slot) in length.iter_mut().enumerate() {
            *slot = *self.buffer.get(1 + at).unwrap_or(&0);
        }
        let declared = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if declared > MAX_PAYLOAD_BYTES {
            return Err(WireError::TooLarge { declared });
        }
        if self.buffer.len() < HEADER_BYTES + declared {
            return Ok(None);
        }
        self.buffer.drain(..HEADER_BYTES);
        let payload: Vec<u8> = self.buffer.drain(..declared).collect();
        Ok(Some((kind, payload)))
    }
}

/// A one-frame, latest-wins outbox: the brain never waits for a client.
///
/// # Why a slot and not a queue
///
/// herdr's render mailbox is single-slot for the property this needs
/// (`src/server/client_transport.rs`): a client that is slow to read must cost
/// the server nothing, and a queue of stale frames is worse than one fresh one
/// — every frame in it is a picture the operator will never see, drawn one
/// after another before the current one.
///
/// Replacement is only sound because every frame is WHOLE (see the module
/// doc). A dropped frame therefore loses nothing at all: the frame that
/// replaced it paints every cell the dropped one would have.
///
/// [`Mailbox::put`] never blocks and never fails. There is no back pressure to
/// exert, because there is no queue to fill.
#[derive(Debug)]
pub struct Mailbox {
    slot: std::sync::Mutex<Slot>,
    ready: tokio::sync::Notify,
}

/// The mailbox's contents, and whether anybody is still coming for them.
#[derive(Debug, Default)]
struct Slot {
    pending: Option<ToClient>,
    closed: bool,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self { slot: std::sync::Mutex::new(Slot::default()), ready: tokio::sync::Notify::new() }
    }
}

impl Mailbox {
    /// An empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Leave a frame, replacing whatever was not collected.
    ///
    /// Answers whether a frame was DROPPED, which is the only thing worth
    /// counting here: a mailbox that drops nothing is a client keeping up.
    pub fn put(&self, message: ToClient) -> bool {
        let dropped = match self.slot.lock() {
            Ok(mut slot) => {
                if slot.closed {
                    return false;
                }
                slot.pending.replace(message).is_some()
            }
            // A poisoned lock means a writer panicked while holding it. There
            // is no state to repair — the slot holds one frame — and refusing
            // to draw for the rest of the session would be a far worse answer
            // than losing this frame.
            Err(_) => false,
        };
        self.ready.notify_one();
        dropped
    }

    /// Park until there is a frame, or `None` once the mailbox is closed.
    pub async fn take(&self) -> Option<ToClient> {
        loop {
            // Registered BEFORE the slot is examined, so a `put` that lands
            // between the look and the park is not missed.
            let waiting = self.ready.notified();
            tokio::pin!(waiting);
            waiting.as_mut().enable();
            match self.slot.lock() {
                Ok(mut slot) => {
                    if let Some(message) = slot.pending.take() {
                        return Some(message);
                    }
                    if slot.closed {
                        return None;
                    }
                }
                Err(_) => return None,
            }
            waiting.await;
        }
    }

    /// Stop this mailbox: every later [`Mailbox::take`] answers `None`.
    pub fn close(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.closed = true;
            slot.pending = None;
        }
        // `notify_waiters`, not `notify_one`: a close has to release whoever is
        // parked, and there is no frame for a permit to be saved for.
        self.ready.notify_waiters();
        self.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::{Frames, Mailbox, ToBrain, ToClient, WireError, MAX_PAYLOAD_BYTES, PROTOCOL};

    /// THE RULE: what one end writes is exactly what the other end reads.
    #[test]
    fn every_message_a_client_sends_survives_the_wire_unchanged() {
        let sent = vec![
            ToBrain::Hello { protocol: PROTOCOL, pane: "%17".to_owned(), columns: 26, rows: 51 },
            ToBrain::Input(b"\x1b[<0;2;7M\x1b[<0;2;7m".to_vec()),
            ToBrain::Resize { columns: 240, rows: 60 },
            ToBrain::WakePerson {
                protocol: PROTOCOL,
                pane: "%81".to_owned(),
                person: "ada".to_owned(),
            },
            // An empty read is a real message: it is what a pty hands back at
            // EOF, and a decoder that could not represent it would turn a
            // closed stdin into a parse error.
            ToBrain::Input(Vec::new()),
        ];
        let mut wire = Frames::new();
        for message in &sent {
            wire.feed(&message.encode());
        }
        let mut read = Vec::new();
        while let Some(message) = wire.next_to_brain().expect("the stream is well formed") {
            read.push(message);
        }
        assert_eq!(read, sent);
    }

    /// The same rule downstream, and the gesture is the field that matters:
    /// it is the ONLY way an id crosses a process boundary now.
    #[test]
    fn a_frame_carries_its_gesture_across_the_socket() {
        let sent =
            ToClient::Frame { gesture: Some(1_786_842_093_497_611), bytes: b"\x1b[2;1H".to_vec() };
        let mut wire = Frames::new();
        wire.feed(&sent.encode());
        assert_eq!(wire.next_to_client().expect("well formed"), Some(sent));
    }

    /// The one QUESTION on this wire, and the answer `chief bench click` turns
    /// a row of the rail into an id with.
    #[test]
    fn the_company_the_brain_names_survives_the_wire() {
        let mut wire = Frames::new();
        wire.feed(&ToBrain::Describe.encode());
        assert_eq!(wire.next_to_brain().expect("well formed"), Some(ToBrain::Describe));

        let named = super::Named {
            departments: vec![("quant".to_owned(), "Quant".to_owned())],
            people: vec![("ada".to_owned(), "Ada".to_owned())],
        };
        let mut back = Frames::new();
        back.feed(&ToClient::Company(named.clone()).encode());
        assert_eq!(back.next_to_client().expect("well formed"), Some(ToClient::Company(named)));
    }

    #[test]
    fn a_card_wake_acceptance_names_the_exact_person() {
        let sent = ToClient::WakeAccepted { person: "ada".to_owned() };
        let mut wire = Frames::new();
        wire.feed(&sent.encode());
        assert_eq!(wire.next_to_client().expect("well formed"), Some(sent));

        let rejected = ToClient::WakeRejected { person: "grace".to_owned() };
        let mut wire = Frames::new();
        wire.feed(&rejected.encode());
        assert_eq!(wire.next_to_client().expect("well formed"), Some(rejected));
    }

    /// "No gesture" and "gesture 0" are different facts on the wire too — the
    /// same rule `GestureId::from_raw` states, held at the boundary that now
    /// carries it.
    #[test]
    fn a_frame_nobody_asked_for_names_no_gesture() {
        let sent = ToClient::Frame { gesture: None, bytes: b"x".to_vec() };
        let mut wire = Frames::new();
        wire.feed(&sent.encode());
        let Some(ToClient::Frame { gesture, .. }) = wire.next_to_client().expect("well formed")
        else {
            panic!("one frame");
        };
        assert_eq!(gesture, None);
    }

    /// A socket read is not a message. Fed one byte at a time, the reader must
    /// still yield exactly one whole message and not a single partial one.
    #[test]
    fn a_message_split_across_reads_is_yielded_once_and_whole() {
        let message =
            ToBrain::Hello { protocol: PROTOCOL, pane: "%3".to_owned(), columns: 26, rows: 40 };
        let bytes = message.encode();
        let mut wire = Frames::new();
        for (at, byte) in bytes.iter().enumerate() {
            wire.feed(&[*byte]);
            let read = wire.next_to_brain().expect("well formed");
            if at + 1 < bytes.len() {
                assert_eq!(read, None, "byte {at} completed a message it should not have");
            } else {
                assert_eq!(read, Some(message.clone()));
            }
        }
    }

    /// Two messages in ONE read are two messages.
    #[test]
    fn several_messages_in_one_read_are_all_yielded() {
        let mut wire = Frames::new();
        let mut bytes = ToBrain::Input(b"ab".to_vec()).encode();
        bytes.extend(ToBrain::Resize { columns: 10, rows: 20 }.encode());
        wire.feed(&bytes);
        assert_eq!(
            wire.next_to_brain().expect("well formed"),
            Some(ToBrain::Input(b"ab".to_vec()))
        );
        assert_eq!(
            wire.next_to_brain().expect("well formed"),
            Some(ToBrain::Resize { columns: 10, rows: 20 })
        );
        assert_eq!(wire.next_to_brain().expect("well formed"), None);
    }

    /// A length field this build will not honour is a REFUSAL, never an
    /// allocation. The reader is handed bytes by another process.
    #[test]
    fn an_absurd_length_is_refused_rather_than_reserved() {
        let mut wire = Frames::new();
        wire.feed(&[2, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(
            wire.next_to_brain(),
            Err(WireError::TooLarge { declared: usize::try_from(u32::MAX).expect("64-bit") }),
            "a corrupted length must not become a {MAX_PAYLOAD_BYTES}-byte read"
        );
    }

    /// A kind from a build that is not this one is refused, not guessed at.
    /// `bun run release` replaces the binary under a live session, so the two
    /// ends CAN be different builds.
    #[test]
    fn a_kind_this_build_does_not_speak_is_refused() {
        let mut wire = Frames::new();
        wire.feed(&[99, 0, 0, 0, 0]);
        assert_eq!(wire.next_to_brain(), Err(WireError::UnknownKind { kind: 99 }));
    }

    /// A payload too short for its kind is refused rather than read as
    /// something else.
    #[test]
    fn a_truncated_payload_is_refused() {
        let mut wire = Frames::new();
        wire.feed(&[3, 0, 0, 0, 1, 7]);
        assert_eq!(wire.next_to_brain(), Err(WireError::Malformed { kind: 3 }));
    }

    /// THE MAILBOX RULE: one slot, and the freshest frame wins.
    #[tokio::test]
    async fn a_client_that_is_not_reading_gets_the_freshest_frame_and_never_a_backlog() {
        let mailbox = Mailbox::new();
        let first = ToClient::Frame { gesture: Some(1), bytes: b"one".to_vec() };
        let second = ToClient::Frame { gesture: Some(2), bytes: b"two".to_vec() };
        assert!(!mailbox.put(first), "the first frame displaces nothing");
        assert!(mailbox.put(second.clone()), "the second frame displaces the first");
        assert_eq!(mailbox.take().await, Some(second), "the operator sees the newest picture");
        // And nothing is left behind: a queue would hand back the stale frame
        // here, which is a picture the operator would watch being overtaken.
        let done = tokio::time::timeout(std::time::Duration::from_millis(50), mailbox.take()).await;
        assert!(done.is_err(), "an emptied mailbox parks rather than replaying");
    }

    /// A `put` that lands while the writer is parked wakes it. Without this the
    /// first frame after a quiet spell would sit in the slot until the next
    /// one displaced it — the rail would draw one gesture behind, for ever.
    #[tokio::test]
    async fn a_parked_writer_is_woken_by_the_frame_that_arrives() {
        let mailbox = std::sync::Arc::new(Mailbox::new());
        let writer = std::sync::Arc::clone(&mailbox);
        let parked = tokio::spawn(async move { writer.take().await });
        // Yield so the task is genuinely parked before the frame is left.
        tokio::task::yield_now().await;
        mailbox.put(ToClient::Frame { gesture: None, bytes: b"hello".to_vec() });
        let taken = tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("the writer must be woken")
            .expect("the task must not panic");
        assert!(taken.is_some());
    }

    /// A closed mailbox releases whoever is parked on it, so a client that
    /// went away does not leave a task waiting for the life of the session.
    #[tokio::test]
    async fn closing_the_mailbox_releases_the_writer() {
        let mailbox = std::sync::Arc::new(Mailbox::new());
        let writer = std::sync::Arc::clone(&mailbox);
        let parked = tokio::spawn(async move { writer.take().await });
        tokio::task::yield_now().await;
        mailbox.close();
        let taken = tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("the writer must be released")
            .expect("the task must not panic");
        assert_eq!(taken, None);
        assert!(!mailbox.put(ToClient::Frame { gesture: None, bytes: Vec::new() }));
    }
}
