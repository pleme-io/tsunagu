use std::path::PathBuf;

/// XDG-compliant socket and PID file path resolution.
///
/// Sockets are placed in `$XDG_RUNTIME_DIR/{app}/` or fallback to `/tmp/{app}/`.
pub struct SocketPath;

impl SocketPath {
    /// Resolve the socket path for an application.
    ///
    /// Returns `{runtime_dir}/{app_name}/{app_name}.sock`
    #[must_use]
    pub fn for_app(app_name: &str) -> PathBuf {
        Self::runtime_base(app_name).join(format!("{app_name}.sock"))
    }

    /// Resolve the PID file path for an application daemon.
    ///
    /// Returns `{runtime_dir}/{app_name}/{app_name}.pid`
    #[must_use]
    pub fn pid_file(app_name: &str) -> PathBuf {
        Self::runtime_base(app_name).join(format!("{app_name}.pid"))
    }

    /// Resolve the base runtime directory for an application.
    ///
    /// Uses `$XDG_RUNTIME_DIR` if set, otherwise falls back to `/tmp`.
    #[must_use]
    pub fn runtime_base(app_name: &str) -> PathBuf {
        let base = std::env::var("XDG_RUNTIME_DIR").map_or_else(
            |_| dirs::runtime_dir().unwrap_or_else(std::env::temp_dir),
            PathBuf::from,
        );
        base.join(app_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_contains_app_name() {
        let path = SocketPath::for_app("tobira");
        assert!(path.to_string_lossy().contains("tobira"));
        assert!(path.to_string_lossy().ends_with("tobira.sock"));
    }

    #[test]
    fn pid_file_contains_app_name() {
        let path = SocketPath::pid_file("tobira");
        assert!(path.to_string_lossy().contains("tobira"));
        assert!(path.to_string_lossy().ends_with("tobira.pid"));
    }

    #[test]
    fn socket_and_pid_share_directory() {
        let sock = SocketPath::for_app("myapp");
        let pid = SocketPath::pid_file("myapp");
        assert_eq!(sock.parent(), pid.parent());
    }

    #[test]
    fn runtime_base_contains_app_name() {
        let base = SocketPath::runtime_base("karakuri");
        assert!(base.to_string_lossy().ends_with("karakuri"));
    }

    #[test]
    fn respects_xdg_runtime_dir() {
        // SAFETY: test runs single-threaded; no other threads reading this env var.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/tmp/test-xdg-runtime") };
        let path = SocketPath::for_app("test");
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        assert!(path.starts_with("/tmp/test-xdg-runtime/test"));
    }
}
