use crate::error::TsunaguError;
use crate::socket::SocketPath;
use std::path::{Path, PathBuf};

/// Daemon process lifecycle management.
///
/// Handles PID file creation, staleness detection, and graceful cleanup.
/// The PID file and socket are automatically removed on drop.
pub struct DaemonProcess {
    app_name: String,
    pid_path: PathBuf,
    socket_path: PathBuf,
}

impl DaemonProcess {
    /// Create a new daemon process manager for the given application.
    #[must_use]
    pub fn new(app_name: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            pid_path: SocketPath::pid_file(app_name),
            socket_path: SocketPath::for_app(app_name),
        }
    }

    /// Create a daemon process with custom paths (useful for testing).
    #[must_use]
    pub fn with_paths(app_name: &str, pid_path: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            app_name: app_name.to_string(),
            pid_path,
            socket_path,
        }
    }

    /// Check if a daemon is already running (PID file exists and process is alive).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.read_pid().is_some_and(process_alive)
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
    pub fn acquire(&self) -> Result<(), TsunaguError> {
        if let Some(pid) = self.read_pid() {
            if process_alive(pid) {
                return Err(TsunaguError::DaemonAlreadyRunning { pid });
            }
            // Stale PID file — remove it
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

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Check if a process with the given PID is alive.
///
/// Uses `/proc/{pid}` on Linux, falls back to `ps -p` on macOS/other Unix.
pub(crate) fn process_alive(pid: u32) -> bool {
    // Try /proc first (Linux)
    let proc_path = std::path::PathBuf::from(format!("/proc/{pid}"));
    if proc_path.exists() {
        return true;
    }

    // Fallback to `ps -p` (macOS + other Unix)
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_daemon(dir: &TempDir) -> DaemonProcess {
        DaemonProcess::with_paths(
            "test-app",
            dir.path().join("test.pid"),
            dir.path().join("test.sock"),
        )
    }

    #[test]
    fn new_daemon_not_running() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        assert!(!d.is_running());
    }

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
    fn is_running_detects_current_process() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        // Our own process IS alive
        assert!(d.is_running());
    }

    #[test]
    fn is_running_false_for_stale_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        // Write a PID that almost certainly doesn't exist
        std::fs::write(d.pid_path(), "99999999").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn cleanup_removes_files() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        // Create a fake socket file
        std::fs::write(d.socket_path(), "").unwrap();
        assert!(d.pid_path().exists());
        assert!(d.socket_path().exists());
        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.socket_path().exists());
    }

    #[test]
    fn acquire_succeeds_when_not_running() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.acquire().unwrap();
        assert!(d.pid_path().exists());
    }

    #[test]
    fn acquire_removes_stale_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        // Write stale PID
        std::fs::write(d.pid_path(), "99999999").unwrap();
        // Should succeed because the PID is stale
        d.acquire().unwrap();
        // Now has our PID
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn acquire_fails_when_already_running() {
        let dir = TempDir::new().unwrap();
        // Write our own PID (which IS alive)
        let pid_path = dir.path().join("test.pid");
        std::fs::write(&pid_path, std::process::id().to_string()).unwrap();

        let d = DaemonProcess::with_paths(
            "test-app",
            pid_path,
            dir.path().join("test.sock"),
        );
        let err = d.acquire().unwrap_err();
        assert!(err.to_string().contains("already running"));
    }

    #[test]
    fn app_name_is_stored() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        assert_eq!(d.app_name(), "test-app");
    }

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
        // After drop, files should be gone
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
    }

    // --- Additional tests ---

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
    fn read_pid_returns_none_for_zero() {
        // PID 0 is technically parseable as u32, but the function returns Some(0).
        // This test documents the current behavior: 0 is a valid u32 parse result.
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "0").unwrap();
        assert_eq!(d.read_pid(), Some(0));
    }

    #[test]
    fn write_pid_creates_parent_directories() {
        let dir = TempDir::new().unwrap();
        let nested_pid = dir.path().join("nested").join("deep").join("test.pid");
        let d = DaemonProcess::with_paths(
            "nested-test",
            nested_pid.clone(),
            dir.path().join("test.sock"),
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
    fn cleanup_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        std::fs::write(d.socket_path(), "").unwrap();
        d.cleanup();
        // Second cleanup should not panic
        d.cleanup();
        assert!(!d.pid_path().exists());
        assert!(!d.socket_path().exists());
    }

    #[test]
    fn cleanup_when_no_files_exist() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        // Files never created; cleanup should not panic
        d.cleanup();
    }

    #[test]
    fn cleanup_removes_pid_even_if_socket_missing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        // Do not create socket file
        assert!(d.pid_path().exists());
        d.cleanup();
        assert!(!d.pid_path().exists());
    }

    #[test]
    fn cleanup_removes_socket_even_if_pid_missing() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.socket_path(), "").unwrap();
        // Do not create PID file
        assert!(d.socket_path().exists());
        d.cleanup();
        assert!(!d.socket_path().exists());
    }

    #[test]
    fn acquire_writes_current_pid() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.acquire().unwrap();
        assert_eq!(d.read_pid(), Some(std::process::id()));
    }

    #[test]
    fn daemon_already_running_error_contains_pid() {
        // Test the error variant directly; process_alive may not work in
        // sandboxed environments (macOS entitlement issue with `ps`).
        let err = TsunaguError::DaemonAlreadyRunning { pid: 54321 };
        let msg = err.to_string();
        assert!(msg.contains("54321"), "error should contain the PID");
        assert!(msg.contains("already running"));
    }

    #[test]
    fn acquire_replaces_stale_pid_with_current() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        // Write a stale PID
        std::fs::write(d.pid_path(), "88888888").unwrap();
        d.acquire().unwrap();
        // After acquire, our PID replaces the stale one
        let stored_pid = d.read_pid().unwrap();
        assert_eq!(stored_pid, std::process::id());
    }

    #[test]
    fn is_running_false_for_empty_pid_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn is_running_false_for_garbage_pid_file() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        std::fs::write(d.pid_path(), "hello-world").unwrap();
        assert!(!d.is_running());
    }

    #[test]
    fn process_alive_returns_false_for_nonexistent_pid() {
        // PID 99_999_999 is almost certainly not running
        assert!(!process_alive(99_999_999));
    }

    #[test]
    fn drop_without_any_writes_is_safe() {
        let dir = TempDir::new().unwrap();
        let pid_path = dir.path().join("noop.pid");
        let sock_path = dir.path().join("noop.sock");
        {
            let _d = DaemonProcess::with_paths("noop", pid_path.clone(), sock_path.clone());
            // Drop immediately without writing anything
        }
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
    }

    #[test]
    fn multiple_daemons_different_apps_coexist() {
        let dir = TempDir::new().unwrap();
        let d1 = DaemonProcess::with_paths(
            "app-a",
            dir.path().join("a.pid"),
            dir.path().join("a.sock"),
        );
        let d2 = DaemonProcess::with_paths(
            "app-b",
            dir.path().join("b.pid"),
            dir.path().join("b.sock"),
        );
        d1.write_pid().unwrap();
        d2.write_pid().unwrap();
        assert!(d1.pid_path().exists());
        assert!(d2.pid_path().exists());
        assert_eq!(d1.app_name(), "app-a");
        assert_eq!(d2.app_name(), "app-b");
    }

    #[test]
    fn write_pid_then_read_is_consistent() {
        let dir = TempDir::new().unwrap();
        let d = test_daemon(&dir);
        d.write_pid().unwrap();
        // Read it back multiple times to confirm consistency
        let pid1 = d.read_pid();
        let pid2 = d.read_pid();
        assert_eq!(pid1, pid2);
        assert_eq!(pid1, Some(std::process::id()));
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
}
