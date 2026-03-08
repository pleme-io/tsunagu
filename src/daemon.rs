use crate::error::TsunaguError;
use crate::socket::SocketPath;
use std::path::PathBuf;

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
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Path to the PID file.
    #[must_use]
    pub fn pid_path(&self) -> &PathBuf {
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

/// Check if a process with the given PID is alive. Pure Rust, no libc.
///
/// Uses `/proc/{pid}` on Linux, `ps -p` on macOS/other.
fn process_alive(pid: u32) -> bool {
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
}
