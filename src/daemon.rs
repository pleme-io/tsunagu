use crate::error::TsunaguError;
use crate::socket::SocketPath;
use std::path::{Path, PathBuf};

/// Abstraction for checking whether a process is alive.
///
/// The default implementation ([`SystemProcessChecker`]) probes `/proc` on
/// Linux and falls back to `ps -p` elsewhere.  Consumers can supply a
/// custom implementation (or [`MockProcessChecker`] in tests) to
/// [`DaemonProcess::with_checker`] to avoid real I/O in unit tests.
pub trait ProcessChecker: Send + Sync {
    /// Return `true` when the process identified by `pid` is alive.
    fn is_alive(&self, pid: u32) -> bool;
}

/// [`ProcessChecker`] that queries the operating system.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessChecker;

impl ProcessChecker for SystemProcessChecker {
    fn is_alive(&self, pid: u32) -> bool {
        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        if proc_path.exists() {
            return true;
        }

        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// A test double that always returns a fixed answer.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub struct MockProcessChecker {
    pub alive: bool,
}

#[cfg(test)]
impl ProcessChecker for MockProcessChecker {
    fn is_alive(&self, _pid: u32) -> bool {
        self.alive
    }
}

/// Daemon process lifecycle management.
///
/// Handles PID file creation, staleness detection, and graceful cleanup.
/// The PID file and socket are automatically removed on drop.
///
/// Generic over [`ProcessChecker`]; defaults to [`SystemProcessChecker`].
pub struct DaemonProcess<C: ProcessChecker = SystemProcessChecker> {
    app_name: String,
    pid_path: PathBuf,
    socket_path: PathBuf,
    checker: C,
}

/// Constructors that use the real operating-system process checker.
impl DaemonProcess {
    /// Create a new daemon process manager for the given application.
    #[must_use]
    pub fn new(app_name: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            pid_path: SocketPath::pid_file(app_name),
            socket_path: SocketPath::for_app(app_name),
            checker: SystemProcessChecker,
        }
    }

    /// Create a daemon process with custom paths (useful for testing).
    #[must_use]
    pub fn with_paths(app_name: &str, pid_path: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            app_name: app_name.to_string(),
            pid_path,
            socket_path,
            checker: SystemProcessChecker,
        }
    }
}

impl<C: ProcessChecker> DaemonProcess<C> {
    /// Create a daemon process with custom paths and a custom [`ProcessChecker`].
    #[must_use]
    pub fn with_checker(
        app_name: &str,
        pid_path: PathBuf,
        socket_path: PathBuf,
        checker: C,
    ) -> Self {
        Self {
            app_name: app_name.to_string(),
            pid_path,
            socket_path,
            checker,
        }
    }

    /// Check if a daemon is already running (PID file exists and process is alive).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.read_pid().is_some_and(|pid| self.checker.is_alive(pid))
    }

    /// Read the PID from the PID file, if it exists and is valid.
    #[must_use]
    pub fn read_pid(&self) -> Option<u32> {
        let contents = std::fs::read_to_string(&self.pid_path).ok()?;
        contents.trim().parse::<u32>().ok()
    }

    /// Write PID file for the current process.
    ///
    /// Creates parent directories if needed.
    #[must_use = "PID write may fail; handle the error"]
    pub fn write_pid(&self) -> Result<(), TsunaguError> {
        if let Some(parent) = self.pid_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.pid_path, std::process::id().to_string())?;
        Ok(())
    }

    /// Acquire the daemon lock: write PID if not already running.
    ///
    /// Returns `Err(DaemonAlreadyRunning)` if another instance is alive.
    #[must_use = "acquire may fail if daemon is already running"]
    pub fn acquire(&self) -> Result<(), TsunaguError> {
        if let Some(pid) = self.read_pid() {
            if self.checker.is_alive(pid) {
                return Err(TsunaguError::DaemonAlreadyRunning { pid });
            }
            tracing::warn!(pid, "removing stale PID file");
            let _ = std::fs::remove_file(&self.pid_path);
        }
        self.write_pid()
    }

    /// Clean up PID file and socket.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.pid_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }

    /// Path to the Unix socket.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Path to the PID file.
    #[must_use]
    pub fn pid_path(&self) -> &Path {
        &self.pid_path
    }

    /// Application name.
    #[must_use]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

impl<C: ProcessChecker> Drop for DaemonProcess<C> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl<C: ProcessChecker> std::fmt::Display for DaemonProcess<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DaemonProcess({}, pid={})",
            self.app_name,
            self.pid_path.display(),
        )
    }
}

/// Check if a process with the given PID is alive using `kill(0)`.
///
/// Unlike [`SystemProcessChecker`] which relies on `ps` (and may fail in
/// sandboxed macOS environments missing the `com.apple.system-task-ports`
/// entitlement), this function uses the POSIX `kill(pid, 0)` syscall which
/// only requires the caller to have permission to signal the process.
#[cfg(test)]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if we *can* signal the process without actually
    // sending a signal.  Returns 0 on success, -1 on error.  ESRCH means
    // the process doesn't exist; EPERM means it exists but we lack
    // permission — still alive in that case.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM → process exists but we can't signal it → alive
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A mock [`ProcessChecker`] that uses `kill(0)` — works in sandboxed macOS
/// where `ps -p` lacks entitlements.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct KillZeroChecker;

#[cfg(test)]
impl ProcessChecker for KillZeroChecker {
    fn is_alive(&self, pid: u32) -> bool {
        process_alive(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_daemon(dir: &TempDir) -> DaemonProcess<MockProcessChecker> {
        DaemonProcess::with_checker(
            "test-app",
            dir.path().join("test.pid"),
            dir.path().join("test.sock"),
            MockProcessChecker { alive: false },
        )
    }

    fn test_daemon_alive(dir: &TempDir) -> DaemonProcess<MockProcessChecker> {
        DaemonProcess::with_checker(
            "test-app",
            dir.path().join("test.pid"),
            dir.path().join("test.sock"),
            MockProcessChecker { alive: true },
        )
    }

    /// Helper: daemon using `kill(0)` for real process detection (sandbox-safe).
    fn test_daemon_real(dir: &TempDir) -> DaemonProcess<KillZeroChecker> {
        DaemonProcess::with_checker(
            "test-app",
            dir.path().join("test.pid"),
            dir.path().join("test.sock"),
            KillZeroChecker,
        )
    }

    // ----------------------------------------------------------------
    // Basic construction
    // ----------------------------------------------------------------

    #[test]
    fn new_daemon_not_running() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        assert!(!d.is_running());
    }

    #[test]
    fn new_uses_socket_path_defaults() {
        let d = DaemonProcess::new("tsunagu-test-new");
        let expected_pid = SocketPath::pid_file("tsunagu-test-new");
        let expected_sock = SocketPath::for_app("tsunagu-test-new");
        assert_eq!(*d.pid_path(), expected_pid);
        assert_eq!(*d.socket_path(), expected_sock);
    }

    #[test]
    fn with_paths_uses_custom_paths() {
        let pid = PathBuf::from("/custom/dir/app.pid");
        let sock = PathBuf::from("/custom/dir/app.sock");
        let d = DaemonProcess::with_paths("custom", pid.clone(), sock.clone());
        assert_eq!(*d.pid_path(), pid);
        assert_eq!(*d.socket_path(), sock);
        assert_eq!(d.app_name(), "custom");
    }

    #[test]
    fn with_paths_preserves_exact_paths() {
        let pid = PathBuf::from("/tmp/special-dir/my.pid");
        let sock = PathBuf::from("/var/run/my.sock");
        let d = DaemonProcess::with_paths("exact", pid, sock);
        assert_eq!(d.pid_path().to_str().unwrap(), "/tmp/special-dir/my.pid");
        assert_eq!(d.socket_path().to_str().unwrap(), "/var/run/my.sock");
    }

    #[test]
    fn app_name_is_stored() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        assert_eq!(d.app_name(), "test-app");
    }

    // ----------------------------------------------------------------
    // PID file write / read
    // ----------------------------------------------------------------

    #[test]
    fn write_pid_creates_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        assert!(d.pid_path().exists());
        let contents = std::fs::read_to_string(d.pid_path()).unwrap();
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn read_pid_returns_written_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn read_pid_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_returns_none_for_empty_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_returns_none_for_non_numeric_content() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "not-a-number").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_trims_whitespace() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "  12345  \n").unwrap();
        assert_eq!(d.read_pid(), Some(12345));
    }

    #[test]
    fn read_pid_returns_none_for_negative_number() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "-1").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_returns_none_for_overflow() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        // u32::MAX + 1 overflows
        std::fs::write(d.pid_path(), "4294967296").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_returns_some_for_zero() {
        // PID 0 is technically parseable as u32.
        // This test documents the current behavior: 0 is a valid u32 parse result.
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "0").unwrap();
        assert_eq!(d.read_pid(), Some(0));
    }

    #[test]
    fn read_pid_handles_pid_with_trailing_newline() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "42\n").unwrap();
        assert_eq!(d.read_pid(), Some(42));
    }

    #[test]
    fn read_pid_handles_pid_with_carriage_return() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "42\r\n").unwrap();
        assert_eq!(d.read_pid(), Some(42));
    }

    #[test]
    fn read_pid_rejects_float() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "3.14").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_rejects_multiple_numbers() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "123 456").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_rejects_hex() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "0xff").unwrap();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn read_pid_accepts_u32_max() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), u32::MAX.to_string()).unwrap();
        assert_eq!(d.read_pid(), Some(u32::MAX));
    }

    #[test]
    fn write_pid_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let nested_pid = dir.path().join("nested").join("deep").join("test.pid");
        let d = DaemonProcess::with_checker(
            "nested-test",
            nested_pid.clone(),
            dir.path().join("test.sock"),
            MockProcessChecker { alive: false },
        );
        d.write_pid().unwrap();
        assert!(nested_pid.exists());
        let contents = std::fs::read_to_string(&nested_pid).unwrap();
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn write_pid_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "old-content").unwrap();
        d.write_pid().unwrap();
        let contents = std::fs::read_to_string(d.pid_path()).unwrap();
        assert_eq!(contents, std::process::id().to_string());
    }

    #[test]
    fn write_pid_then_read_is_consistent() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        let pid1 = d.read_pid();
        let pid2 = d.read_pid();
        assert_eq!(pid1, pid2);
        assert_eq!(pid1, Some(std::process::id()));
    }

    // ----------------------------------------------------------------
    // is_running (with mock checker)
    // ----------------------------------------------------------------

    #[test]
    fn is_running_true_when_pid_exists_and_alive() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        std::fs::write(d.pid_path(), "12345").unwrap();
        assert!(d.is_running());
    }

    #[test]
    fn is_running_false_when_pid_exists_but_dead() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir); // alive: false
        std::fs::write(d.pid_path(), "12345").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn is_running_false_when_no_pid_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        assert!(!d.is_running());
    }

    #[test]
    fn is_running_false_for_empty_pid_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        std::fs::write(d.pid_path(), "").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn is_running_false_for_garbage_pid_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        std::fs::write(d.pid_path(), "hello-world").unwrap();
        assert!(!d.is_running());
    }

    // ----------------------------------------------------------------
    // is_running (with real kill(0) checker)
    // ----------------------------------------------------------------

    #[test]
    fn is_running_detects_current_process_real() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_real(&dir);
        d.write_pid().unwrap();
        assert!(d.is_running());
    }

    #[test]
    fn is_running_false_for_stale_pid_real() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_real(&dir);
        std::fs::write(d.pid_path(), "99999999").unwrap();
        assert!(!d.is_running());
    }

    // ----------------------------------------------------------------
    // Cleanup
    // ----------------------------------------------------------------

    #[test]
    fn cleanup_removes_files() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        std::fs::write(d.socket_path(), "").unwrap();
        assert!(d.pid_path().exists());
        assert!(d.socket_path().exists());
        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.socket_path().exists());
    }

    #[test]
    fn cleanup_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        std::fs::write(d.socket_path(), "").unwrap();
        d.cleanup();
        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.socket_path().exists());
    }

    #[test]
    fn cleanup_when_no_files_exist() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.cleanup();
    }

    #[test]
    fn cleanup_removes_pid_even_if_socket_missing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        assert!(d.pid_path().exists());
        d.cleanup();
        assert!(!d.pid_path().exists());
    }

    #[test]
    fn cleanup_removes_socket_even_if_pid_missing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.socket_path(), "").unwrap();
        assert!(d.socket_path().exists());
        d.cleanup();
        assert!(!d.socket_path().exists());
    }

    // ----------------------------------------------------------------
    // Acquire
    // ----------------------------------------------------------------

    #[test]
    fn acquire_succeeds_when_not_running() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.acquire().unwrap();
        assert!(d.pid_path().exists());
    }

    #[test]
    fn acquire_writes_current_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn acquire_removes_stale_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir); // alive: false → stale
        std::fs::write(d.pid_path(), "99999999").unwrap();
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn acquire_replaces_stale_pid_with_current() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "88888888").unwrap();
        d.acquire().unwrap();
        let stored_pid = d.read_pid().unwrap();
        assert_eq!(stored_pid, std::process::id());
    }

    #[test]
    fn acquire_fails_when_already_running() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("test.pid");
        std::fs::write(&pid_path, "42").unwrap();

        let d = DaemonProcess::with_checker(
            "test-app",
            pid_path,
            dir.path().join("test.sock"),
            MockProcessChecker { alive: true },
        );
        let err = d.acquire().unwrap_err();
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn acquire_error_variant_matches_daemon_already_running() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("test.pid");
        std::fs::write(&pid_path, "54321").unwrap();

        let d = DaemonProcess::with_checker(
            "test-app",
            pid_path,
            dir.path().join("test.sock"),
            MockProcessChecker { alive: true },
        );
        let err = d.acquire().unwrap_err();
        match err {
            TsunaguError::DaemonAlreadyRunning { pid } => {
                assert_eq!(pid, 54321);
            }
            other => panic!("expected DaemonAlreadyRunning, got {other:?}"),
        }
    }

    #[test]
    fn daemon_already_running_error_contains_pid() {
        let err = TsunaguError::DaemonAlreadyRunning { pid: 54321 };
        let msg = err.to_string();
        assert!(msg.contains("54321"), "error should contain the PID");
        assert!(msg.contains("already running"));
    }

    // ----------------------------------------------------------------
    // Drop
    // ----------------------------------------------------------------

    #[test]
    fn drop_cleans_up() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("test.pid");
        let sock_path = dir.path().join("test.sock");
        {
            let d = DaemonProcess::with_paths("test-app", pid_path.clone(), sock_path.clone());
            d.write_pid().unwrap();
            std::fs::write(&sock_path, "").unwrap();
            assert!(pid_path.exists());
        }
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
    }

    #[test]
    fn drop_without_any_writes_is_safe() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("noop.pid");
        let sock_path = dir.path().join("noop.sock");
        {
            let _d = DaemonProcess::with_paths("noop", pid_path.clone(), sock_path.clone());
        }
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
    }

    #[test]
    fn acquire_then_drop_cleans_pid() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("lifecycle.pid");
        let sock_path = dir.path().join("lifecycle.sock");
        {
            let d = DaemonProcess::with_checker(
                "lifecycle",
                pid_path.clone(),
                sock_path.clone(),
                MockProcessChecker { alive: false },
            );
            d.acquire().unwrap();
            assert!(pid_path.exists());
        }
        assert!(!pid_path.exists());
    }

    // ----------------------------------------------------------------
    // Display
    // ----------------------------------------------------------------

    #[test]
    fn display_includes_app_name_and_pid_path() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        let s = d.to_string();
        assert!(s.contains("test-app"), "display should contain app name: {s}");
        assert!(s.contains("test.pid"), "display should contain pid path: {s}");
    }

    // ----------------------------------------------------------------
    // Multi-daemon coexistence
    // ----------------------------------------------------------------

    #[test]
    fn multiple_daemons_different_apps_coexist() {
        let dir = TempDir::new().unwrap();
        let d1 = DaemonProcess::with_checker(
            "app-a",
            dir.path().join("a.pid"),
            dir.path().join("a.sock"),
            MockProcessChecker { alive: false },
        );
        let d2 = DaemonProcess::with_checker(
            "app-b",
            dir.path().join("b.pid"),
            dir.path().join("b.sock"),
            MockProcessChecker { alive: false },
        );
        d1.write_pid().unwrap();
        d2.write_pid().unwrap();
        assert!(d1.pid_path().exists());
        assert!(d2.pid_path().exists());
        assert_eq!(d1.app_name(), "app-a");
        assert_eq!(d2.app_name(), "app-b");
    }

    // ----------------------------------------------------------------
    // process_alive (kill(0) based)
    // ----------------------------------------------------------------

    #[test]
    fn process_alive_returns_true_for_current_process() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn process_alive_returns_false_for_nonexistent_pid() {
        assert!(!process_alive(99_999_999));
    }

    #[test]
    fn process_alive_returns_false_for_pid_one_billion() {
        assert!(!process_alive(1_000_000_000));
    }

    // ----------------------------------------------------------------
    // Full lifecycle (with real kill(0) checker)
    // ----------------------------------------------------------------

    #[test]
    fn full_lifecycle_acquire_check_cleanup() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_real(&dir);

        assert!(!d.is_running());
        assert_eq!(d.read_pid(), None);

        d.acquire().unwrap();
        assert!(d.is_running());
        assert_eq!(d.read_pid(), Some(std::process::id()));
        assert!(d.pid_path().exists());

        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.is_running());
    }

    // ----------------------------------------------------------------
    // Socket handshake integration test
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn socket_handshake_with_pid_lifecycle() {
        use tokio::net::{UnixListener, UnixStream};

        let dir = TempDir::new().unwrap();
        let d = test_daemon_real(&dir);
        d.acquire().unwrap();

        let listener = UnixListener::bind(d.socket_path()).unwrap();

        let client_path = d.socket_path().to_path_buf();
        let client = tokio::spawn(async move {
            let mut stream = UnixStream::connect(&client_path).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, b"ping").await.unwrap();
        });

        let (mut conn, _addr) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4];
        tokio::io::AsyncReadExt::read_exact(&mut conn, &mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        client.await.unwrap();

        assert!(d.is_running());
        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.socket_path().exists());
    }

    // ----------------------------------------------------------------
    // Mock checker tests
    // ----------------------------------------------------------------

    #[test]
    fn mock_checker_always_alive() {
        let dir = TempDir::new().unwrap();
        let d = DaemonProcess::with_checker(
            "mock-app",
            dir.path().join("m.pid"),
            dir.path().join("m.sock"),
            MockProcessChecker { alive: true },
        );
        std::fs::write(d.pid_path(), "12345").unwrap();
        assert!(d.is_running());
    }

    #[test]
    fn mock_checker_never_alive() {
        let dir = TempDir::new().unwrap();
        let d = DaemonProcess::with_checker(
            "mock-app",
            dir.path().join("m.pid"),
            dir.path().join("m.sock"),
            MockProcessChecker { alive: false },
        );
        std::fs::write(d.pid_path(), "12345").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn mock_checker_acquire_blocks_when_alive() {
        let dir = TempDir::new().unwrap();
        let d = DaemonProcess::with_checker(
            "mock-app",
            dir.path().join("m.pid"),
            dir.path().join("m.sock"),
            MockProcessChecker { alive: true },
        );
        std::fs::write(d.pid_path(), "999").unwrap();
        let err = d.acquire().unwrap_err();
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn mock_checker_acquire_succeeds_when_dead() {
        let dir = TempDir::new().unwrap();
        let d = DaemonProcess::with_checker(
            "mock-app",
            dir.path().join("m.pid"),
            dir.path().join("m.sock"),
            MockProcessChecker { alive: false },
        );
        std::fs::write(d.pid_path(), "999").unwrap();
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn mock_checker_is_running_depends_on_pid_file_parse() {
        // Even with alive=true, if the PID file has garbage, is_running returns false
        // because read_pid returns None.
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        std::fs::write(d.pid_path(), "not-a-pid").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn mock_checker_acquire_with_no_existing_pid_file() {
        // acquire should succeed when no PID file exists, regardless of checker
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    // ----------------------------------------------------------------
    // KillZeroChecker tests
    // ----------------------------------------------------------------

    #[test]
    fn kill_zero_checker_detects_self() {
        let checker = KillZeroChecker;
        assert!(checker.is_alive(std::process::id()));
    }

    #[test]
    fn kill_zero_checker_rejects_nonexistent() {
        let checker = KillZeroChecker;
        assert!(!checker.is_alive(99_999_999));
    }

    // ----------------------------------------------------------------
    // SystemProcessChecker (constructor only -- not relying on ps -p)
    // ----------------------------------------------------------------

    #[test]
    fn system_process_checker_is_default_constructible() {
        let _checker = SystemProcessChecker::default();
    }

    #[test]
    fn system_process_checker_rejects_nonexistent_pid() {
        let checker = SystemProcessChecker::default();
        assert!(!checker.is_alive(99_999_999));
    }

    // ----------------------------------------------------------------
    // Bidirectional socket communication
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn socket_bidirectional_communication() {
        use tokio::net::{UnixListener, UnixStream};

        let dir = TempDir::new().unwrap();
        let d = test_daemon_real(&dir);
        d.acquire().unwrap();

        let listener = UnixListener::bind(d.socket_path()).unwrap();

        let client_path = d.socket_path().to_path_buf();
        let client = tokio::spawn(async move {
            let mut stream = UnixStream::connect(&client_path).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, b"hello").await.unwrap();
            let mut buf = vec![0u8; 5];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut buf).await.unwrap();
            assert_eq!(&buf, b"world");
        });

        let (mut conn, _addr) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut conn, &mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        tokio::io::AsyncWriteExt::write_all(&mut conn, b"world").await.unwrap();

        client.await.unwrap();
    }

    // ----------------------------------------------------------------
    // Edge cases
    // ----------------------------------------------------------------

    #[test]
    fn acquire_twice_without_cleanup_succeeds_with_dead_checker() {
        // The second acquire sees a PID file with the current PID,
        // but the mock says the process is dead, so it re-acquires.
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir); // alive: false
        d.acquire().unwrap();
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn acquire_twice_without_cleanup_fails_with_alive_checker() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon_alive(&dir);
        d.acquire().unwrap();
        let err = d.acquire().unwrap_err();
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn cleanup_then_reacquire() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.acquire().unwrap();
        d.cleanup();
        assert!(!d.pid_path().exists());
        d.acquire().unwrap();
        assert!(d.pid_path().exists());
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn write_pid_then_cleanup_then_read_returns_none() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        d.cleanup();
        assert_eq!(d.read_pid(), None);
    }

    #[test]
    fn multiple_writes_last_pid_wins() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        // Manually overwrite with a different PID
        std::fs::write(d.pid_path(), "77777").unwrap();
        assert_eq!(d.read_pid(), Some(77777));
    }

    #[test]
    fn acquire_cleans_stale_pid_file_before_writing() {
        // Verify the stale file is actually removed (not just overwritten)
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir); // alive: false
        let stale_content = "88888888";
        std::fs::write(d.pid_path(), stale_content).unwrap();
        d.acquire().unwrap();
        let contents = std::fs::read_to_string(d.pid_path()).unwrap();
        assert_ne!(contents, stale_content);
        assert_eq!(contents, std::process::id().to_string());
    }
}
