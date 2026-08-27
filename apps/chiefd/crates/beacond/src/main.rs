//! `beacond` binary entry: logging init, config, bind, serve, signals. All
//! real logic lives in the library (`src/lib.rs`) — see its module doc.

use std::sync::Arc;

use beacond::{config, router, shutdown::wait_for_signal, store::Registry, watchdog};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // The console formatter beacond has always had, PLUS the daemon-level file
    // sink `chiefd_log::install` chooses. Both, and the gh#502 ANSI rule, are
    // that call's; the PATH is deliberately not restated here, because beacond
    // is a box-wide service with no company directory to log into and the
    // box-level answer is `chiefd-log`'s to give.
    //
    // beacond's stream matters to a launch specifically: the wait for a
    // company daemon's registration is a wait on THIS program, and it used to
    // be observable only from the waiting side.
    chiefd_log::install("beacond");

    let args: Vec<String> = std::env::args().skip(1).collect();
    match config::classify_args(&args) {
        config::Invocation::Serve => {}
        config::Invocation::Version => {
            println!("beacond {}", env!("CHIEF_VERSION"));
            return std::process::ExitCode::SUCCESS;
        }
        config::Invocation::Help => {
            print!("{}", config::usage());
            return std::process::ExitCode::SUCCESS;
        }
        // Fail closed. This arm used to be "fall through and serve", which
        // meant `beacond --help` bound a port and blocked forever, and any
        // mistyped flag started a daemon the caller never asked for.
        config::Invocation::Unknown(argument) => {
            eprintln!("beacond: unrecognised argument '{argument}'");
            eprint!("{}", config::usage());
            return std::process::ExitCode::FAILURE;
        }
    }

    // Before anything is opened or bound. A beacond spawned by a test fixture
    // must not outlive the runner that owns it, and the cheapest moment to
    // discover the owner is already gone is before this process acquires a
    // database handle and a port. Inert unless BEACOND_WATCH_PID names a pid.
    watchdog::install();

    let config = match config::Config::from_env(|key| std::env::var(key).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("beacond: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let registry = match Registry::open(std::path::Path::new(&config.db_path)) {
        Ok(registry) => Arc::new(registry),
        Err(error) => {
            eprintln!("beacond: could not open registry at {}: {error}", config.db_path);
            return std::process::ExitCode::FAILURE;
        }
    };

    let listener = match tokio::net::TcpListener::bind(&config.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("beacond: could not bind {}: {error}", config.bind);
            return std::process::ExitCode::FAILURE;
        }
    };

    let bound_addr =
        listener.local_addr().map_or_else(|_| "<unknown>".to_string(), |addr| addr.to_string());
    tracing::info!(bind = %bound_addr, db = %config.db_path, "beacond listening");

    // MEASURED HERE, once, at start — while this binary's own file certainly
    // still exists on disk. A release that replaces it a minute from now is
    // exactly the state these facts exist to make visible, and it is a state
    // this process could no longer measure for itself.
    let app = router::router(registry, router::BeacondFacts::measure(config.db_path.clone()));
    let result = axum::serve(listener, app).with_graceful_shutdown(wait_for_signal()).await;

    if let Err(error) = result {
        eprintln!("beacond: serve failed: {error}");
        return std::process::ExitCode::FAILURE;
    }

    std::process::ExitCode::SUCCESS
}
