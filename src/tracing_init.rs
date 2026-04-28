//! `tracing_subscriber` bootstrap helper.
//!
//! Subsumes the boilerplate that recurs in 90+ pleme-io binaries:
//!
//! ```ignore
//! tracing_subscriber::fmt()
//!     .with_env_filter(
//!         tracing_subscriber::EnvFilter::try_from_default_env()
//!             .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
//!     )
//!     .init();
//! ```
//!
//! becomes:
//!
//! ```ignore
//! tsunagu::tracing_init::init_default();
//! ```
//!
//! or for finer control:
//!
//! ```ignore
//! tsunagu::tracing_init::init(tsunagu::tracing_init::TracingOpts {
//!     default_directive: "myapp=info".into(),
//!     json: true,
//!     stderr: true,
//!     with_target: true,
//! });
//! ```
//!
//! `RUST_LOG` always wins when set, matching the canonical
//! `EnvFilter::try_from_default_env()` semantics.
//!
//! # Feature gate
//!
//! Pulled in via `tsunagu = { version = "0.1", features = ["tracing-init"] }`.
//! Off by default so library crates don't drag the `tracing-subscriber`
//! closure into consumers' builds.

use tracing_subscriber::EnvFilter;

/// Options for [`init`].
///
/// Fields are public so callers can construct via struct-literal +
/// `..Default::default()`. Defaults match the most common pleme-io
/// shape: `info` directive, plain (non-JSON) output, stdout writer,
/// no module target prefix.
#[derive(Debug, Clone)]
pub struct TracingOpts {
    /// Directive used when `RUST_LOG` is unset. Examples:
    /// `"info"`, `"warn"`, `"myapp=debug,hyper=warn"`.
    pub default_directive: String,
    /// Emit JSON-formatted spans/events instead of human-readable.
    pub json: bool,
    /// Write to stderr instead of stdout. Common in CLIs where stdout
    /// is reserved for program output.
    pub stderr: bool,
    /// Include the `target=` field (typically the module path).
    pub with_target: bool,
}

impl Default for TracingOpts {
    fn default() -> Self {
        Self {
            default_directive: "info".to_string(),
            json: false,
            stderr: false,
            with_target: false,
        }
    }
}

/// Build the [`EnvFilter`] from `RUST_LOG` falling back to
/// `opts.default_directive`. Pure — no global init side-effect.
///
/// Exposed so callers that need to compose with a custom subscriber
/// stack (e.g. `tracing_appender` rolling files, OpenTelemetry
/// pipelines) can reuse the directive-resolution logic without
/// pulling the rest of the formatter setup.
#[must_use]
pub fn build_filter(default_directive: &str) -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_directive))
}

/// Initialise the global `tracing_subscriber` according to `opts`.
///
/// Idempotent in practice — the underlying `try_init` returns an
/// error if the global subscriber is already set, which is silently
/// ignored so callers can call this from main without crashing tests
/// that have set up their own subscribers.
pub fn init(opts: TracingOpts) {
    let filter = build_filter(&opts.default_directive);
    let fmt = tracing_subscriber::fmt().with_env_filter(filter);

    // 8 (json × stderr × target) variants. Builder type changes per
    // call so we can't pre-build common pieces — manual fan-out below.
    match (opts.json, opts.stderr, opts.with_target) {
        (true, true, true) => {
            let _ = fmt
                .json()
                .with_writer(std::io::stderr)
                .with_target(true)
                .try_init();
        }
        (true, true, false) => {
            let _ = fmt
                .json()
                .with_writer(std::io::stderr)
                .with_target(false)
                .try_init();
        }
        (true, false, true) => {
            let _ = fmt.json().with_target(true).try_init();
        }
        (true, false, false) => {
            let _ = fmt.json().with_target(false).try_init();
        }
        (false, true, true) => {
            let _ = fmt
                .with_writer(std::io::stderr)
                .with_target(true)
                .try_init();
        }
        (false, true, false) => {
            let _ = fmt
                .with_writer(std::io::stderr)
                .with_target(false)
                .try_init();
        }
        (false, false, true) => {
            let _ = fmt.with_target(true).try_init();
        }
        (false, false, false) => {
            let _ = fmt.with_target(false).try_init();
        }
    }
}

/// Initialise with all defaults — `info`, plain text, stdout, no
/// target.
pub fn init_default() {
    init(TracingOpts::default());
}

/// Initialise with a custom default directive but otherwise default
/// formatting. The 80% case for CLIs that want their own crate's
/// log level by default while honouring `RUST_LOG`.
pub fn init_with(default_directive: impl Into<String>) {
    init(TracingOpts {
        default_directive: default_directive.into(),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_opts_match_expected_shape() {
        let o = TracingOpts::default();
        assert_eq!(o.default_directive, "info");
        assert!(!o.json);
        assert!(!o.stderr);
        assert!(!o.with_target);
    }

    // RUST_LOG is process-global; cargo test runs tests in parallel
    // threads. We bundle both directive-resolution paths into a single
    // sequential test under one Mutex guard so they don't race against
    // each other or against any other test that touches RUST_LOG.
    #[test]
    fn build_filter_directive_resolution() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("RUST_LOG").ok();

        // (1) RUST_LOG unset -> falls back to the supplied directive.
        // SAFETY: setter requires unsafe in edition-2024.
        unsafe {
            std::env::remove_var("RUST_LOG");
        }
        let filter = build_filter("warn");
        let s = format!("{filter}");
        assert!(s.contains("warn"), "fallback filter: {s}");

        // (2) RUST_LOG set -> wins over the supplied directive.
        // SAFETY: setter requires unsafe in edition-2024.
        unsafe {
            std::env::set_var("RUST_LOG", "myapp=trace");
        }
        let filter = build_filter("info");
        let s = format!("{filter}");
        assert!(s.contains("myapp"), "rust_log filter: {s}");

        // Restore.
        unsafe {
            match prev {
                Some(p) => std::env::set_var("RUST_LOG", p),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
    }

    #[test]
    fn opts_clone_independent() {
        let a = TracingOpts {
            default_directive: "debug".into(),
            json: true,
            ..Default::default()
        };
        let b = a.clone();
        assert_eq!(a.default_directive, b.default_directive);
        assert_eq!(a.json, b.json);
    }

    // We don't test init() directly because it sets the global
    // subscriber — running it under cargo test would interfere
    // with the harness's own logger. The pure pieces (build_filter,
    // TracingOpts) cover the load-bearing behaviour; the
    // 8-variant fan-out in init() is type-checked at compile time
    // and is the only piece that materially varies.
}
