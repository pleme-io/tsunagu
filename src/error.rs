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
