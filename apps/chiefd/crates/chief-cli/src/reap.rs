//! Reap the processes a company's panes started.
//!
//! # The defect this closes
//!
//! `chief stop` reported, truthfully, `sessionStopped: true`, `actuatorStopped:
//! true`, `daemonStopped: true` — the tmux server was gone and so was chiefd.
//! Nine processes kept running, at loadavg 4.20 on a 2-vCPU box:
//!
//! ```text
//! 8378  bun run test
//! 8379  node node_modules/.bin/turbo run test:unit --continue
//! 8391  turbo run test:unit
//! 8444  bun run test:unit          (packages/piing)
//! 8447  node …/vitest run          (packages/piing)
//! 8445  bun run test:unit          (apps/web)
//! 8449  node …/vitest run          (apps/web)
//! + two more node children
//! ```
//!
//! A person had run `bun run test` from a bash tool. The stop path never held a
//! pid — `runtime_process_handles` stores tmux PANE IDS, and the one place a
//! `#{pane_pid}` is read uses it as an identity fence and then issues
//! `kill-pane` — so termination was delegated entirely to tmux. **A stopped
//! company could consume the machine indefinitely.**
//!
//! Stopping a company must stop the work it started, not only the panes that
//! started it.
//!
//! # Why the process GROUP is not enough, measured
//!
//! `kill-session` hangs up the pane's own process group, so a child that stayed
//! in that group dies with it. The survivors did not stay in it. A tool runner
//! that wants to be able to stop a whole command tree starts the command with
//! `setsid`, and a `setsid` child leads a brand-new SESSION:
//!
//! ```text
//! pid=11465 ppid=11463 pgid=11465 sid=11465   the pane leader
//! pid=11468 ppid=11465 pgid=11465 sid=11465   a plain child — DIES with the pane
//! pid=11467 ppid=11465 pgid=11467 sid=11467   a setsid child — SURVIVES
//! ```
//!
//! Signalling the pane's group reaches 11468 and never 11467. So the group is
//! the unit of the KILL and cannot be the unit of the SEARCH.
//!
//! # What is searched, and the bound
//!
//! The parent chain. At the moment a stop runs, every survivor-to-be is still a
//! descendant of a live pane — `setsid` changes the session, never the ppid —
//! so the tree is intact and names them all. It is intact only until the panes
//! die, which is why [`super::stop`] reaps before it kills: afterwards the
//! orphans are reparented to init and no chain leads to them.
//!
//! Killing broadly on a shared box is its own hazard, so nothing here
//! enumerates the machine for anything but the parent map. Every process group
//! signalled satisfies all of:
//!
//! 1. it contains a process that is the pane, or a descendant of a pane, of
//!    **this company's own session** on **this company's own socket** —
//!    `org-<slug>-<key6>_` or its actuator, which the caller is about to kill;
//! 2. it is not the group this `chief stop` is itself running in, so a stop
//!    issued from inside one of the company's own panes stops the company and
//!    not itself;
//! 3. it is a real group id above 1.
//!
//! A neighbouring company's panes are on another socket under another session
//! name, are never read, and descend from nothing this reads — so they are
//! never signalled.
//!
//! # What the parent chain cannot catch, and what is done about it
//!
//! The chain is intact for a `setsid` child and NOT for a double fork: a
//! process that forks, lets the intermediate parent exit, and is reparented to
//! init has no chain leading to it from the moment it starts. The product's own
//! foreground-bash guidance tells agents to detach exactly that way for a
//! persistent deliverable, so this is a shape the company is INSTRUCTED to
//! produce — real by construction, and enumerated as empty on
//! a live box, 2026-08-24, which is a statement about that hour and
//! not about the shape.
//!
//! [`strays_under`] finds them, by working directory, and NAMES them. It does
//! not signal: a cwd is evidence of belonging and not authority to kill, which
//! is the same line the rest of this module draws. See its own docs for why the
//! marker is `ppid == 1` rather than the cwd alone.
//!
//! # Order
//!
//! `SIGTERM` first so a test runner can flush and exit, then `SIGKILL` for
//! whatever is left: a stop the operator asked for must finish, and a process
//! that ignores `SIGTERM` must not be able to outlive it.

use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::{getpgrp, Pid};

/// How long a pane's group has to exit on `SIGTERM` before `SIGKILL`.
///
/// One second, and short on purpose: this runs inside an interactive `chief
/// stop`. It is a courtesy to a runner that can flush in that time, never a
/// negotiation — the `SIGKILL` below is unconditional, so the stop completes
/// whatever the group does.
const TERM_GRACE: Duration = Duration::from_millis(1_000);

/// How often the grace loop asks whether the group is gone.
const POLL: Duration = Duration::from_millis(50);

/// Wait one poll interval.
///
/// os-liveness: what is being waited out is the KERNEL tearing a process group
/// down after a signal it has already been sent. There is nothing to wake on
/// and no clock a caller could inject, and every wait is bounded by
/// [`TERM_GRACE`]. Narrow and at the call site, so the exemption stays
/// greppable — the same shape as `tmux::replay_wait`.
fn poll_wait() {
    #[allow(clippy::disallowed_methods)]
    std::thread::sleep(POLL);
}

/// What one reap did, so a caller can report it and a test can assert it.
// `Copy` is gone with the addition of `survivors`: the outcome now carries the
// PIDS that survived rather than only counts, and a Vec cannot be Copy. Nothing
// relied on the bit-copy — every reader takes it by value once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapOutcome {
    /// Process groups that were signalled at all — i.e. that still existed.
    pub groups: usize,
    /// Groups that were still alive after the grace and had to be `SIGKILL`ed.
    pub killed: usize,
    /// Groups that were still there AFTER the `SIGKILL` — read back, not assumed.
    ///
    /// # Why a successful `SIGKILL` is not proof of death
    ///
    /// `signal_group` answers whether the CALL succeeded, and a successful
    /// `kill(2)` means the signal was delivered, never that the target is gone.
    /// A process in uninterruptible sleep — blocked in a driver, on an NFS
    /// mount, on a device that will not answer — stays in the table through
    /// `SIGKILL` until it returns from the kernel. So the previous outcome
    /// counted "how many we killed" and could not distinguish that from "how
    /// many died".
    ///
    /// That mattered because of what the caller does with it: a stop reported
    /// success while something it had signalled was still running, which is the
    /// class this codebase has now named four times — an operation that cannot
    /// report its own failure. The operator runs `/stop`, gets a clean receipt,
    /// and something is still alive.
    pub survivors: Vec<i32>,
}

/// Is this errno the answer "that group is already gone"?
///
/// `ESRCH` is the plain answer. `EPERM` is treated as gone too and is the one
/// judgement call here: it means some process in the group is not ours to
/// signal, which on a shared box means the pid has been recycled by another
/// owner. Retrying cannot help and escalating is exactly the broad kill this
/// module refuses to do, so it stops.
fn group_is_gone(errno: Errno) -> bool {
    matches!(errno, Errno::ESRCH | Errno::EPERM)
}

/// Signal one process group, reporting whether it existed.
///
/// `signal: None` is `kill(2)`'s signal 0 — the existence question, which
/// performs the same permission checks and delivers nothing. It is how the
/// grace loop below asks "is this group gone yet" without a side effect;
/// `SIGCONT` would have answered the same question and resumed a group the
/// operator may have stopped on purpose.
fn signal_group(pid: i32, signal: Option<Signal>) -> bool {
    match killpg(Pid::from_raw(pid), signal) {
        Ok(()) => true,
        Err(errno) => !group_is_gone(errno),
    }
}

/// Stop every process group in `pane_pids`: `SIGTERM`, a bounded grace, then
/// `SIGKILL` for whatever survived.
///
/// Idempotent and never fatal. A group that is already gone is the outcome that
/// was asked for, so it is counted as nothing and never reported as an error —
/// a stop that has torn the runtime down must not fail on a process that
/// obliged early.
pub fn reap_process_groups(pane_pids: &[i32]) -> ReapOutcome {
    let mut outcome = ReapOutcome::default();
    let mut pending: Vec<i32> = Vec::new();
    for pid in pane_pids {
        if signal_group(*pid, Some(Signal::SIGTERM)) {
            outcome.groups += 1;
            pending.push(*pid);
        }
    }
    if pending.is_empty() {
        return outcome;
    }
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        pending.retain(|pid| signal_group(*pid, None));
        if pending.is_empty() {
            return outcome;
        }
        poll_wait();
    }
    let mut killed: Vec<i32> = Vec::new();
    for pid in pending {
        if signal_group(pid, Some(Signal::SIGKILL)) {
            outcome.killed += 1;
            killed.push(pid);
        }
    }
    // READ BACK WHAT SHOULD BE GONE. A delivered `SIGKILL` is not a death: an
    // uninterruptible-sleep process stays in the table until it returns from
    // the kernel, and the caller was reporting a clean stop over the top of it.
    // Bounded by the same grace the SIGTERM wait uses — a process that has not
    // gone by then is not going to, and waiting longer would trade one silent
    // failure for a hang.
    if !killed.is_empty() {
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            killed.retain(|pid| group_is_running(*pid));
            if killed.is_empty() {
                break;
            }
            poll_wait();
        }
        outcome.survivors = killed;
    }
    outcome
}

/// Whether any process in `group` is still RUNNING — not merely still in the
/// table.
///
/// # A ZOMBIE IS NOT A SURVIVOR, and signal 0 cannot tell you that
///
/// `killpg(pid, 0)` succeeds for a process that has died and not yet been
/// reaped by its parent, because the entry is still there. So the first cut of
/// this read-back reported every correctly-killed group as a survivor whenever
/// the parent had not yet called `wait` — which is the normal case immediately
/// after a `SIGKILL`, and would have handed the operator a false alarm on every
/// stop that had to escalate.
///
/// A false alarm is not a harmless direction to fail in. It is the same defect
/// as the silence, wearing the opposite coat: a receipt that cries wolf is one
/// the operator learns to ignore, and then the real survivor goes unread too.
///
/// So the question is asked of the process TABLE, which distinguishes a live
/// process from a zombie by state, rather than of the signal path, which
/// cannot.
fn group_is_running(group: i32) -> bool {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-o", "pid=,stat=", "-g", &group.to_string()])
        .output()
    else {
        // A `ps` that will not run is not evidence of a survivor. Positive
        // evidence only — the same rule the click verification and the tmux
        // session read-back follow.
        return false;
    };
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(_pid) = fields.next() else { return false };
        // The first letter of `stat` is the state. `Z` is a zombie: dead, and
        // waiting only for its parent to notice.
        fields.next().is_some_and(|state| !state.starts_with('Z'))
    })
}

/// One row of the process table: who it is, who started it, which group it is
/// signalled as part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessRow {
    /// The process.
    pub pid: i32,
    /// Its parent — the edge the search walks.
    pub ppid: i32,
    /// Its process group — the unit the kill addresses.
    pub pgid: i32,
}

/// Read the box's process table.
///
/// `/bin/ps -Ao pid=,ppid=,pgid=` and nothing cleverer: it is POSIX, it behaves
/// identically on Darwin and Linux, and it needs no `/proc`, which macOS does
/// not have.
///
/// The path is ABSOLUTE, and that matters more here than at most spawn sites.
/// A bare `ps` would be resolved against whatever `PATH` this stop inherited,
/// and a miss is not loud: it yields an empty table, which reaps nothing and
/// still lets the stop report success. `/bin/ps` is where POSIX puts it on both
/// platforms, so there is one answer and no fallback to get wrong.
///
/// A row that does not parse is dropped rather than guessed at — a signal is
/// not something to send to a number nobody read.
pub fn read_process_table() -> Vec<ProcessRow> {
    let Ok(output) =
        std::process::Command::new("/bin/ps").args(["-Ao", "pid=,ppid=,pgid="]).output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let pgid = fields.next()?.parse().ok()?;
            Some(ProcessRow { pid, ppid, pgid })
        })
        .collect()
}

/// Every process group that holds `roots` or anything descended from them,
/// except the group `mine` names.
///
/// Pure, so the whole bound can be asserted against a table written by hand.
/// The walk is breadth-first over the parent edge with a visited set, so a
/// table that somehow reports a cycle terminates rather than hanging a stop.
#[must_use]
pub fn groups_below(table: &[ProcessRow], roots: &[i32], mine: i32) -> Vec<i32> {
    let mut seen: std::collections::BTreeSet<i32> = roots.iter().copied().collect();
    let mut frontier: Vec<i32> = seen.iter().copied().collect();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for row in table {
            if frontier.contains(&row.ppid) && seen.insert(row.pid) {
                next.push(row.pid);
            }
        }
        frontier = next;
    }
    let mut groups: std::collections::BTreeSet<i32> = table
        .iter()
        .filter(|row| seen.contains(&row.pid))
        .map(|row| row.pgid)
        .filter(|pgid| *pgid > 1 && *pgid != mine)
        .collect();
    // A root read from tmux that `ps` never listed is still a pane this stop
    // owns; its own pid is its group, because a tmux pane leader leads one.
    for root in roots {
        if *root > 1 && *root != mine && !table.iter().any(|row| row.pid == *root) {
            groups.insert(*root);
        }
    }
    groups.into_iter().collect()
}

/// A process that outlived the stop and that no group this stop reaped held.
///
/// Detection ONLY. Nothing here signals anything — see [`strays_under`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stray {
    /// The process.
    pub pid: i32,
    /// Its working directory, which is the evidence that it belongs to this
    /// company. Reported so the operator can identify it without a second tool.
    pub cwd: std::path::PathBuf,
}

/// Processes worth interrogating: reparented to init, outside this stop's own
/// group, and not one of the pids the caller already accounts for.
///
/// # Why `ppid == 1` is the marker, and not "anything with the company's cwd"
///
/// The reap above walks the PARENT CHAIN, and that catches every child a pane
/// started — including a `setsid` one, because `setsid` changes the session and
/// never the ppid. The case it cannot catch is a DOUBLE FORK: a process that
/// forks, exits the intermediate parent, and is reparented to init immediately,
/// so no chain leads to it even while the panes are still alive. That is the
/// shape the product's own foreground-bash guidance tells agents to use for a
/// persistent deliverable — "a truly detached process with redirected stdio and
/// an explicit supervisor" — so it is real by INSTRUCTION rather than by
/// observation.
///
/// `ppid == 1` is exactly that shape, and it is also what keeps this from
/// crying wolf. A sweep scoped by cwd alone would name the operator's own shell
/// sitting in the company directory, a second terminal tab, and every tmux
/// client — none of which the stop has any business reporting. Their parents
/// are alive, so none of them is here.
///
/// Pure, so the whole bound is assertable against a table written by hand.
#[must_use]
pub fn orphan_candidates(table: &[ProcessRow], mine: i32, known: &[i32]) -> Vec<i32> {
    table
        .iter()
        .filter(|row| row.ppid == 1)
        .filter(|row| row.pid > 1 && row.pgid != mine)
        .filter(|row| !known.contains(&row.pid))
        .map(|row| row.pid)
        .collect()
}

/// One process's working directory, or `None` when it cannot be read.
///
/// Two backends, because there is no one portable answer and this repo requires
/// identical behaviour on both platforms: `/proc/<pid>/cwd` on Linux, which is
/// always present, and `lsof` on macOS, which ships with the system and where
/// `/proc` does not exist at all.
///
/// `None` is "could not say", never "not this company". A process whose cwd
/// this cannot read is not reported — positive evidence only, the same rule the
/// `SIGKILL` read-back above and the click verification follow.
#[cfg(target_os = "linux")]
fn process_cwd(pid: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// As the Linux arm above. `-d cwd` selects the one descriptor, `-Fn` prints it
/// on its own `n`-prefixed line, and `-a` makes the two selectors an AND rather
/// than lsof's default OR — without it this would report every file the process
/// has open.
#[cfg(not(target_os = "linux"))]
fn process_cwd(pid: i32) -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .map(std::path::PathBuf::from)
}

/// Every double-forked process still sitting inside `company_dir` that this
/// stop did not reap.
///
/// # This DETECTS, and deliberately does not kill
///
/// A cwd is strong evidence of belonging and it is not authority to signal. The
/// reap above signals only what a parent chain from this company's own panes
/// proves is ours; a directory can be shared, entered by a neighbour, or held
/// by something the operator started themselves and wants to keep. So the
/// answer goes into the stop's refusal, where the operator decides — which is
/// also the difference between this and the `pkill -f "chief"` that a stop
/// nobody could trust drove them to.
///
/// `known` is the infrastructure this stop already accounts for by pid, so it
/// is not reported twice: the company daemon, which legitimately holds the
/// company directory as its cwd and which a `--preserve-daemon` stop keeps on
/// purpose.
#[must_use]
pub fn strays_under(company_dir: &std::path::Path, known: &[i32]) -> Vec<Stray> {
    let Ok(company_dir) = company_dir.canonicalize() else { return Vec::new() };
    orphan_candidates(&read_process_table(), getpgrp().as_raw(), known)
        .into_iter()
        .filter_map(|pid| Some(Stray { pid, cwd: process_cwd(pid)? }))
        .filter(|stray| stray.cwd.starts_with(&company_dir))
        .collect()
}

/// Stop every process group that holds one of `pane_pids` or anything those
/// panes started.
///
/// This is the whole verb [`super::stop`] calls. It reads the table once, so
/// every group it signals was named by one consistent snapshot taken while the
/// panes were still alive.
pub fn reap_panes(pane_pids: &[i32]) -> ReapOutcome {
    if pane_pids.is_empty() {
        return ReapOutcome::default();
    }
    let groups = groups_below(&read_process_table(), pane_pids, getpgrp().as_raw());
    reap_process_groups(&groups)
}

#[cfg(test)]
mod tests;
