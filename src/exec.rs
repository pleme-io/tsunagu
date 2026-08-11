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

/// Marker for the SILENCE bound, distinct from [`TIMEOUT_MARKER`] because the
/// two failures want different operator responses.
///
/// A blown total deadline says "this took too long" — maybe legitimately.
/// A blown silence bound says "this produced NOTHING for N seconds", which is
/// the signature of a wedge rather than of slow work, and it is actionable
/// immediately.
pub const SILENCE_MARKER: &str = "silent for";

/// Marker for the ERROR-THEN-QUIET bound — the wedge signal that does not
/// wait out a generous clock.
///
/// [`SILENCE_MARKER`] answers "is it stuck?" with a timer, and a timer must be
/// generous or it kills honest slow work: a single long compile inside
/// `nix build` legitimately prints nothing for many minutes. So silence alone
/// can only ever notice a wedge LATE.
///
/// A failure that has already been REPORTED is a stronger signal than quiet.
/// Once the child has printed its own error text, there is nothing left for it
/// to succeed at, and continued silence means the process tree is not winding
/// down — it is stuck. Measured on cid 2026-08-11: `nix build` errored and
/// exited while a `jq` downstream of it held for 89 more minutes waiting on an
/// EOF that nothing would send. The error was on disk within seconds; the
/// daemon spent 5400s not reading it.
///
/// Requiring BOTH the marker and a quiet period is what keeps this from being
/// worse than the hang it replaces. A build that prints `error:` and keeps
/// working — `nix --keep-going`, a compiler emitting diagnostics, a test suite
/// logging an expected failure — keeps resetting the quiet window and is never
/// killed. Only "reported a failure, then stopped doing anything" trips it.
pub const ERROR_WEDGE_MARKER: &str = "reported an error then went quiet for";

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

/// How a bounded run treats the child's output.
///
/// ── ★ WHY THIS IS A CHOICE AND NOT A FIXED POLICY ────────────────────────
/// v0.1.4 shipped [`Capture::Merged`] only, because the first consumer
/// (sentinela) wanted one thing: the reason a build failed. A six-repo audit
/// then showed that contract excludes most of the fleet — `Output.stdout`
/// always empty is fatal wherever stdout is DATA rather than diagnostics:
///
///   * a store path from `nix build --print-out-paths`
///   * `nix show-config --json` (tail truncation alone guarantees a parse error)
///   * `nix path-info --all | lines().count()` — a tail gives a SILENTLY WRONG
///     number, which is worse than the hang it replaced
///   * a byte-parity harness comparing two stdouts — with both empty, every
///     probe compares equal and the suite reports 100% parity. A green lie.
///
/// Merged stays the default so existing callers are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// stdout+stderr into ONE file; the tail lands in `Output.stderr` and
    /// `Output.stdout` is empty. Right when you only want the failure reason
    /// and the child interleaves diagnostics across both streams.
    Merged,
    /// stdout and stderr into SEPARATE files, each independently
    /// tail-bounded, BOTH populated on return. Right whenever stdout is a
    /// value the caller parses.
    Separate,
    /// No capture at all — the child inherits the caller's stdio. Right for
    /// a live CI log, an interactive `sudo` prompt, or a streaming build,
    /// where capturing would break the thing the operator is watching.
    /// `Output.stdout` and `Output.stderr` are both empty; the deadline and
    /// the group-kill still apply. This is the "bound it but do not touch
    /// its output" shape.
    Inherit,
}

#[derive(Debug, Clone)]
pub struct BoundedRun<'a> {
    capture_path: &'a Path,
    timeout: Option<Duration>,
    on_timeout: OnTimeout,
    tail_bytes: usize,
    poll_interval: Duration,
    capture: Capture,
    silent_after: Option<Duration>,
    error_wedge: Option<(String, Duration)>,
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
            capture: Capture::Merged,
            silent_after: None,
            error_wedge: None,
        }
    }

    /// Fail the run when the capture contains `marker` AND has not grown for
    /// `quiet` — "it told us it failed, then stopped" — without waiting for
    /// the total deadline or the (necessarily generous) silence bound.
    ///
    /// See [`ERROR_WEDGE_MARKER`] for why both conditions are required: the
    /// marker alone would kill any build whose output merely CONTAINS the
    /// word, and quiet alone must be generous enough to miss the wedge for
    /// half an hour. Together they are specific to the shape that actually
    /// occurs.
    ///
    /// `quiet` can therefore be short — the child has already said it failed,
    /// so the only question is whether its tree is still winding down.
    ///
    /// Inert under [`Capture::Inherit`], which has no capture to scan; the
    /// builder accepts the call so a caller need not branch on capture mode.
    #[must_use]
    pub fn error_wedge(mut self, marker: impl Into<String>, quiet: Duration) -> Self {
        self.error_wedge = Some((marker.into(), quiet));
        self
    }

    /// Fail the run when the child produces NO OUTPUT for `d`, even though
    /// its total deadline has not expired.
    ///
    /// ── ★ WHY A TOTAL DEADLINE IS NOT ENOUGH ────────────────────────────
    /// A total deadline has to be sized for the LONGEST LEGITIMATE run, and
    /// on this fleet that is very long: sentinela allows 5400s for a build
    /// because a cold rebuild genuinely takes 90 minutes. A process that
    /// wedges at minute two therefore burns the full 88 remaining minutes
    /// before anything notices, and the operator watching it cannot tell a
    /// wedge from slow progress — both look like silence.
    ///
    /// MEASURED on rio 2026-08-07: a nix build sat at ZERO CPU for over
    /// half an hour holding two CLOSE-WAIT HTTPS sockets with unread bytes,
    /// well inside its total budget. It emitted nothing the entire time. A
    /// silence bound of a few minutes would have converted 90 minutes of
    /// ambiguity into one typed failure naming the symptom.
    ///
    /// The clock RESETS on every byte written, so a slow-but-progressing
    /// build is never killed — which is what makes this safe to set much
    /// tighter than the total deadline.
    ///
    /// ── INERT UNDER [`Capture::Inherit`], AND SAID OUT LOUD ─────────────
    /// Progress is measured by the growth of the capture file. Under
    /// `Inherit` there is no capture file — the child writes straight to
    /// the caller's stdio — so there is nothing to measure and this bound
    /// CANNOT fire. It is deliberately inert rather than approximated: a
    /// silence bound that guessed would kill live interactive work. If you
    /// need both a live log and a silence bound, capture and tee.
    ///
    /// TIER: only-mitigated. It observes the child's OUTPUT, not its
    /// progress; a process that prints a heartbeat while doing nothing
    /// defeats it, and nothing here can tell those apart.
    #[must_use]
    pub fn silent_after(mut self, d: Duration) -> Self {
        self.silent_after = Some(d);
        self
    }

    /// Populate BOTH `Output.stdout` and `Output.stderr`, each independently
    /// tail-bounded. Use this whenever stdout is a value you parse.
    ///
    /// stderr is captured beside `capture_path` with a `.err` extension, so
    /// one caller-supplied path still describes the whole run.
    #[must_use]
    pub fn separate_streams(mut self) -> Self {
        self.capture = Capture::Separate;
        self
    }

    /// Do not capture: the child inherits the caller's stdio, and only the
    /// deadline + group-kill apply.
    ///
    /// Both `Output` byte fields come back empty — that is the contract, not
    /// a failure. Reach for this when capturing would break the point of the
    /// command: a live CI log, an interactive prompt, a streaming build.
    #[must_use]
    pub fn inherit_stdio(mut self) -> Self {
        self.capture = Capture::Inherit;
        self
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
        match self.capture {
            Capture::Merged => {
                let f = File::create(self.capture_path)?;
                let g = f.try_clone()?;
                cmd.stdout(Stdio::from(f));
                cmd.stderr(Stdio::from(g));
            }
            Capture::Separate => {
                cmd.stdout(Stdio::from(File::create(self.capture_path)?));
                cmd.stderr(Stdio::from(File::create(self.err_path())?));
            }
            // Inherit: touch neither, so the child writes where we do.
            Capture::Inherit => {}
        }
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
        // Silence tracking. `Inherit` writes to the caller's stdio, so there
        // is no file whose growth could stand in for progress — the bound is
        // inert there by construction rather than by a guess.
        let watch_silence = self.silent_after.is_some() && self.capture != Capture::Inherit;
        // Same capture precondition as the silence watch: with no capture
        // there is nothing to scan, so the feature is inert rather than
        // wrong.
        let watch_error = self.error_wedge.is_some() && self.capture != Capture::Inherit;
        let mut saw_error = false;
        // The scan is driven by growth, so it needs one unconditional pass:
        // a child that writes its error before the first poll would otherwise
        // never be scanned at all.
        let mut scanned_once = false;
        let mut last_size = self.captured_len();
        let mut last_growth = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(self.finish(status));
            }
            if watch_silence || watch_error {
                let now_size = self.captured_len();
                let grew = now_size != last_size;
                if grew {
                    last_size = now_size;
                    last_growth = Instant::now();
                }
                // Rescan on growth, plus one unconditional first pass.
                // `saw_error` is sticky: an error already printed keeps
                // counting even if later output would push it out of view.
                if watch_error && !saw_error && (grew || !scanned_once) {
                    scanned_once = true;
                    if let Some((marker, _)) = &self.error_wedge {
                        saw_error = String::from_utf8_lossy(&self.capture_bytes())
                            .contains(marker.as_str());
                    }
                }
                if saw_error {
                    if let Some((_, quiet)) = &self.error_wedge {
                        if last_growth.elapsed() >= *quiet {
                            if self.on_timeout == OnTimeout::KillGroup {
                                kill_process_group(&child);
                                let _ = child.wait();
                            }
                            let tail =
                                String::from_utf8_lossy(&self.take_capture()).into_owned();
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                [
                                    "run ",
                                    ERROR_WEDGE_MARKER,
                                    " ",
                                    &quiet.as_secs().to_string(),
                                    "s — the failure is already reported below; \
                                     the tree stopped winding down\n",
                                    &tail,
                                ]
                                .concat(),
                            ));
                        }
                    }
                }
                if let Some(quiet) = self.silent_after {
                    if last_growth.elapsed() >= quiet {
                        if self.on_timeout == OnTimeout::KillGroup {
                            kill_process_group(&child);
                            let _ = child.wait();
                        }
                        let tail = String::from_utf8_lossy(&self.take_capture()).into_owned();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            [
                                "run ",
                                SILENCE_MARKER,
                                " ",
                                &quiet.as_secs().to_string(),
                                "s (total deadline not reached) — no output; \
                                 a wedge, not slow work\n",
                                &tail,
                            ]
                            .concat(),
                        ));
                    }
                }
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

    /// Where stderr lands under [`Capture::Separate`].
    /// Total bytes written to the capture file(s) so far — the progress
    /// proxy for [`Self::silent_after`]. A missing file reads as 0 rather
    /// than an error: the child may not have written yet.
    fn captured_len(&self) -> u64 {
        let one = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        match self.capture {
            Capture::Merged => one(self.capture_path),
            Capture::Separate => one(self.capture_path) + one(&self.err_path()),
            Capture::Inherit => 0,
        }
    }

    /// Whole capture as bytes, non-destructively (unlike `take_capture`).
    /// Used only by the error-wedge scan, which must leave the file intact
    /// for the eventual `finish`/`take_capture`.
    fn capture_bytes(&self) -> Vec<u8> {
        let one = |p: &std::path::Path| std::fs::read(p).unwrap_or_default();
        match self.capture {
            Capture::Merged => one(self.capture_path),
            Capture::Separate => {
                let mut v = one(self.capture_path);
                v.extend_from_slice(&one(&self.err_path()));
                v
            }
            Capture::Inherit => Vec::new(),
        }
    }

    fn err_path(&self) -> std::path::PathBuf {
        let mut p = self.capture_path.to_path_buf();
        let ext = match p.extension().and_then(|e| e.to_str()) {
            Some(e) => [e, ".err"].concat(),
            None => "err".to_owned(),
        };
        p.set_extension(ext);
        p
    }

    fn finish(&self, status: std::process::ExitStatus) -> Output {
        match self.capture {
            Capture::Merged => Output {
                status,
                stdout: Vec::new(),
                stderr: self.take_capture(),
            },
            Capture::Separate => Output {
                status,
                stdout: take_tail(self.capture_path, self.tail_bytes),
                stderr: take_tail(&self.err_path(), self.tail_bytes),
            },
            Capture::Inherit => Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        }
    }

    /// Read the capture and remove it. Best-effort: a run that produced no
    /// output, or a state dir that went read-only, must not turn a
    /// successful child into an error.
    fn take_capture(&self) -> Vec<u8> {
        take_tail(self.capture_path, self.tail_bytes)
    }
}

/// Read a capture file, remove it, and keep its bounded tail.
///
/// Best-effort: a run that produced no output, or a state dir that went
/// read-only, must not turn a successful child into an error.
fn take_tail(path: &Path, max: usize) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_default();
    let _ = std::fs::remove_file(path);
    tail_bytes(bytes, max)
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
    fn error_then_quiet_is_a_wedge_and_does_not_wait_out_the_clock() {
        // The cid 2026-08-11 shape, minimised: the work reports a failure and
        // exits, a downstream holder keeps the tree alive doing nothing. The
        // run must end on the error+quiet signal, NOT on the 60s deadline.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("errwedge");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("echo 'error: builder failed'; sleep 60");
        // Required for the group kill to reach the `sleep`; without it the
        // guard still fires on time but `child.wait()` blocks until the sleep
        // ends, which is what this test's elapsed-time assertion caught.
        cmd.process_group(0);

        let started = Instant::now();
        let err = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(60))
            .error_wedge("error:", Duration::from_millis(300))
            .poll_interval(Duration::from_millis(50))
            .run(cmd)
            .expect_err("an error followed by quiet must fail the run");

        assert!(
            format!("{err}").contains(ERROR_WEDGE_MARKER),
            "must be reported as an error-wedge, not a plain timeout: {err}"
        );
        assert!(
            format!("{err}").contains("error: builder failed"),
            "the reported failure must be carried in the tail: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "must not wait out the deadline: took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn error_followed_by_progress_is_never_killed() {
        // The false-positive this guard must not have. `nix --keep-going`, a
        // compiler emitting diagnostics, a suite logging an expected failure:
        // output CONTAINS the marker and the work is healthy. Each new line
        // resets the quiet window, so the run completes normally.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("errprogress");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("echo 'error: one target failed'; for i in 1 2 3 4 5 6; do sleep 0.1; echo still working; done; exit 0");

        let out = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(30))
            .error_wedge("error:", Duration::from_millis(300))
            .poll_interval(Duration::from_millis(50))
            .run(cmd)
            .expect("a build that keeps making progress must NOT be killed");

        assert!(out.status.success(), "must exit normally: {:?}", out.status);
    }

    #[test]
    fn quiet_without_an_error_is_not_a_wedge() {
        // The other half of the specificity claim: silence alone must not
        // trip THIS guard, or it collapses into `silent_after` and inherits
        // its need to be generous. A long quiet stretch with no reported
        // failure is ordinary slow work.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("quietok");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("echo compiling; sleep 1; echo done; exit 0");

        let out = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(30))
            .error_wedge("error:", Duration::from_millis(200))
            .poll_interval(Duration::from_millis(50))
            .run(cmd)
            .expect("silence without a reported error must not be killed");

        assert!(out.status.success(), "must exit normally: {:?}", out.status);
    }

    #[test]
    fn separate_streams_populates_both_and_never_merges() {
        // The contract five repos need: stdout is a VALUE, not diagnostics.
        // Merged capture would make `nix build --print-out-paths`, a JSON
        // config read, and a byte-parity harness all wrong — the last one
        // silently, by comparing empty to empty and reporting 100% parity.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("sep");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("echo /nix/store/abc-value; echo noise >&2");

        let out = BoundedRun::new(&cap)
            .separate_streams()
            .timeout(Duration::from_secs(30))
            .run(cmd)
            .expect("must not error");

        let so = String::from_utf8_lossy(&out.stdout);
        let se = String::from_utf8_lossy(&out.stderr);
        assert!(so.contains("/nix/store/abc-value"), "stdout must be POPULATED: {so:?}");
        assert!(!so.contains("noise"), "stderr must NOT leak into stdout: {so:?}");
        assert!(se.contains("noise"), "stderr must be populated: {se:?}");
        assert!(
            !se.contains("/nix/store/abc-value"),
            "stdout must NOT leak into stderr: {se:?}"
        );
        assert!(!cap.exists(), "stdout capture cleaned up");
        assert!(
            !std::path::Path::new(&format!("{}.err", cap.display())).exists(),
            "stderr capture cleaned up too"
        );
    }

    #[test]
    fn inherit_stdio_bounds_without_capturing() {
        // The live-log / interactive shape: a deadline and a group-kill, but
        // the child's output goes where the caller's does. Empty byte fields
        // are the CONTRACT here, not a failure.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("inherit");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("exit 7");

        let out = BoundedRun::new(&cap)
            .inherit_stdio()
            .timeout(Duration::from_secs(30))
            .run(cmd)
            .expect("must not error");

        assert_eq!(out.status.code(), Some(7), "status still reported");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "no capture");
        assert!(!cap.exists(), "no capture file is created at all");
    }

    #[test]
    fn inherit_stdio_still_enforces_the_deadline() {
        // The bound must not be a side effect of capturing.
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("inherit-timeout");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 300");
        cmd.process_group(0);

        let t0 = Instant::now();
        let err = BoundedRun::new(&cap)
            .inherit_stdio()
            .timeout(Duration::from_secs(1))
            .poll_interval(Duration::from_millis(50))
            .run(cmd)
            .expect_err("deadline applies without capture");
        assert!(is_timeout(&err));
        assert!(t0.elapsed() < Duration::from_secs(15));
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

    /// The wedge signature: a child that produces nothing, well inside its
    /// total deadline. This is the rio 2026-08-07 shape — zero CPU, two
    /// CLOSE-WAIT sockets, 88 minutes of budget still on the clock.
    #[test]
    fn silence_fires_long_before_the_total_deadline() {
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("silence-fires");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 300");
        cmd.process_group(0);

        let t0 = Instant::now();
        let err = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(120))
            .silent_after(Duration::from_secs(1))
            .poll_interval(Duration::from_millis(100))
            .run(cmd)
            .expect_err("a silent child must error, not run to its deadline");
        let elapsed = t0.elapsed();

        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            format!("{err}").contains(SILENCE_MARKER),
            "the failure must name SILENCE, not the total deadline — the two \
             want different operator responses: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(30),
            "silence must fire on its own clock, not the 120s deadline; took {elapsed:?}"
        );
    }

    /// The property that makes a tight silence bound SAFE: output resets the
    /// clock, so slow-but-progressing work is never killed. Without this the
    /// bound would be unusable and every caller would turn it off — the
    /// failure mode that kills a guard.
    #[test]
    fn output_resets_the_silence_clock() {
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("silence-resets");

        // Prints every 200ms for ~2s, against a 1s silence bound: never
        // quiet for a whole second, so it must be allowed to finish.
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("i=0; while [ $i -lt 10 ]; do echo tick; sleep 0.2; i=$((i+1)); done");
        cmd.process_group(0);

        let out = BoundedRun::new(&cap)
            .timeout(Duration::from_secs(60))
            .silent_after(Duration::from_secs(1))
            .poll_interval(Duration::from_millis(100))
            .run(cmd)
            .expect("a child that keeps printing must NEVER trip the silence bound");
        assert!(out.status.success());
    }

    /// Under `Inherit` there is no capture file, so silence cannot be
    /// measured. It must be INERT rather than approximated — a bound that
    /// guessed here would kill live interactive work (a sudo prompt, a
    /// streaming build) for the crime of being quiet.
    #[test]
    fn silence_is_inert_under_inherit_rather_than_guessing() {
        let _serial = FORK_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let cap = tmp("silence-inherit");

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 2");
        cmd.process_group(0);

        let out = BoundedRun::new(&cap)
            .inherit_stdio()
            .timeout(Duration::from_secs(60))
            .silent_after(Duration::from_millis(200))
            .poll_interval(Duration::from_millis(50))
            .run(cmd)
            .expect("Inherit has no capture file to measure; the bound must not fire");
        assert!(out.status.success());
    }
}
