/// Errors produced by the tsunagu IPC framework.
#[derive(Debug, thiserror::Error)]
pub enum TsunaguError {
    #[error("daemon not running at {path}")]
    DaemonNotRunning { path: String },

    #[error("daemon already running (pid {pid})")]
    DaemonAlreadyRunning { pid: u32 },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("health check failed: {0}")]
    HealthCheck(String),

    #[error("invalid PID file: {0}")]
    InvalidPidFile(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_not_running_display() {
        let err = TsunaguError::DaemonNotRunning {
            path: "/run/myapp/myapp.pid".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("daemon not running"));
        assert!(msg.contains("/run/myapp/myapp.pid"));
    }

    #[test]
    fn daemon_already_running_display() {
        let err = TsunaguError::DaemonAlreadyRunning { pid: 12345 };
        let msg = err.to_string();
        assert!(msg.contains("already running"));
        assert!(msg.contains("12345"));
    }

    #[test]
    fn io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: TsunaguError = io_err.into();
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn health_check_error_display() {
        let err = TsunaguError::HealthCheck("connection timeout".to_string());
        let msg = err.to_string();
        assert!(msg.contains("health check failed"));
        assert!(msg.contains("connection timeout"));
    }

    #[test]
    fn invalid_pid_file_display() {
        let err = TsunaguError::InvalidPidFile("contains garbage".to_string());
        let msg = err.to_string();
        assert!(msg.contains("invalid PID file"));
        assert!(msg.contains("contains garbage"));
    }

    #[test]
    fn error_is_debug_formattable() {
        let err = TsunaguError::DaemonAlreadyRunning { pid: 1 };
        let debug = format!("{err:?}");
        assert!(debug.contains("DaemonAlreadyRunning"));
    }

    #[test]
    fn io_error_preserves_kind() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err: TsunaguError = io_err.into();
        match err {
            TsunaguError::Io(ref e) => assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied),
            other => panic!("expected Io variant, got {other:?}"),
        }
    }

    #[test]
    fn error_implements_std_error() {
        // Verify TsunaguError implements std::error::Error
        let err: Box<dyn std::error::Error> =
            Box::new(TsunaguError::HealthCheck("test".to_string()));
        assert!(err.to_string().contains("health check failed"));
    }

    #[test]
    fn daemon_already_running_with_pid_zero() {
        let err = TsunaguError::DaemonAlreadyRunning { pid: 0 };
        let msg = err.to_string();
        assert!(msg.contains("pid 0"));
    }

    #[test]
    fn daemon_already_running_with_max_pid() {
        let err = TsunaguError::DaemonAlreadyRunning { pid: u32::MAX };
        let msg = err.to_string();
        assert!(msg.contains(&u32::MAX.to_string()));
    }

    #[test]
    fn daemon_not_running_empty_path() {
        let err = TsunaguError::DaemonNotRunning {
            path: String::new(),
        };
        let msg = err.to_string();
        assert!(msg.contains("daemon not running at"));
    }
}
