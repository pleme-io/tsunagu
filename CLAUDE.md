# Tsunagu (繋ぐ) — Service/Daemon IPC Framework

> **★★★ CSE / Knowable Construction.** This repo operates under **Constructive Substrate Engineering** — canonical specification at [`pleme-io/theory/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md`](https://github.com/pleme-io/theory/blob/main/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md). The Compounding Directive (operational rules: solve once, load-bearing fixes only, idiom-first, models stay current, direction beats velocity) is in the org-level pleme-io/CLAUDE.md ★★★ section. Read both before non-trivial changes.


## Build & Test

```bash
cargo build
cargo test --lib
```

## Architecture

Reusable daemon/service communication library for all pleme-io applications that need background processes.

### Modules

| Module | Purpose |
|--------|---------|
| `daemon.rs` | `DaemonProcess` — PID file lifecycle, stale process detection, cleanup on drop |
| `socket.rs` | `SocketPath` — XDG runtime dir socket paths, PID file paths |
| `health.rs` | `HealthCheck` — Healthy/Degraded/Unhealthy status with serde |
| `error.rs` | `TsunaguError` — unified error enum |

### gRPC Pattern

Consumers define their own `.proto` files and use tonic. Tsunagu provides the daemon lifecycle (PID, socket, health) — not the RPC definitions.

### Consumers

Used by: mado, hibiki, kagi, kekkai

## Design Decisions

- **Lifecycle, not protocol**: manages daemon process, not RPC schema
- **XDG compliant**: sockets in `$XDG_RUNTIME_DIR/{app}/` or `/tmp/{app}/`
- **Drop cleanup**: PID file and socket removed when `DaemonProcess` drops
- **No async runtime opinion**: consumers bring their own tokio/async-std
