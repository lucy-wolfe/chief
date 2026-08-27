//! Process liveness and ancestry, read from `/proc`.
//!
//! Plan §4 and §6.2. Two facts chiefd needs about a pid and cannot get any
//! other way:
//!
//! * **kernel start time** (`/proc/<pid>/stat` field 22, the 20th field after
//!   the comm) — pids recycle, `(pid, start_time)` does not. This is what lets
//!   a restarted chiefd reap its own dead leases instantly instead of waiting
//!   out a TTL, and what defeats pid recycling on the *observed* pid.
//! * **ancestry** — the caller's peercred pid must descend from the pane
//!   process whose identity it claims.
//!
//! Parsing note that matters: the comm field is the process name in
//! parentheses and may itself contain spaces and parentheses. Splitting the
//! whole line on whitespace is the classic way to mis-parse this; every field
//! here is taken after the **last** `") "`, as `org-caller-auth.ts:29` does.

use std::path::{Path, PathBuf};

use crate::{HostErr, Pid, ProcIdentity};

/// Fields after the comm, 1-based: `state` is 1, `ppid` is 2, `starttime` 20.
const PPID_OFFSET: usize = 1;
const STARTTIME_OFFSET: usize = 19;

/// Reads process facts from a `/proc`-shaped directory.
///
/// The root is injectable so the parser can be tested against fixture files —
/// the parsing is where the bugs are, not the syscall.
#[derive(Debug, Clone)]
pub struct ProcReader {
    root: PathBuf,
}

impl Default for ProcReader {
    fn default() -> Self {
        Self { root: PathBuf::from("/proc") }
    }
}

impl ProcReader {
    /// A reader over an alternative `/proc` root.
    #[must_use]
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Kernel identity of a pid: the pid plus its start time.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] when the process is gone or `/proc` is not
    /// readable — an absent process is not an error *value* here because every
    /// caller must distinguish "dead" from "unreadable"; both are `Err`, and
    /// liveness is asked separately via [`ProcReader::is_alive`].
    pub fn identity(&self, pid: Pid) -> Result<ProcIdentity, HostErr> {
        let stat = self.stat(pid)?;
        let start_time = field(&stat, STARTTIME_OFFSET)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| HostErr::ToolUnavailable {
                tool: "proc",
                detail: format!("/proc/{pid}/stat has no readable start time"),
            })?;
        Ok(ProcIdentity { pid, start_time })
    }

    /// The parent pid, or `None` for pid 1 / a process whose parent is 0.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] when the process is gone.
    pub fn parent(&self, pid: Pid) -> Result<Option<Pid>, HostErr> {
        let stat = self.stat(pid)?;
        let ppid = field(&stat, PPID_OFFSET)
            .and_then(|value| value.parse::<i32>().ok())
            .ok_or_else(|| HostErr::ToolUnavailable {
                tool: "proc",
                detail: format!("/proc/{pid}/stat has no readable ppid"),
            })?;
        Ok(if ppid <= 0 { None } else { Some(Pid(ppid)) })
    }

    /// Whether a process exists at all.
    #[must_use]
    pub fn is_alive(&self, pid: Pid) -> bool {
        self.stat_path(pid).exists()
    }

    /// Whether `child` descends from `ancestor`.
    ///
    /// Walks upward from the child, bounded: a `/proc` that presents a cycle
    /// (or a racing pid reuse) must terminate as "no", never spin.
    ///
    /// # Errors
    /// [`HostErr::ToolUnavailable`] if the child is gone before the walk
    /// starts. A parent that disappears mid-walk ends the walk with `false` —
    /// an ancestry that cannot be proven is not an ancestry.
    pub fn descends_from(&self, child: Pid, ancestor: Pid) -> Result<bool, HostErr> {
        if child == ancestor {
            return Ok(true);
        }
        // The first read establishes the child exists; after that a vanished
        // process simply ends the chain.
        let mut current = match self.parent(child)? {
            Some(parent) => parent,
            None => return Ok(false),
        };
        for _ in 0..MAX_ANCESTRY_DEPTH {
            if current == ancestor {
                return Ok(true);
            }
            match self.parent(current) {
                Ok(Some(parent)) => current = parent,
                Ok(None) | Err(_) => return Ok(false),
            }
        }
        Ok(false)
    }

    fn stat_path(&self, pid: Pid) -> PathBuf {
        self.root.join(pid.0.to_string()).join("stat")
    }

    fn stat(&self, pid: Pid) -> Result<String, HostErr> {
        read_to_string(&self.stat_path(pid)).map_err(|error| HostErr::ToolUnavailable {
            tool: "proc",
            detail: format!("/proc/{pid}/stat: {error}"),
        })
    }
}

/// `/proc` on Linux is not deep; anything past this is a malformed view.
const MAX_ANCESTRY_DEPTH: usize = 4096;

fn read_to_string(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// Field `offset` (0-based) of the stat line, counted **after** the comm.
fn field(stat: &str, offset: usize) -> Option<&str> {
    let after_comm = stat.rfind(") ").map(|index| &stat[index + 2..])?;
    after_comm.split_whitespace().nth(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line shaped like the kernel's, with a comm that contains the
    /// exact characters that break naive parsers.
    fn stat_line(pid: i32, comm: &str, ppid: i32, start_time: u64) -> String {
        let mut fields = vec![pid.to_string(), format!("({comm})"), "S".into(), ppid.to_string()];
        // pad out to the starttime field
        for index in 0..22 {
            let value = if index == STARTTIME_OFFSET - PPID_OFFSET - 1 {
                start_time.to_string()
            } else {
                "0".to_string()
            };
            fields.push(value);
        }
        format!("{}\n", fields.join(" "))
    }

    fn write_proc(root: &Path, pid: i32, comm: &str, ppid: i32, start_time: u64) {
        // This used to call `crate::files::publish_atomically`, which does not
        // exist here: `files.rs` is layer 3 and still lives in both actuators,
        // so the moved test could not keep reaching for it.
        //
        // The narrow, commented `#[allow]` at the exact call site is the shape
        // `clippy.toml` itself prescribes for a legitimate exception, and this
        // is one: the seam it defends is FILESYSTEM EFFECTS IN THE PRODUCT
        // belonging to the host executor inside a host transaction. A parser
        // fixture written into a throwaway `/proc`-shaped tempdir is not a
        // product effect, is not published anywhere, and has no atomicity
        // requirement to honour. The alternative — copying `files.rs` into the
        // leaf to keep a test's write "inside the owner module" — would
        // re-create the duplication this whole packet is deleting.
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).expect("fixture dir");
        #[allow(
            clippy::disallowed_methods,
            reason = "test fixture in a throwaway tempdir; the seam is product filesystem effects"
        )]
        std::fs::write(dir.join("stat"), stat_line(pid, comm, ppid, start_time))
            .expect("write fixture");
    }

    #[test]
    fn a_comm_containing_spaces_and_parentheses_does_not_shift_the_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_proc(dir.path(), 42, "pi (worker) x", 7, 987_654);
        let reader = ProcReader::with_root(dir.path());
        assert_eq!(reader.identity(Pid(42)).expect("identity").start_time, 987_654);
        assert_eq!(reader.parent(Pid(42)).expect("parent"), Some(Pid(7)));
    }

    #[test]
    fn start_time_distinguishes_a_recycled_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_proc(dir.path(), 42, "pi", 1, 111);
        let reader = ProcReader::with_root(dir.path());
        let before = reader.identity(Pid(42)).expect("identity");
        write_proc(dir.path(), 42, "pi", 1, 222);
        let after = reader.identity(Pid(42)).expect("identity");
        assert_eq!(before.pid, after.pid);
        assert_ne!(before, after, "same pid, different start time is a different process");
    }

    #[test]
    fn ancestry_walks_upward_and_stops_at_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_proc(dir.path(), 1, "init", 0, 1);
        write_proc(dir.path(), 10, "runtime", 1, 2);
        write_proc(dir.path(), 20, "pi", 10, 3);
        write_proc(dir.path(), 30, "shim", 20, 4);
        write_proc(dir.path(), 40, "stranger", 1, 5);
        let reader = ProcReader::with_root(dir.path());
        assert!(reader.descends_from(Pid(30), Pid(10)).expect("walk"));
        assert!(reader.descends_from(Pid(30), Pid(30)).expect("self"));
        assert!(!reader.descends_from(Pid(40), Pid(10)).expect("walk"));
        assert!(!reader.descends_from(Pid(10), Pid(30)).expect("downward is not ancestry"));
    }

    #[test]
    fn a_broken_chain_is_not_an_ancestry() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 20's parent 10 does not exist: the pane process is gone.
        write_proc(dir.path(), 20, "pi", 10, 3);
        write_proc(dir.path(), 30, "shim", 20, 4);
        let reader = ProcReader::with_root(dir.path());
        assert!(
            !reader.descends_from(Pid(30), Pid(1)).expect("walk"),
            "an unprovable ancestry is refused, never assumed"
        );
    }

    #[test]
    fn a_cyclic_proc_view_terminates() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_proc(dir.path(), 10, "a", 20, 1);
        write_proc(dir.path(), 20, "b", 10, 2);
        let reader = ProcReader::with_root(dir.path());
        assert!(!reader.descends_from(Pid(10), Pid(99)).expect("bounded walk"));
    }

    #[test]
    fn a_missing_process_is_an_error_not_a_silent_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reader = ProcReader::with_root(dir.path());
        assert!(!reader.is_alive(Pid(1234)));
        assert!(matches!(
            reader.identity(Pid(1234)),
            Err(HostErr::ToolUnavailable { tool: "proc", .. })
        ));
    }

    #[test]
    fn the_parser_agrees_with_the_real_kernel_for_this_process() {
        // The fixture tests above pin the parse; this one pins that the field
        // offsets match the kernel chiefd actually runs on.
        let reader = ProcReader::default();
        let me = Pid(std::process::id().try_into().expect("pid fits in i32"));
        let identity = reader.identity(me).expect("own identity");
        assert_eq!(identity.pid, me);
        assert!(identity.start_time > 0, "a live process has a start time");
        assert!(reader.is_alive(me));
        let parent = reader.parent(me).expect("own parent");
        if let Some(parent) = parent {
            assert!(reader.descends_from(me, parent).expect("walk"));
        }
    }
}
