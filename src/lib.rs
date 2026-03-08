//! Tsunagu (繋ぐ) — service/daemon IPC framework.
//!
//! Provides reusable patterns for daemon lifecycle management:
//! - [`SocketPath`]: XDG-compliant Unix socket and PID file path resolution
//! - [`DaemonProcess`]: PID file management, staleness detection, and cleanup
//! - [`HealthCheck`]: standardized health/liveness/readiness responses
//!
//! # Quick Start
//!
//! ```
//! use tsunagu::{DaemonProcess, HealthCheck, SocketPath};
//!
//! // Resolve paths for your app
//! let sock = SocketPath::for_app("myapp");
//! let pid = SocketPath::pid_file("myapp");
//! assert!(sock.to_string_lossy().contains("myapp"));
//!
//! // Health check response
//! let hc = HealthCheck::healthy("myapp", "0.1.0");
//! assert!(hc.is_healthy());
//! ```

pub mod daemon;
pub mod error;
pub mod health;
pub mod socket;

pub use daemon::DaemonProcess;
pub use error::TsunaguError;
pub use health::{HealthCheck, HealthStatus};
pub use socket::SocketPath;
