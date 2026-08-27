//! Real tmux proof for the interactive sleeping-person card.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::process::Command;
use std::time::{Duration, Instant};

use chief_cli::actuate::host::Socket;
use chief_cli::actuate::interpret::apply_plan;
use chief_cli::actuate::plan::{ObservedTopology, ObservedWindow, Step};
use chief_cli::actuate::runner::ThreadWaiter;
use chief_cli::actuate::spawn_cmd::LaunchSpec;
use chief_cli::actuate::{EverObserved, TmuxHost};
use chief_cli::control::ControlTransport;
use chief_cli::placement::Topology;
use chief_cli::proc::ProcReader;
use chief_cli::real::RealHostExecutor;
use chief_cli::sidebar::wire::{Frames, ToBrain, ToClient};

struct TmuxServer(String);

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-L", &self.0, "kill-server"]).output();
    }
}

fn tmux(socket: &str, args: &[&str]) -> String {
    let output = match Command::new("tmux").args(["-L", socket]).args(args).output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("tmux could not run: {error}");
            std::process::abort();
        }
    };
    assert!(output.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn tmux_raw(socket: &str, args: &[&str]) -> String {
    let output = match Command::new("tmux").args(["-L", socket]).args(args).output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("tmux {args:?}: {error}");
            std::process::abort();
        }
    };
    assert!(output.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn wait_for(socket: &str, pane: &str, needle: &str) -> String {
    let started = Instant::now();
    loop {
        let frame = tmux(socket, &["capture-pane", "-p", "-t", pane]);
        if frame.contains(needle) {
            return frame;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "missing {needle:?}: {frame:?}; pane={}",
            tmux(
                socket,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "dead=#{pane_dead} status=#{pane_dead_status} command=#{pane_current_command} pid=#{pane_pid} size=#{pane_width}x#{pane_height} cursor=#{cursor_x},#{cursor_y} alternate=#{alternate_on} mouse=#{mouse_any_flag}"
                ]
            )
        );
        std::thread::yield_now();
    }
}

#[test]
fn actual_card_accepts_an_xterm_sgr_click_through_an_attached_tmux_client() {
    let Some(binary) = option_env!("CARGO_BIN_EXE_chief") else { return };
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let root = tempfile::tempdir().unwrap_or_else(|error| panic!("company: {error}"));
    std::fs::create_dir_all(root.path().join(".chief/run"))
        .unwrap_or_else(|error| panic!("runtime directory: {error}"));
    let socket_path = root.path().join(".chief/run/rail.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)
        .unwrap_or_else(|error| panic!("brain socket: {error}"));
    let socket = format!("chief-card-{}", uuid::Uuid::new_v4());
    let _ = Command::new("tmux").args(["-L", &socket, "kill-server"]).output();
    tmux(&socket, &["new-session", "-d", "-s", "card", "-x", "80", "-y", "24"]);
    let _server_guard = TmuxServer(socket.clone());
    tmux(&socket, &["set-option", "-g", "mouse", "on"]);
    tmux(&socket, &["set-option", "-g", "status", "off"]);
    tmux(&socket, &["set-option", "-g", "pane-border-status", "top"]);
    let pane = tmux(&socket, &["display-message", "-p", "-t", "card", "#{pane_id}"]);
    let window = tmux(&socket, &["display-message", "-p", "-t", "card", "#{window_id}"]);
    let rail = tmux(
        &socket,
        &[
            "split-window",
            "-h",
            "-b",
            "-d",
            "-l",
            "12",
            "-t",
            &pane,
            "-P",
            "-F",
            "#{pane_id}",
            "/bin/sh",
            "-c",
            "printf 'People'; exec sleep 30",
        ],
    );
    tmux(&socket, &["set-option", "-t", "card", "@organization_id", "acme"]);
    tmux(&socket, &["set-option", "-w", "-t", &window, "@organization_id", "acme"]);
    tmux(&socket, &["set-option", "-w", "-t", &window, "@organization_window_id", "__focus__"]);
    tmux(&socket, &["set-option", "-p", "-t", &pane, "@chief_sleeping_person", "nia"]);
    tmux(&socket, &["set-option", "-p", "-t", &rail, "@organization_sidebar", "1"]);
    let mut client = Command::new("script")
        .args(["-q", "-c", &format!("tmux -L {socket} attach-session -t card"), "/dev/null"])
        .env("TERM", "xterm-256color")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("ordinary browser-shaped tmux client in a pty");
    let mut client_input = client.stdin.take().expect("attached client's input");
    let started = Instant::now();
    let client_facts = loop {
        let listed = tmux(&socket, &["list-clients", "-F", "#{client_name}"]);
        if let Some(ordinary) = listed.lines().find(|line| line.starts_with("/dev/")) {
            break ordinary.to_owned();
        }
        assert!(started.elapsed() < Duration::from_secs(2), "ordinary tmux client did not attach");
        std::thread::yield_now();
    };
    assert!(client_facts.starts_with("/dev/"));
    tmux(&socket, &["resize-pane", "-t", &rail, "-x", "12"]);
    let company_dir = root.path().to_string_lossy().into_owned();
    tmux(
        &socket,
        &[
            "respawn-pane",
            "-k",
            "-t",
            &pane,
            "-c",
            &company_dir,
            binary,
            "sleeping-person-card",
            "nia",
            "Nia",
            "Research Lead",
            "selected",
            "zipbox/deepseek",
            "deepseek-v4-flash-0731",
            "",
            "",
        ],
    );
    wait_for(&socket, &pane, "Wake Up");
    let sleeping = tmux_raw(&socket, &["capture-pane", "-p", "-t", &pane]);
    let (button_row, button_column) = sleeping
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("Wake Up").map(|column| (row, column + 2)))
        .expect("the visible Wake Up span has an exact terminal cell");
    let pane_left = tmux(&socket, &["display-message", "-p", "-t", &pane, "#{pane_left}"])
        .parse::<usize>()
        .expect("numeric body offset");
    let pane_top = tmux(&socket, &["display-message", "-p", "-t", &pane, "#{pane_top}"])
        .parse::<usize>()
        .expect("numeric body offset");
    let browser_column = pane_left + button_column;
    let browser_row = pane_top + button_row;
    let control = ControlTransport::new(Socket(socket.clone()), "card".to_owned());
    assert_eq!(
        chief_cli::sidebar::Tmux::run(
            &control,
            &["display-message", "-p", "-t", "card", "#{session_name}"],
        ),
        "card"
    );
    assert!(control.is_live(), "the card authority starts from a live control transport");
    let nested = chief_cli::sidebar::Tmux::run(
        &control,
        &["if-shell", "-F", "1", "display-message -p nested-output", ""],
    );
    assert_eq!(nested, "nested-output", "nested output uses the complete spawn capture");
    assert!(control.is_live(), "routing one nested command does not discard the live client");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap_or_else(|error| panic!("card: {error}"));
        let mut frames = Frames::new();
        let mut bytes = [0_u8; 1024];
        loop {
            let count =
                stream.read(&mut bytes).unwrap_or_else(|error| panic!("wake read: {error}"));
            assert!(count > 0);
            frames.feed(&bytes[..count]);
            if let Some(ToBrain::WakePerson { pane, person, .. }) =
                frames.next_to_brain().unwrap_or_else(|error| panic!("wake frame: {error}"))
            {
                assert!(pane.starts_with('%'));
                assert_eq!(person, "nia");
                assert!(chief_cli::sidebar::authorize_sleeping_card(
                    &control, "card", "acme", &pane, "nia",
                ));
                stream
                    .write_all(&ToClient::WakeAccepted { person }.encode())
                    .unwrap_or_else(|error| panic!("accepted: {error}"));
                return;
            }
        }
    });
    write!(
        client_input,
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        browser_column + 1,
        browser_row + 1,
        browser_column + 1,
        browser_row + 1,
    )
    .expect("xterm SGR left-button press and release");
    client_input.flush().expect("deliver the browser-shaped click");
    wait_for(&socket, &pane, "Waking up");
    server.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    assert_eq!(
        tmux(&socket, &["show-options", "-p", "-v", "-t", &pane, "@chief_waking_person"]),
        "nia"
    );
    assert!(
        !tmux(&socket, &["show-options", "-p", "-v", "-t", &pane, "@chief_wake_claim"]).is_empty(),
        "lost nested output is accepted only through the exact durable claim"
    );
    let card_pid = tmux(&socket, &["display-message", "-p", "-t", &pane, "#{pane_pid}"]);
    let tty = root.path().join("tty.txt");
    let fake_pi = root.path().join("fake-pi");
    chief_cli::files::publish_atomically(
        &fake_pi,
        &format!(
            "#!/bin/sh\nstty -a > '{}'\nprintf 'Nia Pi ready'\nexec sleep 30\n",
            tty.display()
        ),
        0o700,
    )
    .unwrap_or_else(|error| panic!("fake Pi: {error}"));

    // ONE WINDOW PER PERSON, AND THE CARD IS NOT IT.
    //
    // This used to be `Step::ClaimWakingFocus`: converge respawned Pi into the
    // very pane the card was drawn in, so the cell the operator clicked became
    // the person's pane. It worked because the card window was where that
    // person's pane was going to LIVE. It is not: `desired_topology` gives
    // every desired person a window of their own, and a claim that put them
    // here would be a pane converge immediately wanted somewhere else — a move,
    // and therefore a resize, which is the whole defect one window per person
    // deletes.
    let person_window = chief_cli::placement::person_window_id("nia");
    let desired = Topology {
        organization: "acme".to_owned(),
        session: "card".to_owned(),
        windows: vec![chief_cli::placement::Window {
            logical_id: person_window.clone(),
            name: "Nia".to_owned(),
            panes: vec![chief_cli::placement::Pane {
                person_id: "nia".to_owned(),
                launch_hash: "hash-nia".to_owned(),
                order: 0,
            }],
        }],
        known_person_ids: BTreeSet::from(["nia".to_owned()]),
    };
    // The card window as the click left it: railed furniture, and no person.
    let observed = ObservedTopology {
        session_exists: true,
        session_organization: "acme".to_owned(),
        windows: vec![ObservedWindow {
            tmux_id: window.clone(),
            organization_id: "acme".to_owned(),
            logical_id: "__focus__".to_owned(),
            protected_ui: true,
            sleeping_notice: false,
        }],
        panes: Vec::new(),
    };
    let launch = BTreeMap::from([(
        "nia".to_owned(),
        LaunchSpec {
            pi_binary: fake_pi,
            pi_home: root.path().join("pi-home"),
            workspace: root.path().to_path_buf(),
            display_name: "Acme · Research Lead".to_owned(),
            person_name: "Nia".to_owned(),
            accent: None,
            tools: Vec::new(),
            extensions: Vec::new(),
            session: None,
            pending_mail: false,
            env: Vec::new(),
        },
    )]);
    let plan = chief_cli::actuate::plan::compute_converge_plan(&desired, &observed)
        .unwrap_or_else(|error| panic!("the cold-click plan: {error}"));
    assert!(
        plan.steps.iter().any(
            |step| matches!(step, Step::CreateWindowWithSpawn { w, .. } if w.0 == person_window)
        ),
        "the person is minted in a window of their own: {:?}",
        plan.steps
    );
    let control = ControlTransport::new(Socket(socket.clone()), "card".to_owned());
    let actuator =
        RealHostExecutor::new(TmuxHost::new(control, ThreadWaiter), ProcReader::default());
    let applied =
        apply_plan(&actuator, &Socket(socket.clone()), &desired, &observed, &launch, &plan);
    assert!(applied.succeeded(), "the cold-click pass failed: {:?}", applied.failure);

    // THE PERSON IS IN THEIR OWN WINDOW, AT ITS OWN GEOMETRY.
    let minted = tmux(
        &socket,
        &["list-windows", "-t", "card", "-F", "#{window_id}\t#{@organization_window_id}"],
    )
    .lines()
    .filter_map(|line| line.split_once('\t'))
    .find(|(_, tag)| tag.trim() == person_window)
    .map(|(id, _)| id.trim().to_owned())
    .expect("a window tagged for this person");
    assert_ne!(minted, window, "and it is NOT the card window");
    let person_pane = tmux(
        &socket,
        &["list-panes", "-t", &minted, "-F", "#{pane_id}\t#{@organization_person_id}"],
    )
    .lines()
    .filter_map(|line| line.split_once('\t'))
    .find(|(_, person)| person.trim() == "nia")
    .map(|(id, _)| id.trim().to_owned())
    .expect("their pane, tagged for them");
    assert_ne!(person_pane, pane, "the card pane was never claimed as the person");
    wait_for(&socket, &person_pane, "Nia Pi ready");

    // AND THE CARD IS EXACTLY AS THE CLICK LEFT IT — same pane, same process,
    // same waking marker. Converge does not own this window and did not touch
    // it; `brain::finish_pending_zoom` parks it once the person is on the glass.
    assert_eq!(
        tmux(&socket, &["display-message", "-p", "-t", &pane, "#{pane_pid}"]),
        card_pid,
        "the card process is not respawned by the pass that mints the person"
    );
    assert_eq!(
        tmux(&socket, &["show-options", "-p", "-v", "-t", &pane, "@chief_waking_person"]),
        "nia",
        "and its waking marker still names the person it is about"
    );
    assert!(
        tmux(&socket, &["display-message", "-p", "-t", &pane, "#{@organization_person_id}"])
            .is_empty(),
        "the card never becomes person ownership"
    );

    // THE NEXT REAL PASS IS STEADY: no second launch, and no move.
    let observed_again = chief_cli::actuate::observe::observe(
        &actuator,
        &Socket(socket.clone()),
        "card",
        &EverObserved::new(),
    )
    .expect("the minted person is observable");
    let steady = chief_cli::actuate::plan::compute_converge_plan(&desired, &observed_again)
        .unwrap_or_else(|error| {
            panic!("the next real pass is steady: {error}; {observed_again:#?}")
        });
    assert!(
        !steady.steps.iter().any(|step| matches!(
            step,
            Step::CreateWindowWithSpawn { .. }
                | Step::CreateWindowByMove { .. }
                | Step::SplitPane { .. }
                | Step::MovePane { .. }
                | Step::Respawn { .. }
        )),
        "the completed cold click neither launches nor moves anybody again: {:?}",
        steady.steps
    );
    let final_pid = tmux(&socket, &["display-message", "-p", "-t", &person_pane, "#{pane_pid}"]);
    let steady_report =
        apply_plan(&actuator, &Socket(socket.clone()), &desired, &observed_again, &launch, &steady);
    assert!(steady_report.succeeded(), "steady pass failed: {:?}", steady_report.failure);
    assert_eq!(
        tmux(&socket, &["display-message", "-p", "-t", &person_pane, "#{pane_pid}"]),
        final_pid,
        "a second converge pass keeps the final Pi process"
    );
    let tty_state =
        std::fs::read_to_string(&tty).unwrap_or_else(|error| panic!("fake Pi tty state: {error}"));
    let flags = tty_state.split_whitespace().collect::<Vec<_>>();
    assert!(flags.contains(&"icanon") && !flags.contains(&"-icanon"), "{tty_state}");
    assert!(flags.contains(&"echo") && !flags.contains(&"-echo"), "{tty_state}");
    drop(client_input);
    let _ = client.kill();
    let _ = client.wait();
}
