//! BEFORE/AFTER for the tmux transport: spawn-per-command vs control mode.
//!
//! Not a test — it measures, and a timing assertion in CI is a flake waiting
//! to happen. Run it when the number matters:
//!
//! ```text
//! cargo run -p chief-cli --example tmux_transport_bench -- [iterations]
//! ```
//!
//! It stands up its own tmux server on a private socket, runs the SAME
//! command list through both transports, and prints the per-command cost of
//! each. The command list is the shape the rail and the actuator actually
//! issue: a read, a format read, and an option write.

use std::time::Instant;

use chief_cli::actuate::host::{Socket, TmuxCmd};
use chief_cli::actuate::runner::{SystemTmuxRunner, TmuxRunner};
use chief_cli::control::ControlTransport;

/// Fixture setup and teardown, through the spawn runner the benchmark is
/// measuring AGAINST.
///
/// Deliberately not a bare `Command::new("tmux")`: the product has exactly one
/// place that resolves the tmux binary for this crate, and a benchmark is not
/// a reason to open a second one — `scripts/spawn-program-absolute-lib.mjs`
/// registers that single site and would have to register this too.
fn cli(socket: &str, args: &[&str]) -> String {
    let argv = args.iter().map(|a| (*a).to_owned()).collect();
    SystemTmuxRunner::default()
        .run(&Socket(socket.to_owned()), &TmuxCmd { argv })
        .map(|out| out.stdout)
        .unwrap_or_default()
}

fn main() {
    let iterations: u32 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(200);
    let socket = format!("chief-bench-{}", std::process::id());
    let session = "org-bench_".to_owned();

    cli(&socket, &["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"]);

    // The verbs the product really issues, not a synthetic no-op.
    let commands: Vec<TmuxCmd> = vec![
        TmuxCmd { argv: ["list-panes", "-a", "-F", "#{pane_id}"].map(str::to_owned).to_vec() },
        TmuxCmd {
            argv: ["display-message", "-p", "-t", &session, "#{session_name}"]
                .map(str::to_owned)
                .to_vec(),
        },
        TmuxCmd { argv: ["set-option", "-g", "@bench", "value"].map(str::to_owned).to_vec() },
    ];

    let sock = Socket(socket.clone());

    let spawn = SystemTmuxRunner::default();
    // One warm call each, so neither number pays a first-time cost the other
    // does not.
    let _ = spawn.run(&sock, &commands[0]);
    let started = Instant::now();
    let mut spawn_calls = 0_u32;
    for _ in 0..iterations {
        for cmd in &commands {
            let _ = spawn.run(&sock, cmd);
            spawn_calls += 1;
        }
    }
    let spawn_elapsed = started.elapsed();

    let control = ControlTransport::new(sock.clone(), session.clone());
    let _ = TmuxRunner::run(&control, &sock, &commands[0]);
    assert!(control.is_live(), "control mode did not attach; the comparison would be a lie");
    let started = Instant::now();
    let mut control_calls = 0_u32;
    for _ in 0..iterations {
        for cmd in &commands {
            let _ = TmuxRunner::run(&control, &sock, cmd);
            control_calls += 1;
        }
    }
    let control_elapsed = started.elapsed();

    // The server dies here; its socket FILE does not always go with it, and
    // this leaves it. The live-tmux TEST fixture does remove it, because a run
    // of the suite is frequent and automatic, and litter there is what hides a
    // real leak. Doing the same here would mean a `clippy::disallowed_methods`
    // exemption for `std::fs::remove_file` — and that allowlist is a
    // deliberately narrow, greppable boundary (filesystem effects belong to
    // the host executor). A hand-run benchmark's one temp file is not worth
    // widening it. Remove it yourself if it bothers you.
    cli(&socket, &["kill-server"]);

    let spawn_each = spawn_elapsed.as_secs_f64() * 1000.0 / f64::from(spawn_calls);
    let control_each = control_elapsed.as_secs_f64() * 1000.0 / f64::from(control_calls);

    println!("tmux transport, {spawn_calls} commands each");
    println!(
        "  spawn-per-command  {:>8.1} ms total  {spawn_each:>7.3} ms/command",
        spawn_elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "  control mode       {:>8.1} ms total  {control_each:>7.3} ms/command",
        control_elapsed.as_secs_f64() * 1000.0
    );
    println!("  speedup            {:>8.1}x", spawn_each / control_each);
}
