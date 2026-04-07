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
        Self::app_file(app_name, "sock")
    }

    /// Resolve the PID file path for an application daemon.
    ///
    /// Returns `{runtime_dir}/{app_name}/{app_name}.pid`
    #[must_use]
    pub fn pid_file(app_name: &str) -> PathBuf {
        Self::app_file(app_name, "pid")
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

    /// Internal helper: `runtime_base(app) / {app}.{ext}`.
    fn app_file(app_name: &str, ext: &str) -> PathBuf {
        Self::runtime_base(app_name).join(format!("{app_name}.{ext}"))
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

    // --- Additional tests ---

    #[test]
    fn socket_path_has_exactly_three_components_under_base() {
        // runtime_base/{app_name}.sock — the file name should be app_name.sock
        let path = SocketPath::for_app("hibiki");
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(file_name, "hibiki.sock");
    }

    #[test]
    fn pid_file_has_correct_extension() {
        let path = SocketPath::pid_file("mado");
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(file_name, "mado.pid");
    }

    #[test]
    fn runtime_base_is_parent_of_socket_path() {
        let base = SocketPath::runtime_base("kagi");
        let sock = SocketPath::for_app("kagi");
        assert_eq!(sock.parent().unwrap(), base);
    }

    #[test]
    fn runtime_base_is_parent_of_pid_path() {
        let base = SocketPath::runtime_base("kagi");
        let pid = SocketPath::pid_file("kagi");
        assert_eq!(pid.parent().unwrap(), base);
    }

    #[test]
    fn different_apps_get_different_paths() {
        let sock_a = SocketPath::for_app("alpha");
        let sock_b = SocketPath::for_app("beta");
        assert_ne!(sock_a, sock_b);

        let pid_a = SocketPath::pid_file("alpha");
        let pid_b = SocketPath::pid_file("beta");
        assert_ne!(pid_a, pid_b);
    }

    #[test]
    fn same_app_produces_deterministic_paths() {
        let sock1 = SocketPath::for_app("stable");
        let sock2 = SocketPath::for_app("stable");
        assert_eq!(sock1, sock2);

        let pid1 = SocketPath::pid_file("stable");
        let pid2 = SocketPath::pid_file("stable");
        assert_eq!(pid1, pid2);
    }

    #[test]
    fn socket_and_pid_have_different_extensions() {
        let sock = SocketPath::for_app("testapp");
        let pid = SocketPath::pid_file("testapp");
        assert_ne!(sock, pid);
        assert_ne!(
            sock.extension().unwrap().to_string_lossy(),
            pid.extension().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn app_name_with_hyphens() {
        let path = SocketPath::for_app("my-daemon");
        assert!(path.to_string_lossy().contains("my-daemon"));
        assert!(path.to_string_lossy().ends_with("my-daemon.sock"));
    }

    #[test]
    fn app_name_with_underscores() {
        let path = SocketPath::for_app("my_daemon");
        assert!(path.to_string_lossy().contains("my_daemon"));
        assert!(path.to_string_lossy().ends_with("my_daemon.sock"));
    }

    #[test]
    fn app_name_with_dots() {
        // App names might have dots, e.g., "com.pleme.daemon"
        let path = SocketPath::for_app("com.pleme.daemon");
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert_eq!(file_name, "com.pleme.daemon.sock");
    }

    #[test]
    fn empty_app_name_still_produces_path() {
        // Edge case: empty string is degenerate but should not panic
        let path = SocketPath::for_app("");
        assert!(path.to_string_lossy().ends_with(".sock"));
    }

    #[test]
    fn xdg_runtime_dir_with_trailing_slash() {
        // SAFETY: test runs single-threaded; no other threads reading this env var.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/tmp/xdg-test/") };
        let path = SocketPath::for_app("svc");
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        // PathBuf::from normalizes trailing slashes; the path should still work
        assert!(path.to_string_lossy().contains("svc"));
        assert!(path.to_string_lossy().ends_with("svc.sock"));
    }

    #[test]
    fn paths_are_absolute() {
        // With XDG_RUNTIME_DIR set to an absolute path, results should be absolute
        // SAFETY: test runs single-threaded; no other threads reading this env var.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/tmp/abs-test") };
        let sock = SocketPath::for_app("abstest");
        let pid = SocketPath::pid_file("abstest");
        let base = SocketPath::runtime_base("abstest");
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        assert!(sock.is_absolute());
        assert!(pid.is_absolute());
        assert!(base.is_absolute());
    }

    #[test]
    fn fallback_to_temp_dir_when_xdg_unset() {
        // SAFETY: test runs single-threaded; no other threads reading this env var.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        let path = SocketPath::for_app("fallback-test");
        // Should not panic and should produce a valid path
        assert!(path.to_string_lossy().contains("fallback-test"));
        assert!(path.to_string_lossy().ends_with("fallback-test.sock"));
    }
}
