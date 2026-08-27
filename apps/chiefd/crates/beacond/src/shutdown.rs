//! The graceful-shutdown signal wait, written down once.
//!
//! # Why this lives in beacond's library
//!
//! [`wait_for_signal`] existed twice, byte for byte: once in this crate's own
//! `main.rs` and once in `chiefd`'s `docstore_only.rs`, each carrying a
//! doc comment naming the other as the thing it mirrored. Two copies of one
//! judge is how this tree has already produced real defects — a liveness probe
//! that answered "alive" for a failed `kill(pid, 0)` in half the tree, and a
//! trust boundary that gave three different answers — so the copy that both
//! sides can import wins over the copy each side maintains.
//!
//! beacond's library is where it goes because it is the lowest crate in this
//! graph that both binaries ALREADY depend on: `chiefd` and `chief-cli`
//! both carry `beacond = { path = "../beacond" }` today, and this adds no
//! external crate and no new edge. The `tokio` dependency is unchanged too —
//! this file only moves code that already compiled in this same package.

/// Await SIGINT or, on unix, SIGTERM — whichever the operator, the supervisor
/// or a test harness sends first.
///
/// This is the future a server hands to `with_graceful_shutdown`: it resolves
/// once, on the first signal, and the caller decides what draining means.
///
/// # It arms at the AWAIT point, and that is a real constraint
///
/// Constructing the `tokio` signal stream is what registers the handler, and
/// this function does that only when it is first polled. That is correct
/// wherever nothing has installed a competing SIGTERM handler beforehand — and
/// it is WRONG anywhere one has. Once a process installs its own SIGTERM
/// disposition, SIGTERM no longer terminates it by default, so every moment
/// between that install and this registration is a window in which a
/// supervisor's SIGTERM is silently discarded and the process runs until
/// SIGKILL. A caller in that position must arm first and await later
/// (`chiefd`'s `ArmedShutdownSignal` is exactly that shape) rather than
/// reach for this.
///
/// # Failure is non-fatal, deliberately
///
/// If the SIGTERM handler cannot be installed, that is warned about and the
/// wait falls back to SIGINT alone. A shutdown wait that refused to exist
/// would leave a server with no way to stop at all, which is strictly worse
/// than one that can only be stopped one way.
pub async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot install SIGTERM handler; falling back to SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
