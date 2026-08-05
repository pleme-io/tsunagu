//! Bounded subprocess execution — the primitive that keeps a loop alive.
//!
//! Every fleet component that shells out to something slow (nix, tofu,
//! kubectl, git) has the same three ways to hang forever, and std's
//! [`Command`] hands you all three by default. This module is the one place
//! they are solved, so a consumer gets a subprocess that *ends*.
//!
//! # The three hazards
//!
//! **1. `Command::output()` waits for pipe EOF, not for the child.** EOF
//! arrives when the last holder of the write end closes it. A child that
//! spawns *its own* children — `nix build` starting builders,
//! `darwin-rebuild` starting an activation — leaks the pipe to them, so if
//! any of them outlives the child, `output()` blocks after the child has
//! already exited and been zombified. There is no timeout in that path.
//!
//! **2. There is no deadline anywhere.** Not in `output()`, not in `wait()`.
//! A wedged child wedges the caller, permanently, with no signal that
//! differs from "still working".
//!
//! **3. Killing the pid is not killing the work.** A child put in its own
//! process group is a group *leader*; signalling only the leader orphans
//! precisely the descendants that caused hazard 1.
//!
//! # DIAGNOSED, not theorised
//!
//! cid, 2026-08-04: the darwin GitOps daemon sat in
//! `run_darwin_rebuild → Command::output → read_output → poll` for 22
//! minutes at 0% CPU with its direct child a zombie, publishing a heartbeat
//! byte-identical to a healthy slow build, and would have sat there
//! forever. Measured on the same reproduction — a child that leaves one
//! detached grandchild holding stdout:
//!
//! | capture | elapsed |
//! |---|---|
//! | pipe (what `output()` does) | 10.02 s |
//! | file (what this module does) | 0.01 s |
//!
//! # Why a file and not a pipe
//!
//! A file has no EOF contract to wait on. [`std::process::Child::wait`]
//! returns when the *direct* child exits, however many descendants still
//! hold the descriptor, and a grandchild appending afterwards is harmless.
//! Capturing (rather than inheriting) keeps the failure text, which matters
//! because the reason a nix build failed is the last thing it printed.
//!
//! Canonical doctrine: `theory/RECONCILER-LIVENESS.md` (P1).

use std::fs::File;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Marker embedded in the error when a run exceeded its deadline, so a hang
/// stays distinguishable from an ordinary non-zero exit.
pub const TIMEOUT_MARKER: &str = "timed out after";

/// Default cap on retained output, bytes. The tail is kept, not the head.
pub const DEFAULT_TAIL_BYTES: usize = 16 * 1024;

/// What a bounded run does to a child that outlived its deadline.
///
/// An enum rather than a `bool` because the choice is genuinely
/// consequential in both directions, and neither call site should read as
/// the other at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnTimeout {
    /// Kill the child's whole process group.
    ///
    /// Correct when the child has **mutated nothing** — a build, a plan, a
    /// dry-run, a query. Killing is free and leaves a clean slate.
    KillGroup,
    /// Stop waiting; leave the child running.
    ///
    /// Correct when the child may be **mid-mutation** — an apply, an
    /// activation, a migration. Killing it there is the damage, not the
    /// cure: a half-applied system is worse than a slow one. The caller is
    /// expected to re-observe on its next cycle and converge against
    /// whatever actually landed.
    Abandon,
}

/// How a bounded run is configured. Built with [`BoundedRun::new`].
#[derive(Debug, Clone)]
pub struct BoundedRun<'a> {
    capture_path: &'a Path,
    timeout: Option<Duration>,
    on_timeout: OnTimeout,
    tail_bytes: usize,
    poll_interval: Duration,
}

impl<'a> BoundedRun<'a> {
    /// A run capturing to `capture_path`, unbounded until you set a
    /// deadline.
    ///
    /// The capture path is a caller decision on purpose: a daemon wants it
    /// in its state dir (a crash leaves one inspectable artifact), a test
    /// wants it in a temp dir. It is consumed and removed on every exit
    /// path, including timeout.
    #[must_use]
    pub fn new(capture_path: &'a Path) -> Self {
        Self {
            capture_path,
            timeout: None,
            on_timeout: OnTimeout::KillGroup,
            tail_bytes: DEFAULT_TAIL_BYTES,
            poll_interval: Duration::from_secs(1),
        }
    }

    /// Bound the run. **Prefer generous over tight**: a deadline that kills
    /// real work trades a rare hang for a routine regression, and the bound
    /// only has to catch the pathological case.
    #[must_use]
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// What to do with a child that blew the deadline. See [`OnTimeout`] —
    /// the default is [`OnTimeout::KillGroup`], which is wrong for anything
    /// mid-mutation.
    #[must_use]
    pub fn on_timeout(mut self, o: OnTimeout) -> Self {
        self.on_timeout = o;
        self
    }

    /// Cap the retained output. The TAIL is kept: a failure's reason is at
    /// the end, after however many thousand lines of progress.
    #[must_use]
    pub fn tail_bytes(mut self, n: usize) -> Self {
        self.tail_bytes = n;
        self
    }

    /// How often to check for completion while waiting. Only meaningful
    /// with a timeout set; the default (1s) is nil against a multi-minute
    /// child and lets tests drop it.
    #[must_use]
    pub fn poll_interval(mut self, d: Duration) -> Self {
        self.poll_interval = d;
        self
    }

    /// Run `cmd` to completion under this bound.
    ///
    /// Returns an [`Output`] whose `stderr` holds the bounded tail of the
    /// child's **merged** stdout+stderr — merged because callers read that
    /// field for the failure message and most tools split diagnostics
    /// across both streams, so keeping them apart drops half the reason.
    /// `stdout` is always empty; use [`Output::status`] for the verdict.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::TimedOut`] (message contains
    /// [`TIMEOUT_MARKER`], followed by whatever the child managed to print)
    /// when the deadline is blown, or the underlying error if the capture
    /// file cannot be created or the child cannot be spawned.
    ///
    /// # Detached children
    ///
    /// If the child needs to outlive this process (an activation that
    /// restarts your own supervisor unit), set `process_group(0)` on `cmd`
    /// **before** calling this. That is exactly what makes hazard 1 live, so
    /// pair it with a timeout — see `theory/RECONCILER-LIVENESS.md` §IV:
    /// self-delivery without a bound is a hang generator.
    pub fn run(&self, mut cmd: Command) -> std::io::Result<Output> {
        let f = File::create(self.capture_path)?;
        let g = f.try_clone()?;
        cmd.stdout(Stdio::from(f));
        cmd.stderr(Stdio::from(g));
        // Never inherit the caller's stdin: a child that reads it blocks on
        // a descriptor nobody will ever write to.
        cmd.stdin(Stdio::null());

        let mut child = cmd.spawn()?;

        let Some(timeout) = self.timeout else {
            let status = child.wait()?;
            return Ok(self.finish(status));
        };

        // `try_wait` polling rather than a wait-with-timeout syscall: std
        // has no portable one, and this keeps the module free of `unsafe`
        // and of a second thread whose own failure modes would need
        // bounding too.
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(self.finish(status));
            }
            if Instant::now() >= deadline {
                if self.on_timeout == OnTimeout::KillGroup {
                    kill_process_group(&child);
                    // Reap, so the child does not linger as a zombie — the
                    // very symptom that made the original hang confusing.
                    let _ = child.wait();
                }
                let tail = String::from_utf8_lossy(&self.take_capture()).into_owned();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    [
                        "run ",
                        TIMEOUT_MARKER,
                        " ",
                        &timeout.as_secs().to_string(),
                        "s\n",
                        &tail,
                    ]
                    .concat(),
                ));
            }
            std::thread::sleep(self.poll_interval);
        }
    }

    fn finish(&self, status: std::process::ExitStatus) -> Output {
        Output {
            status,
            stdout: Vec::new(),
            stderr: self.take_capture(),
        }
    }

    /// Read the capture and remove it. Best-effort: a run that produced no
    /// output, or a state dir that went read-only, must not turn a
    /// successful child into an error.
    fn take_capture(&self) -> Vec<u8> {
        let bytes = std::fs::read(self.capture_path).unwrap_or_default();
        let _ = std::fs::remove_file(self.capture_path);
        tail_bytes(bytes, self.tail_bytes)
    }
}

/// Kill a child's whole process group.
///
/// The **group**, not the pid: a child spawned with `process_group(0)` is a
/// group leader whose pgid equals its pid, and signalling only the leader
/// orphans exactly the descendants whose survival causes the pipe hang this
/// module exists to prevent. `SIGKILL` rather than `SIGTERM` because this is
/// reached only after the child ignored its entire deadline.
///
/// Best-effort: the realistic error is `ESRCH` (the group already exited
/// between the deadline check and here), and there is nothing useful to do
/// about any other errno while abandoning a hung child anyway.
///
/// Uses rustix's safe wrapper rather than `libc::killpg` in an `unsafe`
/// block, so a consumer under `#![forbid(unsafe_code)]` can take this crate.
pub fn kill_process_group(child: &std::process::Child) {
    let Ok(raw) = i32::try_from(child.id()) else {
        return;
    };
    if let Some(pid) = rustix::process::Pid::from_raw(raw) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
    }
}

/// Keep at most `max` bytes from the END of `v`, cutting on a UTF-8
/// character boundary.
///
/// The tail, because a failure's reason is the last thing printed. The
/// boundary walk, because otherwise a lossy decode opens with U+FFFD.
#[must_use]
pub fn tail_bytes(mut v: Vec<u8>, max: usize) -> Vec<u8> {
    if v.len() > max {
        let mut cut = v.len() - max;
        while cut < v.len() && (v[cut] & 0xC0) == 0x80 {
            cut += 1;
        }
        v = v.split_off(cut);
    }
    v
}

/// Whether an error came from a blown deadline rather than a failed child.
///
/// Checks the kind first and the marker second: the kind is the contract,
/// the marker survives an error that has been stringified through a layer
/// that dropped it (which is what happens when this reaches a receipt).
#[must_use]
pub fn is_timeout(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut || e.to_string().contains(TIMEOUT_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as _;
    use std::sync::Mutex;

    /// Serializes the forking tests against each other and against anything
    /// that holds a lock.
    ///
    /// `fork()` duplicates the whole descriptor table, so between fork and
    /// exec a child owns a copy of any flock this process holds — CLOEXEC
    /// acts at exec, not at fork. A concurrent test can therefore observe a
    /// released lock as still held. MEASURED in sentinela, where adding one
    /// forking test took a suite from 0/8 to 1/8 failing runs.
    static FORK_GUARD: Mutex<()> = Mutex::new(());

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tsunagu-exec-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_leaked_grandchild_cannot_hang_the_run() {
        // The 2026-08-04 deadlock in miniature: `/bin/sh` exits at once but
        // leaves a `sleep` holding the inherited stdout — what a detached
        // activation does. `Command::output()` blocks for the full sleep
        // here; a file capture returns immediately.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("leak");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 10 & echo started");
        cmd.process_group(0);

        let t0 = Instant::now();
        let out = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(60))
            .run(cmd)
            .expect("must not error");
        let elapsed = t0.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "a leaked grandchild must not hold the run open — took {elapsed:?}"
        );
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("started"),
            "output must still be captured, not merely un-blocked"
        );
        assert!(!cap.exists(), "capture is consumed and cleaned up");
    }

    #[test]
    fn a_hung_run_hits_its_deadline_and_the_whole_group_dies() {
        // Proves the GROUP dies, not just the leader: the grandchild
        // announces survival by touching a marker after the deadline. A
        // naive kill of the direct child would pass a timing-only
        // assertion and fail this one.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("timeout");
        let marker = tmp("alive-marker");
        let _ = std::fs::remove_file(&marker);

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(
            ["(sleep 4; touch '", &marker.display().to_string(), "') & sleep 300"].concat(),
        );
        cmd.process_group(0);

        let t0 = Instant::now();
        let err = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(2))
            .poll_interval(Duration::from_millis(100))
            .run(cmd)
            .expect_err("a run past its deadline must error, never hang");
        let elapsed = t0.elapsed();

        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(is_timeout(&err), "a hang must stay distinguishable: {err}");
        assert!(
            elapsed < Duration::from_secs(20),
            "must return at its deadline, not the child's lifetime — {elapsed:?}"
        );
        assert!(!cap.exists(), "capture cleaned up on timeout too");

        std::thread::sleep(Duration::from_secs(6));
        let survived = marker.exists();
        let _ = std::fs::remove_file(&marker);
        assert!(
            !survived,
            "KillGroup must kill the GROUP — a surviving grandchild is the \
             orphaning that causes the pipe hang in the first place"
        );
    }

    #[test]
    fn abandon_leaves_the_child_running() {
        // The apply case: we stop waiting, the work continues. Asserted
        // positively, because "we did not kill it" is the whole contract
        // for a mid-mutation child.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("abandon");
        let marker = tmp("abandon-marker");
        let _ = std::fs::remove_file(&marker);

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(["sleep 3; touch '", &marker.display().to_string(), "'"].concat());
        cmd.process_group(0);

        let err = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(1))
            .poll_interval(Duration::from_millis(50))
            .on_timeout(OnTimeout::Abandon)
            .run(cmd)
            .expect_err("still reports the deadline");
        assert!(is_timeout(&err));

        std::thread::sleep(Duration::from_secs(5));
        let finished = marker.exists();
        let _ = std::fs::remove_file(&marker);
        assert!(
            finished,
            "Abandon must let a mid-mutation child finish — killing it is \
             how a machine ends up half-applied"
        );
    }

    #[test]
    fn a_child_that_beats_its_deadline_is_never_reported_as_timed_out() {
        // `try_wait` is checked BEFORE the clock, so a finished process is
        // reaped rather than misreported. Guards the off-by-one that would
        // make a tight deadline lie about fast work.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("quick");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("echo quick");
        let out = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(30))
            .run(cmd)
            .expect("a fast command must succeed");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("quick"));
    }

    #[test]
    fn a_failing_child_keeps_its_exit_status_and_its_reason() {
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("fail");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("echo 'the reason' >&2; exit 3");
        let out = BoundedRun::new(&cap).run(cmd).expect("not an error");
        assert_eq!(out.status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("the reason"),
            "stderr must survive the merge"
        );
    }

    #[test]
    fn a_capture_is_bounded_to_its_tail_on_a_char_boundary() {
        let big = "é".repeat(40 * 1024).into_bytes();
        let kept = tail_bytes(big.clone(), 16 * 1024);
        assert!(kept.len() <= 16 * 1024);
        assert!(big.ends_with(&kept), "must keep the TAIL, never the head");
        assert!(
            !String::from_utf8_lossy(&kept).starts_with('\u{FFFD}'),
            "must cut on a char boundary"
        );
    }

    #[test]
    fn a_capture_under_the_bound_is_untouched() {
        let small = b"error: flake.lock parse error".to_vec();
        assert_eq!(tail_bytes(small.clone(), 16 * 1024), small);
    }
}
