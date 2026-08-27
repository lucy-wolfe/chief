//! tmux control mode: one persistent client instead of one process per command.
//!
//! # Why this exists
//!
//! Every tmux call chief makes is a PROCESS. Fork, exec the tmux binary,
//! connect the server's unix socket, write one command, read one reply, exit.
//! Measured at ~25 ms with the server on the same box — and tmux's own work
//! inside that is microseconds, so the number is almost entirely process
//! setup and teardown. The cost of a gesture is therefore its spawn COUNT
//! times ~25 ms, which is why the count has been cut so hard already (an
//! observation is 5 invocations; the rail's display gesture is 1).
//!
//! `tmux -C` deletes the process instead of counting it. A control-mode
//! client is a long-lived tmux client that reads commands from stdin and
//! writes replies to stdout as `%begin`/`%end` blocks. The command still
//! crosses the same socket to the same server, but nothing forks, so a call
//! costs well under a millisecond.
//!
//! # The hazard, and the two things that answer it
//!
//! **A control-mode client is a REAL ATTACHED CLIENT, and tmux sizes windows
//! to fit its clients.** A control client that declares a size can shrink the
//! operator's windows, which would look exactly like the geometry bugs the
//! sidebar work spent a day closing. This is measured, not feared. Against
//! tmux 3.3a, `window-size latest` (tmux's DEFAULT), a session made at
//! 200x50, and — where noted — a real pty client attached at 180x45:
//!
//! | client flags | declares a size? | operator attached? | window |
//! |---|---|---|---|
//! | `no-output,ignore-size` | no | yes | 180x44, unchanged |
//! | `no-output,ignore-size` | no | no | 200x50, unchanged |
//! | `no-output,ignore-size` | yes | yes | 180x44, unchanged |
//! | `no-output,ignore-size` | yes | no | **200x50 → 80x24** |
//! | none | no | yes | 180x44, unchanged |
//! | none | no | no | 200x50, unchanged |
//! | none | yes | yes | **180x44 → 80x24** |
//! | none | yes | no | **200x50 → 80x24** |
//!
//! Read the trigger column first: **every resize in that table is a row where
//! the client DECLARED a size.** A control client that never issues
//! `refresh-client -C` has no size for tmux to fit a window to, and is
//! neutral whatever its flags. That is the load-bearing guarantee here, and
//! this module's is structural rather than promised — there is no
//! `refresh-client` anywhere in it.
//!
//! [`CLIENT_FLAGS`] is the second layer, and it is worth being precise about
//! what it does and does not buy, because the obvious reading is wrong.
//! `ignore-size` takes this client out of the session-size calculation, which
//! protects the case that matters — an operator attached, us joining beside
//! them — even if somebody later adds a `refresh-client`. It does NOT protect
//! the solo case (row four): when ours is the only client, tmux must size the
//! window to SOMETHING, so a declared size wins despite the flag. The two
//! layers therefore cover different cases and neither is redundant; only
//! "declare nothing" covers both.
//!
//! `CLIENT_FLAGS` earns its place on the operator-attached row, which is the
//! one an operator would have photographed.
//!
//! `no-output` also suppresses `%output` for every pane in the attached
//! session. That is the difference between a quiet pipe and one carrying
//! every keystroke of every Pi, and it removes the largest class of
//! interleaved notification before the demultiplexer ever sees it.
//!
//! # Framing
//!
//! A reply is the text between `%begin <ts> <num> <flags>` and the matching
//! `%end`, or `%error` when tmux answered "no". Notifications
//! (`%session-changed`, `%window-add`, `%layout-change`, `%exit`, …) arrive
//! between blocks and are dropped. A plain line reader would mistake one for
//! a reply, which is why [`Demux`] is a state machine rather than a
//! `lines()` loop.
//!
//! `%error` maps to a NON-ZERO STATUS with the text on stderr — not to a
//! transport error. Under the spawn transport "tmux said no" is a non-zero
//! exit with text on stderr, and the trust rules in
//! [`crate::actuate::exec`] classify it. Reporting it as a transport failure
//! instead would turn "tmux answered no" into "tmux did not answer", which
//! the actuate path reads as [`HostErr::Untrusted`] — the one confusion this
//! module must never make.
//!
//! # When control mode is not available
//!
//! Falling back to a spawn is a runtime capability check, not a
//! compatibility layer: there is one transport, and it asks whether this host
//! can hold a control client. A host with no session to attach to gets the
//! spawn path and identical behaviour — including identical FAILURE
//! behaviour, which callers already treat as untrusted and fail closed on.
//!
//! **A missing session is not that question being answered**, and conflating
//! the two cost the operator thirty seconds of a visibly slow product. See
//! [`Attach`] for the measurement and [`ABSENT_RETRY`] for what it earns
//! instead.
//!
//! An OLDER tmux needs no fallback at all, which is worth stating because the
//! obvious guess is wrong: tmux SILENTLY IGNORES a client flag it does not
//! know (verified on 3.3a — `attach-session -f no-such-flag` attaches and
//! answers normally), so a tmux without `ignore-size` attaches without it
//! rather than refusing. That loses the second layer and keeps the
//! load-bearing one, because neutrality here rests on declaring no size, not
//! on the flag. If `-f` itself is unsupported the attach fails, `connect`
//! reads the `%error` opening block, and the transport spawns instead.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::time::Duration;

use crate::actuate::host::{HostErr, Socket, TmuxCmd, TmuxOut};
use crate::actuate::redact::redact;
use crate::actuate::runner::{SystemTmuxRunner, TmuxRunner};

/// The flags the control client attaches with, and the reason it is safe.
///
/// `ignore-size` keeps this client out of the session-size calculation, so it
/// can never resize an operator's window. `no-output` suppresses `%output`
/// for every pane in the session. See this module's own doc comment for the
/// measured table behind both.
pub const CLIENT_FLAGS: &str = "no-output,ignore-size";

/// The longest one control-mode command may go unanswered before the client
/// is declared lost.
///
/// The same bound the spawn runner uses for a whole invocation
/// (`runner.rs`'s `DEFAULT_INVOCATION_TIMEOUT`), and for the same reason: a
/// tmux server that accepts a connection and never answers would otherwise
/// wedge a synchronous duty task for the life of the process. Past this the
/// client is dropped and the call is retried on a fresh one, so a stuck
/// server costs one slow call instead of a permanent hang.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

// TWO SECONDS, NOT TEN. Every command this transport sends is a local tmux verb
// against a socket on this machine, and control mode answers them in
// microseconds — 0.086ms per command, measured. The ceiling therefore prices
// nothing but FAILURE, and ten seconds priced it badly: a client whose server
// has gone but whose process lingers answers nothing, and `try_wait` cannot see
// it because the process is genuinely still running. Reproduced under parallel
// load as a call that took 10.06s and then fell back correctly — the answer was
// right and ten seconds late.
//
// Two seconds is still three orders of magnitude above the real cost of the
// slowest verb here, and it bounds the one case that matters: a tmux server
// restart under a live company now costs a blink before the reconnect rather
// than a stall the operator would report as the product hanging.

/// How often a wait for a reply stops to ask whether the client is still alive.
///
/// Short enough that a dead client is noticed in a blink rather than after
/// [`REPLY_TIMEOUT`], long enough that a busy wait costs nothing. See
/// [`ControlClient::await_reply`].
const LIVENESS_HOP: Duration = Duration::from_millis(50);

/// How many replies may queue before the reader thread blocks.
///
/// Commands are serialized by the transport's own mutex, so at most one
/// reply is ever outstanding; the slack absorbs a reply that arrives while a
/// timed-out caller has already walked away.
const REPLY_BACKLOG: usize = 8;

/// One reply block, as the demultiplexer recovered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// tmux's own command number, from `%begin`/`%end`.
    pub num: u64,
    /// Whether the block closed with `%error` rather than `%end`.
    pub failed: bool,
    /// The block body, rebuilt as tmux's own stdout — every line keeps its
    /// terminator, so this is byte-identical to what a spawn would capture.
    pub text: String,
}

impl Reply {
    /// The reply as the spawn transport would have reported the same answer.
    ///
    /// This is the whole error-mapping rule in one place: `%error` is tmux
    /// SAYING NO, which the spawn path reports as a non-zero exit with text
    /// on stderr, so that is what it becomes here. It is never a transport
    /// error.
    #[must_use]
    pub fn into_out(self) -> TmuxOut {
        if self.failed {
            TmuxOut { status: 1, stdout: String::new(), stderr: redact(&self.text) }
        } else {
            TmuxOut { status: 0, stdout: self.text, stderr: String::new() }
        }
    }
}

/// The control-mode stream, demultiplexed.
///
/// Feed it one line at a time. It yields a [`Reply`] only when a block that
/// OPENED with `%begin` closes with `%end` or `%error`; everything else —
/// every notification, and any stray text outside a block — is dropped.
///
/// Kept separate from the reader thread, and from any process, so the
/// framing rules can be tested as rules against a transcript rather than
/// against a live server.
#[derive(Debug, Default)]
pub struct Demux {
    /// The block being accumulated: its command number and its lines so far.
    open: Option<(u64, Vec<String>)>,
    /// Set once `%exit` is seen. The client is going away.
    exited: bool,
}

impl Demux {
    /// A demultiplexer with no block open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the stream has announced `%exit`.
    #[must_use]
    pub const fn exited(&self) -> bool {
        self.exited
    }

    /// Offer one line; get a reply back when that line completed a block.
    pub fn push(&mut self, line: &str) -> Option<Reply> {
        // Inside a block, ONLY the matching terminator ends it. tmux does not
        // nest blocks, so any other line — including one that starts with a
        // `%` — is body text and is kept verbatim.
        if let Some((num, lines)) = self.open.as_mut() {
            if let Some(rest) = line.strip_prefix("%end ") {
                if block_number(rest) == Some(*num) {
                    return self.close(false);
                }
            }
            if let Some(rest) = line.strip_prefix("%error ") {
                if block_number(rest) == Some(*num) {
                    return self.close(true);
                }
            }
            lines.push(line.to_owned());
            return None;
        }

        // Outside a block: `%begin` opens one, `%exit` says the client is
        // finished, and every other notification is not our business.
        if let Some(rest) = line.strip_prefix("%begin ") {
            if let Some(num) = block_number(rest) {
                self.open = Some((num, Vec::new()));
            }
        } else if line == "%exit" || line.starts_with("%exit ") {
            self.exited = true;
        }
        None
    }

    /// Close the open block and hand it back.
    ///
    /// The body is rebuilt as tmux's own stdout, which means every line keeps
    /// its terminator. Control mode strips the newline that ended each line;
    /// a spawn does not, so `lines.join("\n")` would make the two transports
    /// DISTINGUISHABLE by exactly one trailing byte — and every caller that
    /// compares stdout without trimming would change behaviour under the new
    /// transport. Rebuilding it here is what keeps the seam honest.
    ///
    /// Zero lines and one empty line are different answers ("" and "\n"), and
    /// the emptiness test below is what keeps them different.
    fn close(&mut self, failed: bool) -> Option<Reply> {
        let (num, lines) = self.open.take()?;
        let text = if lines.is_empty() { String::new() } else { format!("{}\n", lines.join("\n")) };
        Some(Reply { num, failed, text })
    }
}

/// The command number in a `%begin`/`%end`/`%error` tail.
///
/// The tail is `<timestamp> <number> <flags>`; the number is the second
/// field. Parsed rather than counted so a desynchronised stream is
/// detectable instead of silently mis-attributing one command's answer to
/// another.
fn block_number(rest: &str) -> Option<u64> {
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Quote one argument so tmux's lexer splits the line back into the IDENTICAL
/// argv the spawn transport would have passed.
///
/// Single quotes, with an embedded quote written as `'\''` — the shell's own
/// trick, which tmux's lexer also honours. Verified against tmux 3.3a to
/// round-trip spaces, tabs, `;`, `$`, `\`, `"`, `'`, `#{…}`, `#[…]` and
/// leading dashes byte-for-byte, compared against the same value delivered
/// through `Command::args`.
///
/// Note what this CANNOT represent: a newline, which terminates the command
/// line itself. [`quote_argv`] refuses those rather than truncating a
/// command, and the caller spawns instead.
fn quote_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// The whole command as one control-mode line, or `None` when it cannot be
/// written as one.
///
/// A newline inside an argument is the only unrepresentable case, and it is
/// refused rather than mangled: half a command reaching tmux is worse than
/// no command reaching it, and the caller has a spawn path that carries the
/// argument correctly.
#[must_use]
pub fn quote_argv(argv: &[impl AsRef<str>]) -> Option<Line> {
    if argv.iter().any(|a| a.as_ref().contains('\n')) {
        return None;
    }
    // A BARE `;` IS A COMMAND SEPARATOR AND MUST NOT BE QUOTED.
    //
    // This is the defect that took the operator's company down. Every argument
    // was quoted, INCLUDING the `;` this repo passes as its own argv element to
    // batch a sequence — and this repo batches everywhere, deliberately, because
    // tmux renders once per sequence and separate invocations are separate
    // frames.
    //
    // Quoted, tmux reads it as an ARGUMENT rather than a separator. Measured
    // against a real tmux:
    //
    // ```text
    //   'display-message' '-p' … ';' 'display-message' …
    //   → parse error: command display-message: too many arguments (need at most 1)
    //   → %error
    //   → %exit
    // ```
    //
    // So every batched command failed, the client died on the `%exit`, the next
    // call reconnected and failed the same way, and every read answered EMPTY.
    // An empty answer is a legitimate "not found" everywhere in this code, so
    // `department_window` concluded the engineering window did not exist and
    // minted one — whose rail asked the same question. Windows @64..@69 in two
    // seconds, until the tmux server was gone.
    //
    // The separator also changes the REPLY COUNT: tmux answers one
    // `%begin`/`%end` block PER COMMAND, so a batched line yields N replies and
    // a reader expecting one leaves N-1 in the channel to corrupt later calls.
    // [`Line::blocks`] is that count, and [`ControlClient::run`] collects them
    // all — which is also what makes this byte-identical to the spawn path,
    // where `tmux a ; b` returns the concatenated stdout of both.
    let mut rendered = Vec::with_capacity(argv.len());
    let mut blocks = 1usize;
    for arg in argv {
        if arg.as_ref() == ";" {
            rendered.push(";".to_owned());
            blocks += 1;
        } else {
            rendered.push(quote_arg(arg.as_ref()));
        }
    }
    Some(Line { text: rendered.join(" "), blocks })
}

/// One control-mode command line, and how many reply blocks it will produce.
///
/// The count is not cosmetic: tmux answers per COMMAND, not per line, so a
/// batched sequence must be read to the end or its surplus blocks become the
/// answers to later calls. See [`quote_argv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line as tmux will parse it.
    pub text: String,
    /// How many `%begin`/`%end` blocks tmux will answer with.
    pub blocks: usize,
}

/// A live control-mode client: one tmux process, held open.
#[derive(Debug)]
pub struct ControlClient {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Reply>,
}

impl ControlClient {
    /// Attach a control client to `session` on `socket`.
    ///
    /// # Errors
    /// [`Attach`], which separates the one failure that is a RACE — the
    /// session is not there yet — from every failure that is an answer about
    /// this host. See that type for why the distinction is load-bearing.
    pub fn connect(binary: &str, socket: &Socket, session: &str) -> Result<Self, Attach> {
        let unavailable =
            |detail: String| Attach::Unavailable(HostErr::ToolUnavailable { tool: "tmux", detail });
        let mut child = Command::new(binary)
            .arg("-L")
            .arg(&socket.0)
            .arg("-C")
            .arg("attach-session")
            .arg("-f")
            .arg(CLIENT_FLAGS)
            .arg("-t")
            .arg(session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| unavailable(redact(&error.to_string())))?;

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable("control-mode stdin could not be taken".to_owned()));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable("control-mode stdout could not be taken".to_owned()));
        };

        let (tx, replies) = sync_channel(REPLY_BACKLOG);
        std::thread::spawn(move || pump(stdout, &tx));

        let client = Self { child, stdin, replies };
        // The attach itself answers with one block before any command of
        // ours. Consuming it here is what makes the Nth reply the answer to
        // the Nth command for every caller after this point; leaving it in
        // the channel would shift every reply by one.
        //
        // That block is also where a FAILED attach shows up, and it must be
        // read: attaching to a session that does not exist answers
        // `%begin` / `can't find session` / `%error` and then `%exit`, so a
        // client that took any block as success would report itself live,
        // hand back a lost-client error on the very next command, and
        // reconnect into the same failure forever.
        match client.replies.recv_timeout(REPLY_TIMEOUT) {
            Ok(reply) if !reply.failed => Ok(client),
            Ok(reply) => {
                let text = redact(reply.text.trim());
                let detail = format!("tmux control mode could not attach to '{session}': {text}");
                let error = HostErr::ToolUnavailable { tool: "tmux", detail };
                // BOTH SIGNALS, AND NEITHER ALONE. Getting a block at all
                // proves tmux ran and spoke control mode, so the capability is
                // not in question; the TEXT is what says the refusal is about
                // the session rather than about the client we asked for. A
                // block alone would also cover a tmux too old for
                // [`CLIENT_FLAGS`]' `-f`, which is a real and permanent limit
                // of the host and must keep the long sentence.
                if session_absent(&text) {
                    Err(Attach::Absent(error))
                } else {
                    Err(Attach::Unavailable(error))
                }
            }
            Err(_) => Err(unavailable(format!(
                "tmux control mode did not attach to '{session}' within {}ms",
                REPLY_TIMEOUT.as_millis(),
            ))),
        }
    }

    /// Run one command and return its reply.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] when the command cannot be written, when
    /// no reply arrives within [`REPLY_TIMEOUT`], or when the stream ended —
    /// all of which mean this client is finished and the caller must
    /// reconnect or spawn.
    pub fn run(&mut self, line: &Line) -> Result<Reply, HostErr> {
        let unavailable = |detail: String| HostErr::ToolUnavailable { tool: "tmux", detail };
        // ANYTHING ALREADY IN THE CHANNEL IS NOT THE ANSWER TO A COMMAND WE
        // HAVE NOT SENT YET.
        //
        // Replies are correlated to commands by ORDER — control mode answers in
        // the order it was asked — so a block sitting here before the write is
        // by construction stale, and taking it would answer this command with
        // the previous one's output. Every subsequent command would then be one
        // reply behind, which is the same silent off-by-one that made a chained
        // `show-options` read shift every value up by one.
        //
        // MEASURED: killing the server mid-session left one block queued behind
        // the `%exit` that ended the pump. The next call wrote, read that stale
        // block, and answered `list-sessions` with an empty string — so the
        // reconnect never fired, because from `run`'s point of view nothing had
        // failed. It surfaced as `a_lost_server_reconnects_on_the_next_call`
        // failing only under parallel load, which is exactly how a race
        // announces itself.
        while self.replies.try_recv().is_ok() {}
        writeln!(self.stdin, "{}", line.text)
            .and_then(|()| self.stdin.flush())
            .map_err(|error| unavailable(redact(&error.to_string())))?;
        // ONE BLOCK PER COMMAND, and the line may carry several. Reading only
        // the first would answer the caller with the first command's output and
        // leave the rest in the channel to become the answers to later calls —
        // the same silent off-by-one the drain above exists for, arriving by a
        // different door. See `quote_argv`.
        //
        // The blocks are concatenated because that is what the spawn path
        // returns for the same line: `tmux a ; b` captures both commands'
        // stdout. A FAILED block anywhere makes the whole line failed, exactly
        // as a non-zero exit does for a spawn — a sequence is one command as far
        // as every caller here is concerned.
        let mut text = String::new();
        let mut failed = false;
        let mut num = 0;
        for _ in 0..line.blocks.max(1) {
            let reply = self.await_reply()?;
            num = reply.num;
            failed |= reply.failed;
            text.push_str(&reply.text);
            // TMUX ABORTS A SEQUENCE AT THE FIRST FAILURE, so there are no
            // further blocks to wait for — and waiting would stall this call for
            // the whole reply timeout before failing anyway.
            //
            // Verified against a real tmux with stdin held open (an earlier
            // probe that closed stdin made this look like the client dying,
            // which it does not): `kill-pane -t %9999 ; display-message …`
            // answers with ONE `%error` block and nothing for the second
            // command, and the client stays live and answers the next call
            // normally.
            //
            // This is also exactly what the spawn path does — `tmux a ; b` with
            // `a` failing exits 1 and never runs `b`, measured — so stopping
            // here keeps the two transports equivalent rather than merely fast.
            // A failed batch is ORDINARY here: killing a pane that has already
            // gone is something this codebase does routinely.
            if reply.failed {
                break;
            }
        }
        Ok(Reply { num, failed, text })
    }

    /// Wait for one reply block, giving up promptly on a client that has DIED.
    ///
    /// # The ten-second stall this ends
    ///
    /// A single `recv_timeout(REPLY_TIMEOUT)` is only prompt when the pump
    /// thread notices the stream ended and drops its sender — and there is a
    /// race where it has not yet: the write lands in a pipe buffer belonging to
    /// a process that is already gone, so nothing errors, no reply is coming,
    /// and the call waits the FULL ten seconds before failing. Reproduced at
    /// roughly one run in two by killing the server between two commands; the
    /// tell is a test that takes 10.06s instead of 0.07s.
    ///
    /// That is not merely a slow test. A tmux server restart under a live
    /// company would stall the click path for ten seconds before the reconnect
    /// that was going to fix it even began.
    ///
    /// So the wait is broken into short hops that also ask whether the CHILD is
    /// still running. `try_wait` is the authority the channel cannot be: a
    /// process that has exited will never produce another block, whatever the
    /// pump thread has got around to noticing. The total budget is unchanged, so
    /// a legitimately slow command still gets its full [`REPLY_TIMEOUT`].
    fn await_reply(&mut self) -> Result<Reply, HostErr> {
        let unavailable = |detail: String| HostErr::ToolUnavailable { tool: "tmux", detail };
        let deadline = std::time::Instant::now() + REPLY_TIMEOUT;
        loop {
            match self.replies.recv_timeout(LIVENESS_HOP) {
                Ok(reply) => return Ok(reply),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(unavailable("the tmux control client ended".to_owned()))
                }
                Err(RecvTimeoutError::Timeout) => {
                    // The channel has said nothing yet. Ask the process itself:
                    // an exited child is a reply that is never coming, and the
                    // pump may not have noticed the stream end.
                    if matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
                        return Err(unavailable("the tmux control client exited".to_owned()));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(unavailable(format!(
                            "tmux control mode did not answer within {}ms",
                            REPLY_TIMEOUT.as_millis(),
                        )));
                    }
                }
            }
        }
    }
}

impl Drop for ControlClient {
    /// Close stdin so tmux detaches, then reap. A dropped child is a zombie
    /// (`spawn.rs`'s #61 lesson), so the wait is not optional.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the control stream, demultiplex it, and forward replies.
///
/// Ends when the stream does. Dropping the sender is what turns a dead
/// server into a prompt `Disconnected` for whoever is waiting, rather than a
/// wait for the full timeout.
fn pump(stdout: impl std::io::Read, tx: &SyncSender<Reply>) {
    let mut demux = Demux::new();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Some(reply) = demux.push(&line) {
            if tx.send(reply).is_err() {
                break;
            }
        }
        if demux.exited() {
            break;
        }
    }
}

/// Why an attach produced no client — and, decisively, whether that answer is
/// about this HOST or about the WORLD.
///
/// # The thirty seconds this type exists to stop
///
/// Measured on the operator's own box, 2026-08-16. `chief actuate` starts its
/// converge loop and its session brain while `chief attach` is still minting
/// the tmux session: `actuator.start` at 05:07:22.005, the session present at
/// 05:07:22.115. Both of that process's transports issued their first tmux
/// verb inside those 110ms, both attaches answered `can't find session`, and
/// both were written off as a CAPABILITY answer — so every tmux verb in the
/// company ran as a ~25ms process spawn until 05:07:52.021, thirty seconds
/// later. Every click the operator made landed in that window: a department
/// click took 483ms, and the same click after the transport came back took
/// **25.6ms**. The whole of "a lot of flashing" was this.
///
/// A missing session is not an answer about the host. It is an ordering race
/// against a session chief itself is in the middle of creating, and the
/// transport must ask again on a human-invisible interval rather than serve a
/// thirty-second sentence for it.
#[derive(Debug)]
pub enum Attach {
    /// tmux ran, spoke control mode, and its answer was that there is no such
    /// session. Nothing about this host is in doubt; there is simply nothing
    /// to attach to YET. Retried after [`ABSENT_RETRY`].
    Absent(HostErr),
    /// Everything else — tmux could not be run, its pipes could not be taken,
    /// the attach never answered, or tmux refused for a reason of its own.
    /// This IS the capability answer, and it keeps [`CAPABILITY_RETRY`].
    Unavailable(HostErr),
}

impl std::fmt::Display for Attach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(error) | Self::Unavailable(error) => error.fmt(f),
        }
    }
}

/// Whether tmux's refusal says the SESSION is missing rather than anything
/// about this host.
///
/// tmux has two phrasings and this product meets both. Probed against real
/// binaries on 2026-08-16, tmux 3.3a and tmux 3.7b, character for character:
///
/// * a server that is up but has no session by that name — `%begin`,
///   `can't find session: org-absent_`, `%error`, `%exit`;
/// * no server on that socket at all — `%begin`, `no sessions`, `%error`,
///   `%exit`.
///
/// Both are the same fact from the transport's point of view, and both are
/// what a company looks like in the moment before `chief attach` finishes
/// minting its session.
///
/// A rewording upstream costs the long sentence again, which is exactly
/// today's behaviour — so the failure mode of this test is the state it
/// replaces, never something worse.
fn session_absent(text: &str) -> bool {
    text.contains("can't find session") || text.contains("no sessions")
}

/// Whether this transport has a control client, and whether it may look for
/// one.
#[derive(Debug)]
enum Slot {
    /// Never attempted. The next call decides whether this host supports it.
    Cold,
    /// Attached and answering.
    Live(Box<ControlClient>),
    /// Was live, then lost. The next call reconnects.
    Lost,
    /// The SESSION was not there, as of `since`.
    ///
    /// The capability question was never asked — tmux spoke control mode
    /// perfectly well and told us the session does not exist — so this is not
    /// [`Slot::Unsupported`] and must never be given its sentence. See
    /// [`Attach`] for the thirty seconds that distinction is worth.
    Absent { since: std::time::Instant },
    /// No control client could be had, as of `since`.
    ///
    /// # This used to be permanent, and that was the defect
    ///
    /// It latched on the FIRST attempt and was never retried, on the reasoning
    /// that the first attempt is the capability question. That reasoning holds
    /// for a host that cannot do control mode at all — and it is wrong for
    /// every host that can, because the first attempt is also the one most
    /// likely to land in a bad moment. The rail's first tmux call happens
    /// milliseconds after `split-window` mints its pane, and the actuator's
    /// happens while a session is still being set up.
    ///
    /// A miss there is not a capability answer, it is a race. Latching it made
    /// a long-lived process spawn every command for the rest of its life at
    /// ~25ms each, and said nothing. A capability that can be lost to a race
    /// must be regainable, so this carries WHEN and is retried after
    /// [`CAPABILITY_RETRY`].
    ///
    /// **And the commonest race does not reach this state at all any more.**
    /// Making the sentence thirty seconds instead of forever was strictly
    /// better and still far too long for the one that actually fires: a
    /// session that has not been minted yet lands in [`Slot::Absent`], whose
    /// wait is [`ABSENT_RETRY`].
    Unsupported { since: std::time::Instant },
}

/// How long a transport stays on the spawn path before asking again whether it
/// can hold a control client.
///
/// Long enough that a host which genuinely cannot do control mode pays one
/// failed attach per interval rather than one per command, and short enough
/// that an operator does not sit through a slow surface waiting for it. The
/// attach it costs is the same one `connect` already makes.
const CAPABILITY_RETRY: Duration = Duration::from_secs(30);

/// How long a transport waits before it asks again, when the only thing wrong
/// is that its session does not exist yet.
///
/// Derived from the budget this product is held to rather than guessed:
/// the design record sets a 50ms ceiling on a click, so a retry at 50ms
/// costs a gesture that lands mid-race **at most one degraded frame** before
/// the fast path is back. The race it covers is the one measured in [`Attach`]
/// and lasts ~110ms.
///
/// It is not zero, because zero would mean a doomed attach on every command
/// for as long as a session is missing, and a session an operator has killed
/// stays missing. Bounding it by TIME rather than by call count makes the cost
/// the same whether one verb or a thousand are issued while it lasts.
const ABSENT_RETRY: Duration = Duration::from_millis(50);

/// The transport: a control client when one can be had, a spawn when not.
///
/// One `ControlTransport` serves both seams — [`TmuxRunner`] for the actuate
/// path and [`crate::sidebar::Tmux`] for the rail — because both are the same
/// question asked twice: one command in, one captured answer out.
#[derive(Debug)]
pub struct ControlTransport {
    slot: Mutex<Slot>,
    fallback: SystemTmuxRunner,
    binary: String,
    socket: Socket,
    session: String,
}

impl ControlTransport {
    /// A transport that will attach to `session` on `socket`.
    ///
    /// Nothing connects here. The first command decides, so constructing a
    /// transport for a company whose session does not exist yet costs
    /// nothing and refuses nothing.
    #[must_use]
    pub fn new(socket: Socket, session: String) -> Self {
        Self {
            slot: Mutex::new(Slot::Cold),
            fallback: SystemTmuxRunner::default(),
            binary: "tmux".to_owned(),
            socket,
            session,
        }
    }

    /// The same transport against a specific tmux binary. Tests and the e2e
    /// harness pin one, exactly as `SystemTmuxRunner::with_binary` does.
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        let binary = binary.into();
        self.fallback = SystemTmuxRunner::with_binary(&binary);
        self.binary = binary;
        self
    }

    /// Whether a control client is currently attached. Observability, and the
    /// hook the size-neutrality test needs to know it measured the real path.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.slot.lock().is_ok_and(|slot| matches!(*slot, Slot::Live(_)))
    }

    /// Run one command over the control client, or say why it could not.
    ///
    /// `Err(Spawn)` is the signal to spawn, and it CARRIES ITS REASON — see
    /// [`Spawn`] for why an untyped `None` was not enough.
    fn over_control(
        &self,
        socket: &Socket,
        cmd: &TmuxCmd,
    ) -> Result<Result<TmuxOut, HostErr>, Spawn> {
        // A different socket than the one this transport attached to has no
        // client here. That is not a failure; it is a command for somewhere
        // else, and it spawns.
        if socket != &self.socket {
            return Err(Spawn::AnotherServer);
        }
        // A control block closes when tmux accepts the outer `if-shell`.
        // Output from a command inside either branch arrives later and cannot
        // be attributed to this call. The spawn transport captures the whole
        // nested command and leaves no reply for a later call.
        if if_shell_has_command_branch(&cmd.argv) {
            return Err(Spawn::NotRepresentable);
        }
        let Some(line) = quote_argv(&cmd.argv) else {
            return Err(Spawn::NotRepresentable);
        };
        let Ok(mut slot) = self.slot.lock() else {
            return Err(Spawn::Degraded(Degraded {
                reason: "the transport's own lock is poisoned".to_owned(),
                retry: CAPABILITY_RETRY,
            }));
        };

        // NOT "never ask again" ANY MORE, and the WAIT IS CHOSEN BY THE
        // REASON. A latch for the life of the process turned one bad moment
        // into a permanently slow process that said nothing about it; a single
        // thirty-second wait for every reason then turned a 110ms startup race
        // into thirty seconds of it. See `Slot::Absent` and `Attach`.
        let waiting = match &*slot {
            Slot::Absent { since } => since.elapsed() < ABSENT_RETRY,
            Slot::Unsupported { since } => since.elapsed() < CAPABILITY_RETRY,
            Slot::Cold | Slot::Live(_) | Slot::Lost => false,
        };
        if waiting {
            return Err(Spawn::Silent);
        }
        if !matches!(*slot, Slot::Live(_)) {
            let regaining = !matches!(*slot, Slot::Cold);
            match ControlClient::connect(&self.binary, &self.socket, &self.session) {
                Ok(client) => {
                    if regaining {
                        self.announce_regained();
                    }
                    *slot = Slot::Live(Box::new(client));
                }
                Err(failure) => {
                    let reason = format!("{failure}");
                    let (next, retry) = match failure {
                        Attach::Absent(_) => {
                            (Slot::Absent { since: std::time::Instant::now() }, ABSENT_RETRY)
                        }
                        Attach::Unavailable(_) => (
                            Slot::Unsupported { since: std::time::Instant::now() },
                            CAPABILITY_RETRY,
                        ),
                    };
                    // The EDGE is what is worth saying, and moving between the
                    // two down states is an edge: the retry interval the line
                    // reports changes by a factor of six hundred, so a reader
                    // who saw only the first line would be told the wrong
                    // thing about how long this lasts.
                    let already_said =
                        std::mem::discriminant(&*slot) == std::mem::discriminant(&next);
                    *slot = next;
                    return Err(if already_said {
                        Spawn::Silent
                    } else {
                        Spawn::Degraded(Degraded { reason, retry })
                    });
                }
            }
        }

        let Slot::Live(client) = &mut *slot else {
            return Err(Spawn::Degraded(Degraded {
                reason: "the control client vanished mid-call".to_owned(),
                retry: CAPABILITY_RETRY,
            }));
        };
        match client.run(&line) {
            Ok(reply) => Ok(Ok(reply.into_out())),
            Err(_) => {
                // The client is finished. Reconnect once, in this same call,
                // so a server that restarted between two commands costs one
                // reconnect rather than one failed gesture.
                *slot = Slot::Lost;
                match ControlClient::connect(&self.binary, &self.socket, &self.session) {
                    Ok(mut fresh) => {
                        let answer = fresh.run(&line);
                        *slot = Slot::Live(Box::new(fresh));
                        answer.map(|reply| Ok(reply.into_out())).map_err(|error| {
                            Spawn::Degraded(Degraded {
                                reason: format!("{error}"),
                                retry: CAPABILITY_RETRY,
                            })
                        })
                    }
                    Err(failure) => {
                        let reason = format!("{failure}");
                        let retry = match failure {
                            Attach::Absent(_) => {
                                *slot = Slot::Absent { since: std::time::Instant::now() };
                                ABSENT_RETRY
                            }
                            Attach::Unavailable(_) => {
                                *slot = Slot::Unsupported { since: std::time::Instant::now() };
                                CAPABILITY_RETRY
                            }
                        };
                        Err(Spawn::Degraded(Degraded { reason, retry }))
                    }
                }
            }
        }
    }

    /// Say that the fast path is back, on the edge where it returns.
    fn announce_regained(&self) {
        tracing::info!(
            event = "tmux.control.regained",
            session = %self.session,
            "the control client is attached again; commands are back on the fast path"
        );
    }
}

fn if_shell_has_command_branch(argv: &[String]) -> bool {
    if argv.first().map(String::as_str) != Some("if-shell") {
        return false;
    }
    let mut index = 1;
    while let Some(argument) = argv.get(index).map(String::as_str) {
        match argument {
            "-b" | "-F" => index += 1,
            "-t" => index += 2,
            "--" => {
                index += 1;
                break;
            }
            _ if argument.starts_with('-') => return true,
            _ => break,
        }
    }
    argv.get(index.saturating_add(1)..)
        .is_some_and(|branches| branches.iter().any(|branch| !branch.is_empty()))
}

/// Why a command is about to be run as a PROCESS instead of over the client.
///
/// # Why this is typed rather than an `Option`
///
/// It used to be `Option`, and `None` meant seven different things. Three of
/// them are ordinary routing and three are a degradation the operator cannot
/// see, and they were spelled identically — so a transport that had silently
/// fallen back to ~25ms-per-command spawns for the life of a process looked,
/// in the record, exactly like one that was working. The operator asked why
/// tmux felt slow and nothing in the logs could answer them.
///
/// The distinction this type draws is the one that matters to a reader of the
/// log: **did we choose to spawn, or did we lose the ability not to?**
#[derive(Debug)]
enum Spawn {
    /// A command aimed at a different tmux server. There is no client here to
    /// carry it and there never was one to lose. Not a degradation, and
    /// deliberately silent — this fires whenever a verb targets another
    /// socket, which is ordinary.
    AnotherServer,
    /// A command no control line can carry. `quote_argv` refuses an argv
    /// holding a newline, because a newline TERMINATES the command line and
    /// half the command would reach tmux — the spawn path carries it
    /// correctly. A real limit of the wire format, not of this host, so it is
    /// also silent.
    NotRepresentable,
    /// The fast path was lost, and this is the edge where it happened. Said
    /// out loud, once, with the reason and with how long it lasts.
    Degraded(Degraded),
    /// Still degraded, already reported. Silent so a permanent condition
    /// cannot fill the log at one line per command.
    Silent,
}

/// What the operator is told when the fast path goes: why, and for how long.
///
/// The duration is part of the fact and not decoration. "tmux verbs are
/// spawning" is a very different report at 50ms than at 30 seconds, and the
/// line that reported a single hard-coded 30 is what made a 110ms startup race
/// read, in the log, as a host that cannot do control mode at all.
#[derive(Debug)]
struct Degraded {
    /// The failure, in tmux's own words.
    reason: String,
    /// How long this transport waits before it asks again.
    retry: Duration,
}

impl TmuxRunner for ControlTransport {
    /// One command, over the control client when there is one.
    ///
    /// # Errors
    /// Exactly what the spawn runner returns, because when control mode is
    /// not available this IS the spawn runner. A failure here is
    /// indistinguishable in behaviour from a failed spawn today, which is
    /// what keeps the trust rules above it unchanged.
    fn run(&self, socket: &Socket, cmd: &TmuxCmd) -> Result<TmuxOut, HostErr> {
        match self.over_control(socket, cmd) {
            Ok(answer) => answer,
            Err(reason) => {
                // ONE LINE PER TRANSITION, NOT PER COMMAND. `Spawn::Silent` is
                // the already-reported case and is what keeps a permanently
                // degraded transport from writing a line for every verb it
                // runs — which, on a rail asking tmux several times a gesture,
                // would bury the very event worth finding.
                //
                // `info` and not `debug`: the operator's own box captures no
                // debug events at all, so a debug line here would be exactly
                // as invisible as the silence it replaces.
                if let Spawn::Degraded(degraded) = &reason {
                    tracing::info!(
                        event = "tmux.control.degraded",
                        session = %self.session,
                        reason = %degraded.reason,
                        retry_in_ms = degraded.retry.as_millis(),
                        "the control client could not be had, so tmux commands are running as \
                         PROCESSES at roughly 25ms each instead of under a millisecond; this \
                         is the slow path and it is not permanent"
                    );
                }
                self.fallback.run(socket, cmd)
            }
        }
    }
}

impl crate::sidebar::Tmux for ControlTransport {
    /// The rail's seam: stdout, or empty when the command failed.
    ///
    /// The rail's contract is that a stale verb is a no-op it survives, not
    /// an error it reports, so a non-zero status and a transport failure
    /// collapse to the same empty string here — the same collapse the spawn
    /// implementation makes.
    ///
    /// The output is TRIMMED, because the rail's spawn implementation trims
    /// (`tmux.rs`'s `spawn_once`) and the rail's readers are written against
    /// that. The actuate seam above deliberately does not trim, because ITS
    /// spawn implementation does not. The two seams disagree, and this
    /// transport matches each rather than picking one and moving a caller.
    fn run(&self, args: &[&str]) -> String {
        let started = std::time::Instant::now();
        let argv = args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let cmd = TmuxCmd { argv };
        let out = TmuxRunner::run(self, &self.socket.clone(), &cmd);
        // EVERY VERB THE RAIL ISSUES, IN THE FILE. `trace`, so it costs
        // nothing until somebody goes looking with `RUST_LOG=chief_cli=trace`
        // — and then it is the whole story: a flicker is a sequence of tmux
        // commands, and without this the only way to see that sequence is to
        // guess it from the source. The gestures this surface performs are
        // two or three verbs long; a churn loop is the same three verbs
        // repeating, and that shape is unmistakable once it is written down.
        //
        // `transport` is new and is the reason this line survived the move
        // off the spawn implementation: with two transports in the product,
        // "which one answered this verb" is the first question a slow gesture
        // raises, and `elapsed_us` is meaningless without it.
        tracing::trace!(
            event = "sidebar.tmux",
            verb = args.first().copied().unwrap_or_default(),
            argv = %args.join(" "),
            exit = out.as_ref().map_or(-1, |out| out.status),
            transport = if self.is_live() { "control" } else { "spawn" },
            elapsed_us = started.elapsed().as_micros(),
            "the rail asked tmux for something"
        );
        match out {
            Ok(out) if out.status == 0 => out.stdout.trim().to_owned(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests;
