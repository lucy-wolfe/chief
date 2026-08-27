//! Named pause points (TESTING.md §4.3).
//!
//! A crash-injection test cannot be written against "somewhere in the middle
//! of the host transaction". It needs the process to be *parked at a named
//! instant* so that killing it is deterministic — the project's hard rule is
//! that no race is ever validated by repetition (TESTING.md §1.2), and a test
//! that SIGKILLs on a timer is exactly that.
//!
//! So the host-transaction path calls [`at`] at each durable boundary. In a
//! release build [`at`] compiles to nothing: the hook, and the whole notion of
//! a pause point, exist only under the `test-support` feature. The crash tests
//! install a hook that SIGKILLs the process when it sees the point it is
//! interested in and ignores every other point.

/// Announce that execution has reached the named durable boundary.
///
/// A no-op unless the `test-support` feature is on.
#[cfg(not(any(test, feature = "test-support")))]
#[inline]
pub fn at(_name: &str) {}

#[cfg(any(test, feature = "test-support"))]
pub use enabled::{at, install, installed_names, uninstall};

#[cfg(any(test, feature = "test-support"))]
mod enabled {
    use std::cell::RefCell;

    type Hook = Box<dyn Fn(&str)>;

    // THREAD-SCOPED, not process-global, and that is a bug fix.
    //
    // `cargo test` runs a crate's tests as threads in ONE process. With a
    // global hook, a hook installed by test A fires for a pause point reached
    // by test B — so B's transaction wrote into A's observation sink, and A
    // failed an assertion about its own invariant roughly one run in twenty.
    // The invariant was never violated; the sink was contaminated by a
    // stranger. That is the failure mode that teaches people to re-run CI
    // until it goes green, so it is fixed at the seam rather than papered over
    // in the one test that happened to notice.
    //
    // Failure polarity if a pause point is ever reached on a different thread
    // from the installer: the hook does not fire, the crash tests report "the
    // pause point was never reached", and they FAIL. Loud, not silent — which
    // is the right way round. Today every `pause::at` runs on the caller's
    // thread and both installers install on the thread that then drives the
    // transaction, so nothing relies on cross-thread firing.
    thread_local! {
        static SLOT: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Announce that execution has reached the named durable boundary.
    ///
    /// Records the name (so a test can assert the boundaries were reached in
    /// the documented order even when it does not crash) and then runs this
    /// thread's installed hook, if any. The borrow is released before the hook
    /// runs so a hook that never returns — the SIGKILL case — cannot leave a
    /// borrow outstanding, and so a hook that itself reaches a pause point
    /// does not panic on a re-entrant borrow.
    pub fn at(name: &str) {
        SEEN.with(|seen| seen.borrow_mut().push(name.to_owned()));
        let hook = SLOT.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook(name);
            SLOT.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(hook);
                }
            });
        }
    }

    /// Install a hook on **this thread** and clear this thread's recorded
    /// names.
    pub fn install(hook: impl Fn(&str) + 'static) {
        SEEN.with(|seen| seen.borrow_mut().clear());
        SLOT.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// Remove this thread's hook. The recorded names survive.
    pub fn uninstall() {
        SLOT.with(|slot| *slot.borrow_mut() = None);
    }

    /// Every pause point reached on this thread since the last [`install`],
    /// in order.
    #[must_use]
    pub fn installed_names() -> Vec<String> {
        SEEN.with(|seen| seen.borrow().clone())
    }
}
