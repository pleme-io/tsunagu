# Tsunagu (繋ぐ)

Service/daemon IPC framework for pleme-io applications. Provides reusable patterns for client-daemon communication so every app with a background service shares the same lifecycle management.

## Components

| Module | Purpose |
|--------|---------|
| `daemon` | `DaemonProcess` — PID file management, process lifecycle |
| `socket` | `SocketPath` — XDG-compliant Unix socket path resolution |
| `health` | `HealthCheck` — standardized liveness/readiness probes |
| `error` | Unified error type |

## Usage

```toml
[dependencies]
tsunagu = { git = "https://github.com/pleme-io/tsunagu" }
```

```rust
use tsunagu::{DaemonProcess, SocketPath};

let daemon = DaemonProcess::new("myapp");
if !daemon.is_running() {
    daemon.write_pid()?;
    // start gRPC server on daemon.socket_path()
}
```

## Build

```bash
cargo build
cargo test --lib
```
