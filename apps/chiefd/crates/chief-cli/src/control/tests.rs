//! The rules the control-mode transport must keep.
//!
//! Two halves. Above the divider, the framing and quoting rules are pinned
//! against TRANSCRIPTS — no process, no server — because they are rules about
//! bytes and deserve to fail for one reason each. Below it, the size,
//! equivalence and reconnect rules are pinned against a REAL tmux, because
//! every one of them is a claim about what tmux does, and this transport
//! exists only because assumptions about tmux semantics cost two separate
//! bugs in one day.

use super::*;

// ---------------------------------------------------------------- framing --

#[test]
fn a_reply_is_the_text_between_begin_and_end() {
    let mut demux = Demux::new();
    assert_eq!(demux.push("%begin 1786752184 273 1"), None);
    assert_eq!(demux.push("first"), None);
    assert_eq!(demux.push("second"), None);
    let reply = demux.push("%end 1786752184 273 1").expect("the block closed");
    assert_eq!(
        reply,
        Reply { num: 273, failed: false, text: "first\nsecond\n".to_owned() },
        "every body line, in order, and nothing else"
    );
}

#[test]
fn a_reply_is_not_confused_by_an_interleaved_notification() {
    // THE demultiplexer rule. tmux writes notifications between blocks, and a
    // line reader would hand one back as a command's answer — the failure
    // this state machine exists to make impossible.
    let mut demux = Demux::new();
    for noise in [
        "%output %1 some pane wrote this",
        "%window-add @3",
        "%sessions-changed",
        "%layout-change @1 b25d,80x24,0,0,1",
        "%client-detached client-99",
    ] {
        assert_eq!(demux.push(noise), None, "'{noise}' is a notification, never a reply");
    }
    assert_eq!(demux.push("%begin 1786752184 300 1"), None);
    assert_eq!(demux.push("the answer"), None);
    let reply = demux.push("%end 1786752184 300 1").expect("the block closed");
    assert_eq!(
        reply.text, "the answer\n",
        "the reply is the block body; the notifications before it left no trace"
    );
}

#[test]
fn a_terminator_for_another_command_does_not_close_this_block() {
    // Correlation is by tmux's own command number, not by "the next line that
    // looks like an end". A mismatched terminator is body text, and treating
    // it as the close would mis-attribute one command's answer to another.
    let mut demux = Demux::new();
    assert_eq!(demux.push("%begin 1786752184 400 1"), None);
    assert_eq!(demux.push("%end 1786752184 399 1"), None, "399 is not 400");
    let reply = demux.push("%end 1786752184 400 1").expect("the matching terminator closed it");
    assert_eq!(reply.num, 400);
    assert_eq!(reply.text, "%end 1786752184 399 1\n", "the mismatched line was body");
}

#[test]
fn an_error_block_is_tmux_saying_no_not_a_transport_failure() {
    // The one confusion this module must never make. Under the spawn
    // transport "tmux said no" is a non-zero exit with text on stderr, and
    // the trust rules classify it. Reporting it as a transport failure would
    // turn "tmux answered no" into "tmux did not answer", which the actuate
    // path reads as Untrusted.
    let mut demux = Demux::new();
    assert_eq!(demux.push("%begin 1786751599 283 1"), None);
    assert_eq!(demux.push("can't find session: nonexist"), None);
    let reply = demux.push("%error 1786751599 283 1").expect("the block closed as an error");
    assert!(reply.failed, "%error is a failure of the COMMAND");

    let out = reply.into_out();
    assert_eq!(out.status, 1, "a refusal is a non-zero status, exactly as a spawn reports it");
    // No trailing newline, because the stderr of BOTH transports goes
    // through `redact`, which trims. Matching the spawn path is the rule; the
    // trim is where that rule happens to land.
    assert_eq!(out.stderr, "can't find session: nonexist", "and its text is on stderr");
    assert!(out.stdout.is_empty(), "a refusal has no stdout");
}

#[test]
fn a_successful_reply_is_stdout_with_a_zero_status() {
    let out = Reply { num: 1, failed: false, text: "org-acme_:0\n".to_owned() }.into_out();
    assert_eq!(out.status, 0);
    assert_eq!(out.stdout, "org-acme_:0\n");
    assert!(out.stderr.is_empty());
}

#[test]
fn an_empty_block_is_an_empty_answer_not_a_missing_one() {
    // `set-option` and friends answer with a block containing nothing. That
    // is success with no output, and must not be mistaken for a lost reply.
    let mut demux = Demux::new();
    demux.push("%begin 1786752184 272 1");
    let reply = demux.push("%end 1786752184 272 1").expect("an empty block still closes");
    assert_eq!(reply.text, "");
    assert_eq!(reply.into_out().status, 0);
}

#[test]
fn exit_marks_the_stream_finished() {
    let mut demux = Demux::new();
    assert!(!demux.exited());
    demux.push("%exit");
    assert!(demux.exited(), "%exit means the client is going away");
}

// ---------------------------------------------------------------- quoting --

#[test]
fn quoting_survives_every_character_tmux_would_otherwise_eat() {
    // Verified against tmux 3.3a by round-tripping each of these through
    // `set-option`/`show-options` and comparing with the same value delivered
    // through `Command::args`. The live half of that comparison is
    // `the_control_transport_answers_exactly_what_a_spawn_answers` below.
    for hostile in [
        "plain",
        "hello world",
        "#{pane_id}",
        "#[fg=red]",
        "a;b",
        "a$b",
        "a\\b",
        "it's",
        "a\"b",
        "a\tb",
        "-leading-dash",
        "a}b{c",
        "",
    ] {
        let line = quote_argv(&[hostile]).expect("no newline, so it is representable").text;
        assert!(
            line.starts_with('\'') && line.ends_with('\''),
            "every argument is single-quoted: {line}"
        );
    }
}

#[test]
fn an_embedded_single_quote_is_closed_and_reopened() {
    assert_eq!(
        quote_argv(&["it's"]).expect("representable").text,
        r"'it'\''s'",
        "the shell's own trick, which tmux's lexer honours"
    );
}

#[test]
fn arguments_are_joined_by_spaces_each_quoted_separately() {
    assert_eq!(
        quote_argv(&["display-message", "-p", "a b"]).expect("representable").text,
        "'display-message' '-p' 'a b'",
        "the lexer must split this back into the identical argv"
    );
}

#[test]
fn a_newline_argument_is_refused_rather_than_truncated() {
    // A newline terminates the command line itself, so half a command would
    // reach tmux. Refusing sends the caller to the spawn path, which carries
    // the argument correctly — a truncated command would not just fail, it
    // would run something ELSE.
    assert_eq!(quote_argv(&["send-keys", "line1\nline2"]), None);
    assert!(quote_argv(&["ok", "also ok"]).is_some(), "a newline-free command is representable");
}

// ------------------------------------------------------------- live tmux --
//
// Everything below drives a real tmux server on a private socket. These are
// the claims about tmux's own behaviour, and none of them can be made against
// a transcript.

/// A private tmux server for one test, torn down when the test ends.
struct Server {
    socket: String,
}

impl Server {
    /// A server with one session `org-test_`, sized `width`x`height`, and
    /// `window-size latest` — tmux's DEFAULT, and the setting under which a
    /// sized client resizes the operator.
    fn start(name: &str, width: u32, height: u32) -> Self {
        let socket = format!("chief-ctl-test-{name}-{}", std::process::id());
        let server = Self { socket };
        server.cli(&[
            "new-session",
            "-d",
            "-s",
            "org-test_",
            "-x",
            &width.to_string(),
            "-y",
            &height.to_string(),
        ]);
        server.cli(&["set-option", "-g", "window-size", "latest"]);
        server
    }

    /// One spawn-per-command invocation, stdout UNTRIMMED — the transport
    /// this change replaces, and therefore the byte-exact reference answer.
    fn cli_raw(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The same, trimmed, for fixtures that only care about the value.
    fn cli(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("tmux runs");
        String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
    }

    /// Stand a session up on this socket, and do not return until tmux AGREES
    /// it is there.
    ///
    /// # Why the COMMAND is retried and not merely the probe
    ///
    /// This is the same rule `tmux::test_support::start_session` states, and
    /// it is here for the same reason: no amount of waiting produces a session
    /// nobody successfully asked for.
    ///
    /// Two earlier shapes both went red in CI and both were about the same
    /// millisecond. Reading nothing at all let a failed `new-session` pass
    /// silently and the wait after it took the blame. Reading the STATUS and
    /// demanding first-time success — the shape this replaces — panicked with
    /// `server exited unexpectedly`, because [`Self::kill_server_and_wait`]
    /// waits for `list-sessions` to fail and the old server stops ANSWERING
    /// before it unlinks its socket. In that window a client still connects,
    /// and is then dropped as the server finishes dying. Both are the fixture's
    /// own race with its own teardown, and neither is a finding about the
    /// transport under test.
    ///
    /// # Panics
    /// With `setup failed: …`, which can never be read as a finding about the
    /// reconnect rule.
    fn mint_session(&self, args: &[&str]) {
        let name = args.iter().position(|arg| *arg == "-s").and_then(|at| args.get(at + 1));
        let name = name.expect("a minted session is named with -s");
        for _ in 0..50 {
            let present = std::process::Command::new("tmux")
                .args(["-L", &self.socket, "has-session", "-t", name])
                .output()
                .expect("tmux runs")
                .status
                .success();
            if present {
                return;
            }
            let _ = std::process::Command::new("tmux")
                .arg("-L")
                .arg(&self.socket)
                .args(args)
                .output()
                .expect("tmux runs");
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("setup failed: tmux never reported session {name:?} on socket {:?}", self.socket);
    }

    /// Kill this server and WAIT until it is really gone.
    ///
    /// `kill-server` returns when the tmux CLIENT exits, which is not when the
    /// SERVER has finished dying. For those milliseconds the socket is still
    /// there and still accepting, so a `new-session` issued into that window
    /// connects to the dying server and FAILS instead of standing a new one up
    /// — reproduced locally at 3 runs in 30, and the cause of this test's 28%
    /// failure rate in CI.
    ///
    /// `list-sessions` is the probe because it is the one that CANNOT paper
    /// over the answer: tmux starts a server for `new-session` and friends, but
    /// a query against a dead socket simply fails. So asking cannot change what
    /// it measures.
    ///
    /// What it measures is NOT "the socket is gone", and the difference cost a
    /// CI red: a server stops ANSWERING before it unlinks its socket, so a
    /// mint issued the instant this returns can still connect to the dying
    /// server and be dropped with `server exited unexpectedly`. Closing that
    /// window by probing the socket FILE instead would trade one tmux
    /// implementation detail for another, so the mint owns it —
    /// [`Self::mint_session`] retries until tmux names the session.
    fn kill_server_and_wait(&self) {
        self.cli(&["kill-server"]);
        let gone = std::time::Instant::now();
        while std::process::Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("list-sessions")
            .output()
            .expect("tmux runs")
            .status
            .success()
        {
            assert!(
                gone.elapsed() < std::time::Duration::from_secs(10),
                "the server did not die; this is a broken fixture, not a reconnect failure"
            );
            #[allow(clippy::disallowed_methods)]
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Every window's geometry, as one comparable string.
    fn geometry(&self) -> String {
        self.cli(&["list-windows", "-a", "-F", "#{window_index}=#{window_width}x#{window_height}"])
    }

    fn transport(&self) -> ControlTransport {
        ControlTransport::new(Socket(self.socket.clone()), "org-test_".to_owned())
    }
}

impl Drop for Server {
    /// Teardown that cannot be skipped by a panicking assertion, which is why
    /// it is `Drop` and not a trailing line in each test.
    ///
    /// Removing a socket this test itself created — the same sanctioned use
    /// of the seam-disallowed writer as `sidebar/tests.rs`'s `LiveServer`.
    /// The path is ASKED FOR rather than assembled: these servers are named
    /// with `-L`, whose file lands under `$TMUX_TMPDIR`-or-`/tmp` plus a
    /// uid-stamped directory, and guessing that layout is how a teardown
    /// silently stops working on the other platform.
    #[allow(clippy::disallowed_methods)]
    fn drop(&mut self) {
        let path = self.cli(&["display-message", "-p", "#{socket_path}"]);
        let _ = std::process::Command::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
        // And the socket FILE, which tmux does not always take with the
        // server. Cosmetic on its own; it stops /tmp filling with one dead
        // entry per test run, which is what makes a REAL leak hard to see.
        if !path.is_empty() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn cmd(argv: &[&str]) -> TmuxCmd {
    TmuxCmd { argv: argv.iter().map(|a| (*a).to_owned()).collect() }
}

#[test]
fn attaching_a_control_client_does_not_change_any_window_geometry() {
    // THE hazard. tmux sizes windows to fit its clients, and a control client
    // is a real client — so a transport that reported a size would shrink the
    // operator's windows, which is indistinguishable from the sidebar
    // geometry bugs and would be blamed on them.
    let server = Server::start("neutral", 200, 50);
    server.cli(&["new-window", "-t", "org-test_"]);
    let before = server.geometry();
    assert!(before.contains("200x50"), "the fixture is the size it claims: {before}");

    let transport = server.transport();
    for _ in 0..5 {
        let out = TmuxRunner::run(
            &transport,
            &Socket(server.socket.clone()),
            &cmd(&["list-windows", "-a", "-F", "#{window_id}"]),
        )
        .expect("the transport answers");
        assert_eq!(out.status, 0);
    }
    assert!(
        transport.is_live(),
        "the control client really attached — otherwise this proves nothing about control mode"
    );

    assert_eq!(server.geometry(), before, "a live control client changed a window's geometry");
    drop(transport);
    assert_eq!(server.geometry(), before, "and detaching changed nothing back");
}

#[test]
fn a_window_created_through_the_transport_is_the_session_size() {
    // The actuator's commonest write is minting a window or a pane, and it
    // issues them THROUGH the control client. A window born while a sized
    // client is the most recent one would be born at that client's size —
    // the same hazard as the resize, arriving by creation instead. It must
    // come out the size every other window is.
    let server = Server::start("mint", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    let out = TmuxRunner::run(
        &transport,
        &socket,
        &cmd(&[
            "new-window",
            "-d",
            "-t",
            "org-test_",
            "-P",
            "-F",
            "#{window_width}x#{window_height}",
        ]),
    )
    .expect("the transport answers");
    assert_eq!(out.status, 0);
    assert!(transport.is_live(), "minted over the control client, not a spawn");
    assert_eq!(
        out.stdout.trim(),
        "200x50",
        "a window minted through the control client is the SESSION's size"
    );
    for window in server.geometry().lines() {
        assert!(window.ends_with("200x50"), "every window is still the session size: {window}");
    }
}

#[test]
fn the_geometry_probe_can_actually_see_a_resize() {
    // The negative control, and it is not optional: without it the test above
    // could pass because the probe is blind rather than because the transport
    // is neutral. A control client that DECLARES a size is the exact thing
    // `ControlTransport` must never become, so this pins that the difference
    // is observable at the same seam.
    let server = Server::start("visible", 200, 50);
    let before = server.geometry();

    let mut client = ControlClient::connect("tmux", &Socket(server.socket.clone()), "org-test_")
        .expect("control mode attaches");
    client
        .run(&quote_argv(&["refresh-client", "-C", "80x24"]).expect("representable"))
        .expect("the declaration is accepted");

    assert_ne!(
        server.geometry(),
        before,
        "declaring a size MUST move the geometry — if it does not, the neutrality test proves nothing"
    );
    assert!(server.geometry().contains("80x24"), "and it moves it to the size the client declared");
}

#[test]
fn the_transport_never_declares_a_size() {
    // The structural half of the guarantee. Every resize in the measured
    // table is a row where the client declared a size, so the rule is that
    // this module contains no `refresh-client` at all.
    // Prose is exempt: the module doc names `refresh-client` repeatedly, to
    // explain why it is absent. The rule is about CODE.
    let code: String = include_str!("../control.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("refresh-client"),
        "the transport must never declare a size; that is what makes it neutral"
    );
    assert!(
        code.contains("ignore-size"),
        "and it attaches with ignore-size, which covers the operator-attached case"
    );
}

#[test]
fn the_control_transport_answers_exactly_what_a_spawn_answers() {
    // The equivalence that lets this land behind the seams without touching a
    // caller: for the same command, the two transports must be
    // indistinguishable — including for arguments tmux's lexer could eat.
    let server = Server::start("same", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    for hostile in ["plain", "hello world", "#{pane_id}", "a;b", "a$b", "it's", "a\"b", "-dash"] {
        server.cli(&["set-option", "-g", "@probe", hostile]);
        let spawned = server.cli_raw(&["show-options", "-v", "-g", "@probe"]);
        server.cli(&["set-option", "-g", "@probe", "CLEARED"]);

        let set =
            TmuxRunner::run(&transport, &socket, &cmd(&["set-option", "-g", "@probe", hostile]))
                .expect("the transport answers");
        assert_eq!(set.status, 0, "setting '{hostile}' over control mode");
        let read =
            TmuxRunner::run(&transport, &socket, &cmd(&["show-options", "-v", "-g", "@probe"]))
                .expect("the transport answers");

        assert_eq!(
            read.stdout, spawned,
            "'{hostile}' must survive the control-mode lexer byte for byte, \
             trailing newline included"
        );
    }
}

#[test]
fn tmux_saying_no_is_a_nonzero_status_over_the_control_client() {
    // The live half of the %error rule: a refusal must look like a refusal,
    // not like a transport that failed to answer.
    let server = Server::start("refusal", 200, 50);
    let transport = server.transport();
    let out = TmuxRunner::run(
        &transport,
        &Socket(server.socket.clone()),
        &cmd(&["kill-session", "-t", "no-such-session"]),
    )
    .expect("the TRANSPORT succeeded; the COMMAND did not");

    assert_ne!(out.status, 0, "tmux refused, so the status is non-zero");
    assert!(
        out.stderr.contains("no-such-session") || out.stderr.contains("find session"),
        "and it says why: {}",
        out.stderr
    );
}

#[test]
fn a_lost_server_reconnects_on_the_next_call() {
    // A control client dies with its server. The next call must reconnect
    // rather than fail forever — the difference between a restarted tmux
    // costing one slow command and costing every command after it.
    let server = Server::start("reconnect", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    let first =
        TmuxRunner::run(&transport, &socket, &cmd(&["list-sessions", "-F", "#{session_name}"]))
            .expect("the first call answers");
    assert_eq!(first.stdout, "org-test_\n");
    assert!(transport.is_live(), "a control client is attached");

    // Kill the server out from under it and stand a new one up on the same
    // socket, exactly as a tmux restart would.
    //
    // THE DEATH IS WAITED FOR, AND THE BIRTH IS RETRIED. Both halves, because
    // this test has now been fixed twice at the wrong end of the same
    // millisecond.
    //
    // `kill-server` returns when the CLIENT exits, not when the SERVER has
    // finished dying, and for those milliseconds the socket is still there and
    // still accepting. A `new-session` issued into that window connects to the
    // dying server and FAILS — measured at 3 runs in 30 locally, and 28% of
    // runs of this test. The first fix read nothing, so no replacement was ever
    // created and the readiness loop waited ten seconds for a session that did
    // not exist. The second read the STATUS and demanded first-time success,
    // which turned the same lost race into `the fixture's own tmux new-session
    // … failed: server exited unexpectedly` — an honest message about a race
    // the fixture should simply not lose. Retrying the COMMAND is what does
    // not lose it.
    //
    // The RULE — that a lost client reconnects on the next call rather than
    // failing forever — is unchanged and is what the assertions below check.
    // Neither of these lines is a wait FOR the reconnect; they are the fixture
    // making sure there is a server to reconnect TO.
    server.kill_server_and_wait();
    server.mint_session(&["new-session", "-d", "-s", "org-test_", "-x", "200", "-y", "50"]);

    let after =
        TmuxRunner::run(&transport, &socket, &cmd(&["list-sessions", "-F", "#{session_name}"]))
            .expect("the next call reconnects and answers");
    assert_eq!(after.stdout, "org-test_\n", "the reconnected client answers the same question");
}

#[test]
fn a_command_for_another_socket_is_not_sent_to_this_client() {
    // The transport holds ONE client, for one socket. A command aimed
    // somewhere else must go there, not to the attached server — sending it
    // here would answer a question about the wrong company.
    let here = Server::start("here", 200, 50);
    let elsewhere = Server::start("elsewhere", 100, 30);
    let transport = here.transport();

    // Warm the client so the routing decision is made against a LIVE one.
    TmuxRunner::run(
        &transport,
        &Socket(here.socket.clone()),
        &cmd(&["list-sessions", "-F", "#{session_name}"]),
    )
    .expect("answers");
    assert!(transport.is_live());

    elsewhere.cli(&["set-option", "-g", "@who", "elsewhere"]);
    let out = TmuxRunner::run(
        &transport,
        &Socket(elsewhere.socket.clone()),
        &cmd(&["show-options", "-v", "-g", "@who"]),
    )
    .expect("answers");
    assert_eq!(out.stdout, "elsewhere\n", "the other socket answered, via a spawn");
}

#[test]
fn a_newline_argument_still_reaches_tmux_by_spawning() {
    // The refusal in `quote_argv` is a routing decision, not a failure: the
    // command must still run, and still carry its argument.
    let server = Server::start("newline", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    let out =
        TmuxRunner::run(&transport, &socket, &cmd(&["set-option", "-g", "@multi", "one\ntwo"]))
            .expect("the spawn path carries it");
    assert_eq!(out.status, 0);
    assert_eq!(
        server.cli(&["show-options", "-v", "-g", "@multi"]),
        "one\ntwo",
        "both lines arrived, so nothing was truncated"
    );
}

#[test]
fn the_rail_seam_gets_stdout_and_empties_on_refusal() {
    // The sidebar's contract: stdout on success, EMPTY on failure, because a
    // stale verb is a no-op the rail survives rather than an error it
    // reports. The transport must make the same collapse the spawn
    // implementation makes.
    let server = Server::start("rail", 200, 50);
    let transport = server.transport();

    assert_eq!(
        crate::sidebar::Tmux::run(&transport, &["list-sessions", "-F", "#{session_name}"]),
        "org-test_",
        "a good command answers with its stdout"
    );
    assert_eq!(
        crate::sidebar::Tmux::run(&transport, &["kill-session", "-t", "no-such-session"]),
        "",
        "a refused command is empty, never an error the rail has to handle"
    );
}

#[test]
fn a_missing_session_falls_back_to_spawning_and_the_commands_still_work() {
    // There is no session to attach to here, so control mode cannot be
    // established — and every command must still work, through the spawn path,
    // with identical answers.
    let server = Server::start("nosession", 200, 50);
    let transport = ControlTransport::new(Socket(server.socket.clone()), "org-absent_".to_owned());
    let socket = Socket(server.socket.clone());

    let out =
        TmuxRunner::run(&transport, &socket, &cmd(&["list-sessions", "-F", "#{session_name}"]))
            .expect("the spawn path answers");
    assert_eq!(out.stdout, "org-test_\n", "the command ran, just not over control mode");
    assert!(!transport.is_live(), "no control client was established");

    let again =
        TmuxRunner::run(&transport, &socket, &cmd(&["list-sessions", "-F", "#{session_name}"]))
            .expect("and keeps answering");
    assert_eq!(again.stdout, "org-test_\n");
    assert!(!transport.is_live());
}

/// THE RULE: A SESSION THAT IS NOT THERE YET IS A RACE, NOT A CAPABILITY
/// ANSWER — so the transport asks again in milliseconds, not in half a minute.
///
/// # The thirty seconds this pins, measured on the operator's own box
///
/// 2026-08-16. `chief actuate` starts its converge loop and its session brain
/// while `chief attach` is still minting the tmux session — `actuator.start`
/// at 05:07:22.005, `company.session.present` at 05:07:22.115. Both of that
/// process's transports issued their first tmux verb inside those 110ms, both
/// attaches answered `can't find session`, and both were written off as an
/// answer about the HOST. Every tmux verb in the company then ran as a ~25ms
/// process spawn until `tmux.control.regained` at 05:07:52.021.
///
/// Every click the operator made landed inside that window. A first department
/// click took 483ms; the same gesture at 05:07:55, seconds after the transport
/// came back, took **25.6ms**. There was no other cause for "a lot of
/// flashing" — this was the whole of it.
///
/// The assertion is deliberately made through the PUBLIC seam (`is_live` after
/// an ordinary verb) rather than by reading the slot, because what broke the
/// product is what the next VERB got, not what the enum said.
#[test]
fn a_session_minted_after_the_first_attach_is_picked_up_within_one_frame() {
    let server = Server::start("racedsession", 200, 50);
    // The company's own session is not there yet. This is exactly the state
    // `chief actuate` finds when it starts beside `chief attach`.
    let transport = ControlTransport::new(Socket(server.socket.clone()), "org-later_".to_owned());
    let socket = Socket(server.socket.clone());
    let probe = cmd(&["list-sessions", "-F", "#{session_name}"]);

    TmuxRunner::run(&transport, &socket, &probe).expect("the spawn path answers meanwhile");
    assert!(!transport.is_live(), "there was nothing to attach to, so the verb spawned");

    // `chief attach` finishes minting it.
    server.mint_session(&["new-session", "-d", "-s", "org-later_"]);
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(ABSENT_RETRY);

    TmuxRunner::run(&transport, &socket, &probe).expect("and answers again");
    assert!(
        transport.is_live(),
        "the session exists now, so the next verb {}ms later is back on the fast path — this \
         used to be written off for {}s, and every click inside that window cost the operator \
         hundreds of milliseconds",
        ABSENT_RETRY.as_millis(),
        CAPABILITY_RETRY.as_secs()
    );
}

/// AND THE CLASSIFICATION ITSELF, as a unit — the fast failure that names the
/// cause when the test above regresses.
///
/// Both phrasings were probed against real binaries on 2026-08-16, tmux 3.3a
/// and tmux 3.7b, and both answered character for character the same: a server
/// that is up but has no session by that name says `can't find session: <name>`,
/// and a socket with no server at all says `no sessions`.
#[test]
fn tmux_saying_there_is_no_session_is_never_read_as_a_host_that_cannot_do_control_mode() {
    assert!(session_absent("can't find session: org-tribes-capital_"));
    assert!(session_absent("no sessions"));
    // And the answers that really ARE about this host, which must keep the
    // long sentence rather than being retried every frame for ever.
    assert!(!session_absent("unknown flag -f"), "a tmux too old for CLIENT_FLAGS is a host fact");
    assert!(
        !session_absent("tmux control mode did not attach to 'org-x_' within 10000ms"),
        "a tmux that never answered told us nothing about the session"
    );
}

/// A `;` IS A SEPARATOR, NOT AN ARGUMENT — AND A BATCH ANSWERS ONCE PER COMMAND.
///
/// # The outage this pins
///
/// Every argument was quoted, including the bare `;` this repo passes as its own
/// argv element to batch a sequence. This repo batches EVERYWHERE and on
/// purpose: tmux renders once per command sequence, so separate invocations are
/// separate frames, and collapsing them is how the sidebar's flicker was fixed.
///
/// Quoted, tmux reads the separator as an argument. Measured against a real
/// tmux: `parse error: command display-message: too many arguments`, then
/// `%error`, then `%exit`. So every batched command failed, the client died, the
/// next call reconnected and failed identically, and every read answered EMPTY.
///
/// An empty answer is a legitimate "not found" everywhere in this code. So
/// `department_window` concluded the engineering window did not exist and minted
/// one — whose rail asked the same question and minted another. Windows
/// @64..@69 inside two seconds on the operator's live company, until the tmux
/// server was gone.
///
/// The second half is the reply COUNT: tmux answers one block per command, so a
/// batched line yields N and a reader taking one leaves N-1 behind to become the
/// answers to later calls. Both halves are pinned here, against a real server,
/// because a unit test over the demux alone would have missed both.
#[test]
fn a_batched_sequence_is_parsed_as_commands_and_read_to_the_end() {
    let server = Server::start("batched", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    // Exactly the shape the rail and the actuator issue: several commands in one
    // sequence, separated by a bare `;`.
    let batched = cmd(&[
        "display-message",
        "-p",
        "-t",
        "org-test_",
        "-F",
        "#{session_name}",
        ";",
        "display-message",
        "-p",
        "-t",
        "org-test_",
        "-F",
        "#{window_width}",
    ]);
    let out = TmuxRunner::run(&transport, &socket, &batched).expect("the batch answers");

    assert_eq!(out.status, 0, "a batch is not an error: {out:?}");
    assert!(
        out.stdout.contains("org-test_"),
        "the FIRST command's output is there: {:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains("200"),
        "and the SECOND's — a reader that stopped at one block would have left this in the \
         channel to corrupt the next call: {:?}",
        out.stdout
    );
    assert!(transport.is_live(), "and the client survived — a quoted `;` used to %exit it");

    // The very next call must be answered correctly, which is what proves no
    // surplus block was left behind.
    let after = TmuxRunner::run(
        &transport,
        &socket,
        &cmd(&["display-message", "-p", "-t", "org-test_", "-F", "#{session_name}"]),
    )
    .expect("the next call answers");
    assert_eq!(after.stdout.trim(), "org-test_", "no reply shifted onto this call: {after:?}");
}

/// AND THE QUOTING ITSELF, as a unit: a separator survives bare, everything else
/// is quoted. The end-to-end test above is the proof; this is the fast failure
/// that names the cause when it regresses.
#[test]
fn a_separator_is_never_quoted_and_is_counted_as_another_block() {
    let line =
        quote_argv(&["set-option", "-p", "-t", "%1", "@a", "b", ";", "kill-pane", "-t", "%2"])
            .expect("representable");
    assert!(
        line.text.contains(" ; "),
        "the separator is BARE — quoted, tmux reads it as an argument and refuses the whole \
         command: {:?}",
        line.text
    );
    assert!(!line.text.contains("';'"), "never quoted: {:?}", line.text);
    assert_eq!(line.blocks, 2, "two commands, so two reply blocks to read");
    assert_eq!(
        quote_argv(&["list-panes"]).expect("representable").blocks,
        1,
        "and an ordinary command answers once"
    );
}

/// A FAILED BATCH ANSWERS AT ONCE, because tmux aborts the sequence there.
///
/// # The ten-second stall this prevents
///
/// A batched line normally answers with one block per command, and `run` reads
/// exactly that many. But tmux ABORTS a sequence at its first failure and sends
/// no block for the commands after it — so a reader still counting would wait
/// out `REPLY_TIMEOUT`, ten seconds, before failing a call that tmux had already
/// answered.
///
/// That is not a rare path. Killing a pane that has already gone is something
/// this codebase does routinely — the placeholder sweeps and the layout batches
/// both do it — so this would have put a ten-second stall on the click path.
///
/// Both facts were measured against a real tmux with stdin held open. (An
/// earlier probe closed stdin and made this look like the client dying; it does
/// not — it stays live and answers the next call normally, which is the second
/// half of what this pins.) It is also exactly what the spawn path does: `tmux a
/// ; b` with `a` failing exits 1 and never runs `b`.
#[test]
fn a_batch_that_fails_part_way_answers_immediately_and_keeps_the_client() {
    let server = Server::start("batch-fail", 200, 50);
    let transport = server.transport();
    let socket = Socket(server.socket.clone());

    let started = std::time::Instant::now();
    let out = TmuxRunner::run(
        &transport,
        &socket,
        // The first command cannot succeed; the second would have, and tmux
        // never reaches it.
        &cmd(&[
            "kill-pane",
            "-t",
            "%9999",
            ";",
            "display-message",
            "-p",
            "-t",
            "org-test_",
            "-F",
            "#{session_name}",
        ]),
    )
    .expect("the transport answers rather than erroring");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "it answered at once — counting the missing block would have cost the full \
         REPLY_TIMEOUT: {:?}",
        started.elapsed()
    );
    assert_eq!(out.status, 1, "tmux said no, and that is a non-zero status not a transport error");
    assert!(out.stderr.contains("can't find pane"), "with tmux's own words: {:?}", out.stderr);

    // The client survives a refused command, and the next call is unshifted.
    assert!(transport.is_live(), "a refusal is not a lost client");
    let after = TmuxRunner::run(
        &transport,
        &socket,
        &cmd(&["display-message", "-p", "-t", "org-test_", "-F", "#{session_name}"]),
    )
    .expect("the next call answers");
    assert_eq!(after.stdout.trim(), "org-test_", "and answers ITS OWN question: {after:?}");
}

/// THE RULE: a spawn that was CHOSEN is silent; a spawn that was FORCED says
/// so.
///
/// The operator asked why tmux felt slow, and nothing in the record could
/// answer them, because `over_control` returned a bare `None` for seven
/// different conditions. Three are ordinary routing and three are a
/// degradation to ~25ms-per-command process spawns — spelled identically, so a
/// transport that had lost the fast path looked exactly like one that never
/// needed it.
///
/// The classes are asserted here rather than the log line, because the log
/// line is downstream of this decision and would pass for the wrong reason if
/// the classification were wrong.
#[test]
fn a_spawn_that_was_chosen_is_silent_and_a_spawn_that_was_forced_is_not() {
    let transport = ControlTransport::new(Socket("no-such-socket".to_owned()), "s".to_owned());

    // ROUTING. A verb for another server has no client here and never did.
    let elsewhere = transport
        .over_control(&Socket("somewhere-else".to_owned()), &cmd(&["list-panes"]))
        .expect_err("a command for another socket cannot go over this client");
    assert!(
        matches!(elsewhere, Spawn::AnotherServer),
        "a command aimed at a different tmux server is routing, not degradation: {elsewhere:?}"
    );

    // REPRESENTABILITY. A newline terminates a control line, so half the
    // command would reach tmux; the spawn path carries it whole.
    let unrepresentable = transport
        .over_control(&transport.socket, &cmd(&["send-keys", "line1\nline2"]))
        .expect_err("a newline cannot be carried on a control line");
    assert!(
        matches!(unrepresentable, Spawn::NotRepresentable),
        "a limit of the wire format is not a lost capability: {unrepresentable:?}"
    );

    let nested = transport
        .over_control(
            &transport.socket,
            &cmd(&["if-shell", "-F", "1", "display-message -p accepted", ""]),
        )
        .expect_err("nested command output cannot belong to the outer control block");
    assert!(matches!(nested, Spawn::NotRepresentable));
    assert!(!if_shell_has_command_branch(&[
        "if-shell".to_owned(),
        "-F".to_owned(),
        "0".to_owned(),
        String::new(),
        String::new(),
    ]));
}

/// THE RULE: losing the fast path is reported ONCE, not once per command.
///
/// A rail asks tmux several times per gesture. A line per command would bury
/// the one event worth finding under the noise it generates, so the edge is
/// what speaks and the steady state is quiet.
#[test]
fn a_lost_control_client_is_reported_on_the_edge_and_then_stays_quiet() {
    let transport = ControlTransport::new(Socket("no-such-socket".to_owned()), "s".to_owned());
    let probe = cmd(&["list-panes"]);

    let first = transport
        .over_control(&transport.socket, &probe)
        .expect_err("there is no server on this socket, so no client can attach");
    assert!(
        matches!(first, Spawn::Degraded(_)),
        "the FIRST failure is the edge, and it carries the reason: {first:?}"
    );

    for _ in 0..3 {
        let again = transport.over_control(&transport.socket, &probe).expect_err("still no server");
        assert!(
            matches!(again, Spawn::Silent),
            "every later call is the same fact already reported, and must not repeat it: \
             {again:?}"
        );
    }
}

/// THE RULE: a capability lost to a race is regained without a restart, and
/// THE WAIT IS CHOSEN BY THE REASON.
///
/// `Slot::Unsupported` used to latch on the FIRST attempt and never retry. The
/// first attempt is also the one most likely to land in a bad moment — a rail
/// asks tmux milliseconds after `split-window` mints its pane — so a race
/// there made a long-lived process spawn every command for the rest of its
/// life. Making that thirty seconds was strictly better and still far too long
/// for the race that actually fires, so the two answers are now separate
/// states with separate waits. Both arms are asserted here, because a
/// transport that gave EVERYTHING the short wait would hammer a host that
/// genuinely cannot do control mode with a doomed attach every 50ms for ever.
#[test]
fn a_missing_session_and_a_host_that_cannot_do_control_mode_get_different_waits() {
    let probe = cmd(&["list-panes"]);

    // NO SESSION. tmux speaks control mode perfectly well and tells us there
    // is nothing to attach to, which says nothing about this host.
    let absent = ControlTransport::new(Socket("no-such-socket".to_owned()), "s".to_owned());
    let _ = absent.over_control(&absent.socket, &probe);
    let slot = absent.slot.lock().expect("the slot is not poisoned");
    let Slot::Absent { since } = &*slot else {
        panic!(
            "a session that is not there is a RACE, and must not be given the long wait: {slot:?}"
        );
    };
    assert!(
        since.elapsed() < ABSENT_RETRY,
        "the retry clock starts at the failure, and it is {}ms — not the {}s a capability \
         answer earns",
        ABSENT_RETRY.as_millis(),
        CAPABILITY_RETRY.as_secs()
    );
    drop(slot);

    // NO TMUX. Nothing ever spoke control mode here, so this IS the capability
    // answer and it keeps the long wait.
    let unusable = ControlTransport::new(Socket("no-such-socket".to_owned()), "s".to_owned())
        .with_binary("chief-has-no-such-tmux");
    let _ = unusable.over_control(&unusable.socket, &probe);
    let slot = unusable.slot.lock().expect("the slot is not poisoned");
    let Slot::Unsupported { since } = &*slot else {
        panic!("a host with no tmux at all cannot be asked again every frame: {slot:?}");
    };
    assert!(
        since.elapsed() < CAPABILITY_RETRY,
        "and even that is never for the life of the process — it is {}s",
        CAPABILITY_RETRY.as_secs()
    );
}
