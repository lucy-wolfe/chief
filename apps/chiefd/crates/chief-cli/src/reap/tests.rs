//! What a reap must do, proved against real process groups.
//!
//! These spawn `/bin/sh` in a process group of its own and let it fork a child
//! that outlives it, which is the shape of the live defect: `bun run test` from
//! a person's bash tool, whose parent is hung up while the work keeps running.
//! The group leader stands in for the tmux pane leader — a pane leader is the
//! session leader of its own pty session, so its pgid is its pid, which is the
//! same relationship these build with `process_group(0)`.

use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::Pid;

use super::{orphan_candidates, reap_process_groups, strays_under, ReapOutcome};

/// Spawn a group leader running `script`, and return it.
///
/// `process_group(0)` makes the child a process-group leader, so its pgid is
/// its pid and everything it forks joins that group — the property the whole
/// module depends on.
fn spawn_group(script: &str) -> Child {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    command.process_group(0);
    command.spawn().expect("spawn a group leader")
}

/// Is this pid still alive? Signal 0 is the existence question.
fn alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

/// Wait for `condition`, or give up. Returns whether it came true.
fn within(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        // os-liveness: these tests wait for the kernel to tear down real
        // process groups, which is the one thing an injected clock cannot
        // simulate — the whole point of the module is that the signals land.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(20));
    }
    condition()
}

/// Read one line of a child's stdout — the tests use it to learn a grandchild's
/// pid, which the parent cannot otherwise know.
fn first_line(child: &mut Child) -> String {
    use std::io::{BufRead as _, BufReader};
    let stdout = child.stdout.take().expect("piped stdout");
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).expect("read the child's first line");
    line.trim().to_owned()
}

/// THE DEFECT, as a test: work a pane started must not outlive the reap.
///
/// The leader forks a long sleep and exits immediately, so by the time the reap
/// runs the sleep is an ORPHAN whose parent is gone — reparented to init,
/// exactly like the nine survivors of the live stop. No ppid chain leads to it.
/// Only its process group still names it, and that is what is signalled.
#[test]
fn the_reap_stops_a_grandchild_that_already_outlived_its_parent() {
    // Print the background pid, then leave: the sleep is orphaned at once.
    let mut leader = spawn_group("sleep 120 & echo $!; exit 0");
    let orphan: i32 = first_line(&mut leader).parse().expect("the orphan's pid");
    let group = leader.id() as i32;
    leader.wait().expect("the leader exits");

    assert!(
        within(Duration::from_secs(2), || alive(orphan)),
        "precondition: the orphaned sleep is running"
    );

    let outcome = reap_process_groups(&[group]);
    assert_eq!(outcome.groups, 1, "the group was still there and was signalled");

    assert!(
        within(Duration::from_secs(3), || !alive(orphan)),
        "the orphan a stopped company left behind must be gone after the reap"
    );
}

/// A group that ignores SIGTERM is still stopped, and the outcome says so.
///
/// This is why the ladder does not end at SIGTERM: an operator's stop must
/// finish, and a runner that traps the signal must not be able to outlive it.
#[test]
fn a_group_that_ignores_sigterm_is_killed_and_counted() {
    let mut leader = spawn_group("trap '' TERM; echo ready; sleep 120");
    assert_eq!(first_line(&mut leader), "ready", "the leader armed its trap");
    let group = leader.id() as i32;

    let outcome = reap_process_groups(&[group]);
    assert_eq!(
        outcome,
        ReapOutcome { groups: 1, killed: 1, survivors: Vec::new() },
        "a group that would not stop on SIGTERM is killed, and the operator is told"
    );
    assert!(
        within(Duration::from_secs(2), || {
            leader.try_wait().ok().flatten().is_some() || !alive(group)
        }),
        "the group is gone"
    );
    let _ = leader.wait();
}

/// A group that obliges on SIGTERM is never killed, and the grace is not spent.
///
/// The leader is reaped by a thread of its own, because in production the pane
/// leader is TMUX's child and tmux reaps it. Without that, an exited leader
/// stays a zombie, a zombie is still a process for `kill(2)`, and the grace
/// loop below would wait out its whole budget and then report a group that
/// obliged as one that had to be killed.
#[test]
fn a_group_that_stops_on_sigterm_is_not_killed() {
    let mut leader = spawn_group("echo ready; sleep 120");
    assert_eq!(first_line(&mut leader), "ready", "the leader is up");
    let group = leader.id() as i32;
    let reaper = std::thread::spawn(move || leader.wait());

    let started = Instant::now();
    let outcome = reap_process_groups(&[group]);
    assert_eq!(
        outcome,
        ReapOutcome { groups: 1, killed: 0, survivors: Vec::new() },
        "SIGTERM was enough, so nothing was killed"
    );
    assert!(
        started.elapsed() < super::TERM_GRACE,
        "the reap returns as soon as the group is gone rather than waiting out the grace"
    );
    let _ = reaper.join();
}

/// A pid whose group is already gone is the outcome that was asked for.
///
/// A stop must not fail on a pane that obliged early, so this counts nothing
/// and reports nothing.
#[test]
fn a_group_that_is_already_gone_is_not_an_error() {
    let mut leader = spawn_group("exit 0");
    let group = leader.id() as i32;
    leader.wait().expect("the leader exits");
    assert!(
        within(Duration::from_secs(2), || killpg(Pid::from_raw(group), None).is_err()),
        "precondition: the group is gone"
    );

    assert_eq!(
        reap_process_groups(&[group]),
        ReapOutcome::default(),
        "an absent group is already stopped"
    );
}

/// **A DELIVERED `SIGKILL` IS NOT A DEATH, AND THE OUTCOME NOW SAYS WHICH.**
///
/// It is also not the same thing as a ZOMBIE, which is what the first cut of
/// this read-back got wrong: `killpg(pid, 0)` succeeds for a process that has
/// died and not yet been reaped, so every correctly-killed group reported as a
/// survivor. A receipt that cries wolf is the silence wearing the opposite
/// coat — the operator learns to ignore it, and then the real survivor goes
/// unread too. The check reads process STATE now, not the signal path.
///
/// `signal_group` answers whether the CALL succeeded. A process in
/// uninterruptible sleep stays in the table through `SIGKILL` until it returns
/// from the kernel, so "killed: 1" used to mean "we sent one signal" and was
/// read as "one died". A stop then reported success over something still
/// running — the class this codebase has named four times: an operation that
/// cannot report its own failure.
///
/// This pins the ordinary case from the other side: a group that DOES die
/// leaves `survivors` empty, so the field is a report about the world rather
/// than a constant. The uninterruptible case itself cannot be manufactured in a
/// unit test without a driver that blocks — which is exactly why the code reads
/// back instead of assuming, and why this test asserts the read HAPPENED rather
/// than simulating a survivor.
#[test]
fn a_group_that_really_dies_leaves_no_survivors_to_report() {
    let mut leader = spawn_group("trap '' TERM; echo ready; sleep 120");
    assert_eq!(first_line(&mut leader), "ready", "the leader armed its trap");
    let group = leader.id() as i32;

    let outcome = reap_process_groups(&[group]);

    assert_eq!(outcome.killed, 1, "it had to be SIGKILLed");
    assert!(
        outcome.survivors.is_empty(),
        "and it actually died, so there is nothing to report: {:?}",
        outcome.survivors
    );
    let _ = leader.wait();
}

/// THE BOUND. A reap signals the groups it was given and nothing else.
///
/// A shared box is the whole reason this module refuses to enumerate anything:
/// a neighbouring company's pane leads its own group, is never read from this
/// company's socket, and must survive this company's stop.
#[test]
fn a_group_that_was_not_named_survives_the_reap() {
    let mut mine = spawn_group("echo ready; sleep 120");
    assert_eq!(first_line(&mut mine), "ready", "my pane is up");
    let mut neighbour = spawn_group("echo ready; sleep 120");
    assert_eq!(first_line(&mut neighbour), "ready", "the neighbour's pane is up");
    let neighbour_group = neighbour.id() as i32;

    reap_process_groups(&[mine.id() as i32]);

    assert!(
        killpg(Pid::from_raw(neighbour_group), None).is_ok(),
        "a group this company never named must not be touched by this company's stop"
    );
    let _ = killpg(Pid::from_raw(neighbour_group), Signal::SIGKILL);
    let _ = mine.wait();
    let _ = neighbour.wait();
}

/// An empty list is a no-op — a company with no panes has nothing to reap.
#[test]
fn no_panes_is_no_signals() {
    assert_eq!(reap_process_groups(&[]), ReapOutcome::default());
}

// --- the search: which groups a pane owns -------------------------------

use super::{groups_below, read_process_table, reap_panes, ProcessRow};

/// A hand-written table, so the bound can be asserted without a box.
fn row(pid: i32, ppid: i32, pgid: i32) -> ProcessRow {
    ProcessRow { pid, ppid, pgid }
}

/// THE MEASURED DEFECT, as a table. A `setsid` child leads its own session and
/// its own group, so signalling the PANE's group never reaches it — which is
/// why the search walks the parent edge and not the group.
///
/// The pids are the ones actually observed:
///
/// ```text
/// 11465 pane leader     pgid 11465
/// 11468 plain child     pgid 11465   dies with the pane
/// 11467 setsid child    pgid 11467   survived the stop
/// ```
#[test]
fn a_setsid_child_is_found_even_though_its_group_is_not_the_panes() {
    let table = [row(11465, 11463, 11465), row(11468, 11465, 11465), row(11467, 11465, 11467)];
    assert_eq!(
        groups_below(&table, &[11465], 999),
        vec![11465, 11467],
        "the group a setsid child leads must be found, or the work outlives the stop"
    );
}

/// The search follows the chain as far as it goes: `bun run test` starts turbo,
/// turbo starts vitest, and none of them is the pane.
#[test]
fn the_search_reaches_a_whole_command_tree_and_not_only_its_first_process() {
    let table = [
        row(100, 1, 100),      // the pane leader
        row(8378, 100, 8378),  // bun run test, setsid into its own group
        row(8379, 8378, 8378), // turbo
        row(8447, 8379, 8447), // vitest, its own group again
    ];
    assert_eq!(groups_below(&table, &[100], 999), vec![100, 8378, 8447]);
}

/// THE BOUND. Nothing that does not descend from a pane is ever named.
#[test]
fn a_process_that_does_not_descend_from_a_pane_is_never_named() {
    let table = [
        row(100, 1, 100),   // this company's pane
        row(101, 100, 101), // its work
        row(200, 1, 200),   // a neighbouring company's pane
        row(201, 200, 200), // the neighbour's work
        row(1, 0, 1),       // init
    ];
    assert_eq!(
        groups_below(&table, &[100], 999),
        vec![100, 101],
        "a stop must name only what its own panes started"
    );
}

/// A stop issued from INSIDE one of the company's own panes must stop the
/// company, not itself. Without this the reap kills the `chief stop` that
/// ordered it, half way through the teardown.
#[test]
fn the_stops_own_process_group_is_never_signalled() {
    let table = [
        row(100, 1, 100),   // the pane
        row(555, 100, 555), // `chief stop`, run from that pane
        row(556, 555, 555), // and something it started
        row(101, 100, 101), // the work that must die
    ];
    assert_eq!(
        groups_below(&table, &[100], 555),
        vec![100, 101],
        "the group this stop runs in must survive its own reap"
    );
}

/// init and group 0 are never signalled, whatever the table says.
#[test]
fn no_reap_ever_names_group_one_or_zero() {
    let table = [row(100, 1, 1), row(101, 100, 0), row(102, 100, 102)];
    assert_eq!(groups_below(&table, &[100], 999), vec![102]);
}

/// A pane tmux named that `ps` did not list is still this company's, and its
/// pid is its group — a tmux pane leader leads one.
#[test]
fn a_pane_missing_from_the_table_is_still_reaped_as_its_own_group() {
    assert_eq!(groups_below(&[], &[4242], 999), vec![4242]);
}

/// A table that reports a cycle must terminate rather than hang a stop.
#[test]
fn a_cyclic_table_does_not_hang_the_search() {
    let table = [row(100, 101, 100), row(101, 100, 101)];
    assert_eq!(groups_below(&table, &[100], 999), vec![100, 101]);
}

/// The reader is real: this process is in the table it returns, with the parent
/// and group the kernel reports.
#[test]
fn the_process_table_reader_finds_this_very_process() {
    let table = read_process_table();
    assert!(!table.is_empty(), "ps produced a table");
    let me = std::process::id() as i32;
    let mine = table.iter().find(|row| row.pid == me).expect("this process is in the table");
    assert_eq!(mine.pgid, nix::unistd::getpgrp().as_raw(), "the group ps reports is the real one");
}

/// END TO END, against real processes: a `setsid` descendant of a pane is
/// stopped by a reap of that pane.
///
/// This is the case a process-group kill cannot reach and `kill-session` did
/// not reach — the live survivors, in miniature.
#[test]
fn reaping_a_pane_stops_a_setsid_descendant_of_it() {
    let mut leader = spawn_group("setsid sleep 120 & echo $!; sleep 120");
    let escaped: i32 = first_line(&mut leader).parse().expect("the escaped pid");
    let pane = leader.id() as i32;
    assert!(within(Duration::from_secs(2), || alive(escaped)), "precondition: it is running");

    // It really did escape: its group is not the pane's. WAITED FOR, and no
    // longer skipped when the row is missing.
    //
    // `echo $!` names the child the instant the shell forks it, and `setsid`
    // changes that child's group a moment later. A single sample can therefore
    // catch it still sitting in the PANE's group and report the fixture broken
    // — measured under a loaded parallel run, `pgid` and `pane` both read the
    // same pid. The old `if let` had the mirror of that hole: a row `ps` had
    // not listed yet made the whole precondition vanish silently, so the test
    // went on to "prove" a reap reached a process it had never established was
    // outside the group.
    let mut observed = None;
    assert!(
        within(Duration::from_secs(5), || {
            observed =
                read_process_table().into_iter().find(|row| row.pid == escaped).map(|row| row.pgid);
            observed.is_some_and(|pgid| pgid != pane)
        }),
        "precondition: setsid must put it in a group of its own, which is why the group kill \
         misses it; ps reported group {observed:?} for pid {escaped} against pane {pane}"
    );

    reap_panes(&[pane]);

    assert!(
        within(Duration::from_secs(3), || !alive(escaped)),
        "a setsid descendant of a company's pane must not outlive the company's stop"
    );
    let _ = kill(Pid::from_raw(escaped), Some(Signal::SIGKILL));
    let _ = leader.wait();
}

/// A DOUBLE FORK IS THE ONE SHAPE THE PARENT CHAIN CANNOT SEE, and the pure
/// candidate filter is what separates it from everything else with `ppid == 1`.
#[test]
fn only_a_process_reparented_to_init_is_a_candidate() {
    let table = [
        // The double-forked child. Nothing leads to it.
        ProcessRow { pid: 900, ppid: 1, pgid: 900 },
        // The operator's own shell, sitting in the company directory. Its
        // parent is alive, so a cwd-scoped sweep must not reach it — this is
        // the false alarm the marker exists to prevent.
        ProcessRow { pid: 901, ppid: 400, pgid: 901 },
        // This stop itself, and a sibling in its group.
        ProcessRow { pid: 902, ppid: 1, pgid: 902 },
        // init.
        ProcessRow { pid: 1, ppid: 0, pgid: 1 },
    ];
    assert_eq!(
        orphan_candidates(&table, 902, &[]),
        vec![900],
        "only the reparented process is a candidate: not a child of a live parent, not this \
         stop's own group, and not init"
    );
    assert_eq!(
        orphan_candidates(&table, 0, &[900]),
        vec![902],
        "a pid the caller already accounts for -- the company daemon, which holds the company \
         directory on purpose -- is never reported a second time"
    );
}

/// END TO END, against a real double fork: a stop names the process no pane's
/// tree leads to, and it names it by its working directory.
#[test]
fn a_double_forked_process_in_the_company_directory_is_named() {
    let company = tempfile::tempdir().expect("a company directory");
    let dir = company.path().canonicalize().expect("canonical");
    // Fork, background, and let the intermediate shell EXIT — which reparents
    // the sleeper to init and severs every chain a reap could walk.
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("sleep 120 & echo $!")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // ITS OWN PROCESS GROUP, and this is a property of the case rather than a
    // fixture convenience. `strays_under` skips this stop's own group, so a
    // stop cannot report itself or the shell that launched it. A child left in
    // the test runner's group would therefore be filtered out — measured, the
    // first run of this test returned an empty sweep for exactly that reason.
    // A real detached process leads a group of its own; this makes the fixture
    // the shape the detector is about.
    command.process_group(0);
    let mut spawner = command.spawn().expect("spawn the intermediate parent");
    let orphan: i32 = first_line(&mut spawner).parse().expect("the orphan pid");
    let _ = spawner.wait();

    // REFUSE RATHER THAN FAIL if the host adopted the orphan itself. A process
    // that installed itself as a child subreaper takes the place of init, so
    // `ppid` never reaches 1 and the marker legitimately does not match. That
    // is a fact about the host, not about this code, and reporting it as a red
    // would be the dead-red-hides-a-live-one failure this repo keeps naming.
    let reparented = within(Duration::from_secs(5), || {
        read_process_table().iter().any(|row| row.pid == orphan && row.ppid == 1)
    });
    if !reparented {
        eprintln!(
            "CANNOT CHECK: pid {orphan} was not reparented to init, so this host adopts orphans \
             (a child subreaper) and the double-fork marker cannot apply"
        );
        let _ = kill(Pid::from_raw(orphan), Some(Signal::SIGKILL));
        return;
    }

    let strays = strays_under(&dir, &[]);
    assert!(
        strays.iter().any(|stray| stray.pid == orphan),
        "a double-forked process holding the company directory must be NAMED by the stop; got \
         {strays:?}"
    );
    assert!(
        strays.iter().all(|stray| stray.cwd.starts_with(&dir)),
        "every stray is reported with the working directory that is the evidence for it: \
         {strays:?}"
    );

    // AND IT IS STILL ALIVE. Detection, not a kill -- the whole point of the
    // sweep is that a working directory is evidence of ownership and not
    // authority to signal, so a test that did not check this would pass over a
    // change that started killing.
    assert!(alive(orphan), "the sweep must NAME a stray, never signal it");

    // The same process is NOT named when the caller already accounts for it.
    assert!(
        strays_under(&dir, &[orphan]).iter().all(|stray| stray.pid != orphan),
        "a known pid is excluded, which is how the company daemon's own cwd stays out of the \
         refusal"
    );

    let _ = kill(Pid::from_raw(orphan), Some(Signal::SIGKILL));
}
