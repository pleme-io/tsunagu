use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};

/// Trait for pluggable health check implementations.
///
/// Services implement this to provide custom health semantics.
/// The default [`SimpleHealthChecker`] reports a fixed status that can be
/// toggled at runtime via atomic state.
///
/// # Example
///
/// ```
/// use tsunagu::{HealthChecker, SimpleHealthChecker, HealthStatus};
///
/// let checker = SimpleHealthChecker::new("myapp", "0.1.0");
/// assert!(checker.check().is_healthy());
///
/// // Use as trait object
/// let boxed: Box<dyn HealthChecker> = Box::new(checker);
/// assert_eq!(boxed.service_name(), "myapp");
/// ```
pub trait HealthChecker: Send + Sync {
    /// Perform a health check and return the current status.
    fn check(&self) -> HealthStatus;

    /// Service name for reporting.
    fn service_name(&self) -> &str;

    /// Service version for reporting.
    fn version(&self) -> &str;
}

/// Basic health checker that reports a fixed status.
///
/// The status can be toggled at runtime via [`set_healthy`](Self::set_healthy),
/// [`set_degraded`](Self::set_degraded), and [`set_unhealthy`](Self::set_unhealthy).
/// Status is stored as an atomic u8 (0 = healthy, 1 = degraded, 2 = unhealthy)
/// so reads and writes are lock-free.
///
/// # Example
///
/// ```
/// use tsunagu::{HealthChecker, SimpleHealthChecker};
///
/// let checker = SimpleHealthChecker::new("svc", "1.0");
/// assert!(checker.check().is_healthy());
///
/// checker.set_degraded();
/// assert!(checker.check().is_degraded());
/// ```
pub struct SimpleHealthChecker {
    service: String,
    version: String,
    status: AtomicU8,
}

impl SimpleHealthChecker {
    /// Healthy state constant.
    const HEALTHY: u8 = 0;
    /// Degraded state constant.
    const DEGRADED: u8 = 1;
    /// Unhealthy state constant.
    const UNHEALTHY: u8 = 2;

    /// Create a new checker that starts in the [`HealthStatus::Healthy`] state.
    #[must_use]
    pub fn new(service: &str, version: &str) -> Self {
        Self {
            service: service.to_string(),
            version: version.to_string(),
            status: AtomicU8::new(Self::HEALTHY),
        }
    }

    /// Set the status to [`HealthStatus::Healthy`].
    pub fn set_healthy(&self) {
        self.status.store(Self::HEALTHY, Ordering::Relaxed);
    }

    /// Set the status to [`HealthStatus::Degraded`].
    pub fn set_degraded(&self) {
        self.status.store(Self::DEGRADED, Ordering::Relaxed);
    }

    /// Set the status to [`HealthStatus::Unhealthy`].
    pub fn set_unhealthy(&self) {
        self.status.store(Self::UNHEALTHY, Ordering::Relaxed);
    }
}

impl HealthChecker for SimpleHealthChecker {
    fn check(&self) -> HealthStatus {
        match self.status.load(Ordering::Relaxed) {
            Self::DEGRADED => HealthStatus::Degraded("degraded".to_string()),
            Self::UNHEALTHY => HealthStatus::Unhealthy("unhealthy".to_string()),
            _ => HealthStatus::Healthy,
        }
    }

    fn service_name(&self) -> &str {
        &self.service
    }

    fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Debug for SimpleHealthChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimpleHealthChecker")
            .field("service", &self.service)
            .field("version", &self.version)
            .field("status", &self.status.load(Ordering::Relaxed))
            .finish()
    }
}

/// Health status for a daemon service.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum HealthStatus {
    /// Service is fully operational.
    #[default]
    Healthy,
    /// Service is running but with degraded functionality.
    Degraded(String),
    /// Service is not operational.
    Unhealthy(String),
}

impl HealthStatus {
    /// Returns the reason string for `Degraded` or `Unhealthy`, `None` for `Healthy`.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Healthy => None,
            Self::Degraded(r) | Self::Unhealthy(r) => Some(r),
        }
    }

    /// `true` when `Healthy`.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }

    /// `true` when `Degraded`.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded(_))
    }

    /// `true` when `Unhealthy`.
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        matches!(self, Self::Unhealthy(_))
    }
}

/// Standardized health check response for daemon services.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct HealthCheck {
    pub service: String,
    pub status: HealthStatus,
    pub version: String,
    pub uptime_secs: Option<u64>,
}

impl HealthCheck {
    /// Create a healthy check result.
    #[must_use]
    pub fn healthy(service: &str, version: &str) -> Self {
        Self {
            service: service.to_string(),
            status: HealthStatus::Healthy,
            version: version.to_string(),
            uptime_secs: None,
        }
    }

    /// Create an unhealthy check result.
    #[must_use]
    pub fn unhealthy(service: &str, version: &str, reason: &str) -> Self {
        Self {
            service: service.to_string(),
            status: HealthStatus::Unhealthy(reason.to_string()),
            version: version.to_string(),
            uptime_secs: None,
        }
    }

    /// Create a degraded check result.
    #[must_use]
    pub fn degraded(service: &str, version: &str, reason: &str) -> Self {
        Self {
            service: service.to_string(),
            status: HealthStatus::Degraded(reason.to_string()),
            version: version.to_string(),
            uptime_secs: None,
        }
    }

    /// Set the uptime in seconds.
    #[must_use]
    pub fn with_uptime(mut self, secs: u64) -> Self {
        self.uptime_secs = Some(secs);
        self
    }

    /// Whether the service is healthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.status.is_healthy()
    }

    /// Whether the service is degraded (running but impaired).
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.status.is_degraded()
    }

    /// Whether the service is unhealthy.
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        self.status.is_unhealthy()
    }

    /// Deserialize from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, crate::TsunaguError> {
        Ok(serde_json::from_str(json)?)
    }

    /// Serialize to a pretty-printed JSON string.
    #[must_use = "serialization may fail; handle the error"]
    pub fn to_json(&self) -> Result<String, crate::TsunaguError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Start building a [`HealthCheck`] with the required fields.
    #[must_use]
    pub fn builder(service: &str, version: &str) -> HealthCheckBuilder {
        HealthCheckBuilder {
            service: service.to_string(),
            version: version.to_string(),
            status: HealthStatus::Healthy,
            uptime_secs: None,
        }
    }
}

/// Incremental builder for [`HealthCheck`].
///
/// ```
/// use tsunagu::{HealthCheck, HealthStatus};
///
/// let hc = HealthCheck::builder("myapp", "0.1.0")
///     .status(HealthStatus::Degraded("slow".into()))
///     .uptime_secs(120)
///     .build();
/// assert!(hc.is_degraded());
/// ```
#[derive(Debug, Clone)]
pub struct HealthCheckBuilder {
    service: String,
    version: String,
    status: HealthStatus,
    uptime_secs: Option<u64>,
}

impl HealthCheckBuilder {
    /// Set the health status (defaults to [`HealthStatus::Healthy`]).
    #[must_use]
    pub fn status(mut self, status: HealthStatus) -> Self {
        self.status = status;
        self
    }

    /// Set uptime in seconds.
    #[must_use]
    pub fn uptime_secs(mut self, secs: u64) -> Self {
        self.uptime_secs = Some(secs);
        self
    }

    /// Consume the builder and produce a [`HealthCheck`].
    #[must_use]
    pub fn build(self) -> HealthCheck {
        HealthCheck {
            service: self.service,
            status: self.status,
            version: self.version,
            uptime_secs: self.uptime_secs,
        }
    }
}

impl fmt::Display for HealthCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} v{} — {}", self.service, self.version, self.status)?;
        if let Some(uptime) = self.uptime_secs {
            write!(f, " (uptime: {uptime}s)")?;
        }
        Ok(())
    }
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degraded(r) => write!(f, "degraded: {r}"),
            Self::Unhealthy(r) => write!(f, "unhealthy: {r}"),
        }
    }
}

/// Parse error returned when a string cannot be interpreted as a [`HealthStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseHealthStatusError {
    input: String,
}

impl fmt::Display for ParseHealthStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid health status: {:?}", self.input)
    }
}

impl std::error::Error for ParseHealthStatusError {}

impl FromStr for HealthStatus {
    type Err = ParseHealthStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "healthy" {
            return Ok(Self::Healthy);
        }
        if let Some(reason) = s.strip_prefix("degraded: ") {
            return Ok(Self::Degraded(reason.to_string()));
        }
        if let Some(reason) = s.strip_prefix("unhealthy: ") {
            return Ok(Self::Unhealthy(reason.to_string()));
        }
        Err(ParseHealthStatusError {
            input: s.to_string(),
        })
    }
}

impl TryFrom<&str> for HealthStatus {
    type Error = ParseHealthStatusError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_check() {
        let hc = HealthCheck::healthy("tobira", "0.1.0");
        assert!(hc.is_healthy());
        assert!(!hc.is_degraded());
        assert!(!hc.is_unhealthy());
        assert_eq!(hc.service, "tobira");
        assert_eq!(hc.version, "0.1.0");
    }

    #[test]
    fn unhealthy_check() {
        let hc = HealthCheck::unhealthy("tobira", "0.1.0", "db down");
        assert!(!hc.is_healthy());
        assert!(hc.is_unhealthy());
        assert_eq!(hc.status, HealthStatus::Unhealthy("db down".to_string()));
    }

    #[test]
    fn degraded_check() {
        let hc = HealthCheck::degraded("tobira", "0.1.0", "slow index");
        assert!(!hc.is_healthy());
        assert!(hc.is_degraded());
    }

    #[test]
    fn with_uptime() {
        let hc = HealthCheck::healthy("tobira", "0.1.0").with_uptime(3600);
        assert_eq!(hc.uptime_secs, Some(3600));
    }

    #[test]
    fn display_healthy() {
        let hc = HealthCheck::healthy("tobira", "0.1.0");
        let s = hc.to_string();
        assert!(s.contains("tobira"));
        assert!(s.contains("healthy"));
    }

    #[test]
    fn display_with_uptime() {
        let hc = HealthCheck::healthy("tobira", "0.1.0").with_uptime(120);
        let s = hc.to_string();
        assert!(s.contains("uptime: 120s"));
    }

    #[test]
    fn display_unhealthy() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "connection refused");
        let s = hc.to_string();
        assert!(s.contains("unhealthy"));
        assert!(s.contains("connection refused"));
    }

    #[test]
    fn serde_roundtrip_healthy() {
        let hc = HealthCheck::healthy("tobira", "0.1.0").with_uptime(60);
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_healthy());
        assert_eq!(deserialized.service, "tobira");
        assert_eq!(deserialized.uptime_secs, Some(60));
    }

    #[test]
    fn serde_roundtrip_degraded() {
        let hc = HealthCheck::degraded("svc", "1.0", "slow");
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_degraded());
    }

    #[test]
    fn to_json_produces_valid_json() {
        let hc = HealthCheck::healthy("tobira", "0.1.0");
        let json = hc.to_json().unwrap();
        let _: serde_json::Value = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn health_status_display() {
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
        assert_eq!(
            HealthStatus::Degraded("slow".to_string()).to_string(),
            "degraded: slow"
        );
        assert_eq!(
            HealthStatus::Unhealthy("down".to_string()).to_string(),
            "unhealthy: down"
        );
    }

    #[test]
    fn health_status_equality() {
        assert_eq!(HealthStatus::Healthy, HealthStatus::Healthy);
        assert_ne!(HealthStatus::Healthy, HealthStatus::Unhealthy("x".into()));
    }

    // --- Additional tests ---

    #[test]
    fn healthy_has_no_uptime_by_default() {
        let hc = HealthCheck::healthy("svc", "1.0");
        assert_eq!(hc.uptime_secs, None);
    }

    #[test]
    fn unhealthy_has_no_uptime_by_default() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "crashed");
        assert_eq!(hc.uptime_secs, None);
    }

    #[test]
    fn degraded_has_no_uptime_by_default() {
        let hc = HealthCheck::degraded("svc", "1.0", "slow");
        assert_eq!(hc.uptime_secs, None);
    }

    #[test]
    fn with_uptime_chainable() {
        let hc = HealthCheck::healthy("svc", "1.0")
            .with_uptime(100)
            .with_uptime(200);
        assert_eq!(hc.uptime_secs, Some(200));
    }

    #[test]
    fn with_uptime_zero_is_valid() {
        let hc = HealthCheck::healthy("svc", "1.0").with_uptime(0);
        assert_eq!(hc.uptime_secs, Some(0));
    }

    #[test]
    fn with_uptime_max_u64() {
        let hc = HealthCheck::healthy("svc", "1.0").with_uptime(u64::MAX);
        assert_eq!(hc.uptime_secs, Some(u64::MAX));
    }

    #[test]
    fn degraded_is_not_healthy_or_unhealthy() {
        let hc = HealthCheck::degraded("svc", "1.0", "slow query");
        assert!(hc.is_degraded());
        assert!(!hc.is_healthy());
        assert!(!hc.is_unhealthy());
    }

    #[test]
    fn unhealthy_is_not_healthy_or_degraded() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "crash");
        assert!(hc.is_unhealthy());
        assert!(!hc.is_healthy());
        assert!(!hc.is_degraded());
    }

    #[test]
    fn display_degraded() {
        let hc = HealthCheck::degraded("myapp", "2.0.1", "high latency");
        let s = hc.to_string();
        assert!(s.contains("myapp"));
        assert!(s.contains("2.0.1"));
        assert!(s.contains("degraded"));
        assert!(s.contains("high latency"));
    }

    #[test]
    fn display_healthy_no_uptime_has_no_uptime_text() {
        let hc = HealthCheck::healthy("svc", "1.0");
        let s = hc.to_string();
        assert!(!s.contains("uptime"));
    }

    #[test]
    fn display_format_with_version() {
        let hc = HealthCheck::healthy("mado", "0.3.7");
        let s = hc.to_string();
        assert!(
            s.contains("v0.3.7"),
            "display should show 'v' prefix on version: {s}"
        );
    }

    #[test]
    fn serde_roundtrip_unhealthy() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "timeout");
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_unhealthy());
        assert_eq!(deserialized.service, "svc");
        assert_eq!(deserialized.version, "1.0");
        match &deserialized.status {
            HealthStatus::Unhealthy(reason) => assert_eq!(reason, "timeout"),
            other => panic!("expected Unhealthy, got {other:?}"),
        }
    }

    #[test]
    fn serde_roundtrip_with_uptime() {
        let hc = HealthCheck::healthy("svc", "1.0").with_uptime(42);
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.uptime_secs, Some(42));
    }

    #[test]
    fn serde_roundtrip_without_uptime() {
        let hc = HealthCheck::healthy("svc", "1.0");
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.uptime_secs, None);
    }

    #[test]
    fn to_json_contains_service_field() {
        let hc = HealthCheck::healthy("myservice", "0.1.0");
        let json = hc.to_json().unwrap();
        assert!(json.contains("\"service\""));
        assert!(json.contains("myservice"));
    }

    #[test]
    fn to_json_contains_status_field() {
        let hc = HealthCheck::healthy("svc", "1.0");
        let json = hc.to_json().unwrap();
        assert!(json.contains("\"status\""));
        assert!(json.contains("Healthy"));
    }

    #[test]
    fn to_json_unhealthy_contains_reason() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "disk full");
        let json = hc.to_json().unwrap();
        assert!(json.contains("disk full"));
    }

    #[test]
    fn to_json_degraded_contains_reason() {
        let hc = HealthCheck::degraded("svc", "1.0", "memory pressure");
        let json = hc.to_json().unwrap();
        assert!(json.contains("memory pressure"));
    }

    #[test]
    fn health_status_clone() {
        let status = HealthStatus::Degraded("reason".to_string());
        let cloned = status.clone();
        assert_eq!(status, cloned);
    }

    #[test]
    fn health_check_clone() {
        let hc = HealthCheck::healthy("svc", "1.0").with_uptime(10);
        let cloned = hc.clone();
        assert_eq!(cloned.service, hc.service);
        assert_eq!(cloned.version, hc.version);
        assert_eq!(cloned.status, hc.status);
        assert_eq!(cloned.uptime_secs, hc.uptime_secs);
    }

    #[test]
    fn health_status_debug_format() {
        let status = HealthStatus::Healthy;
        let debug = format!("{status:?}");
        assert!(debug.contains("Healthy"));
    }

    #[test]
    fn health_check_debug_format() {
        let hc = HealthCheck::healthy("svc", "1.0");
        let debug = format!("{hc:?}");
        assert!(debug.contains("svc"));
        assert!(debug.contains("Healthy"));
    }

    #[test]
    fn degraded_status_equality_same_reason() {
        let a = HealthStatus::Degraded("slow".to_string());
        let b = HealthStatus::Degraded("slow".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn degraded_status_inequality_different_reasons() {
        let a = HealthStatus::Degraded("slow".to_string());
        let b = HealthStatus::Degraded("timeout".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn unhealthy_status_equality_same_reason() {
        let a = HealthStatus::Unhealthy("crash".to_string());
        let b = HealthStatus::Unhealthy("crash".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn unhealthy_status_inequality_different_reasons() {
        let a = HealthStatus::Unhealthy("crash".to_string());
        let b = HealthStatus::Unhealthy("oom".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn degraded_not_equal_to_unhealthy() {
        let a = HealthStatus::Degraded("reason".to_string());
        let b = HealthStatus::Unhealthy("reason".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn empty_service_name() {
        let hc = HealthCheck::healthy("", "1.0");
        assert_eq!(hc.service, "");
        assert!(hc.is_healthy());
    }

    #[test]
    fn empty_version() {
        let hc = HealthCheck::healthy("svc", "");
        assert_eq!(hc.version, "");
    }

    #[test]
    fn empty_reason_degraded() {
        let hc = HealthCheck::degraded("svc", "1.0", "");
        assert!(hc.is_degraded());
        assert_eq!(hc.status, HealthStatus::Degraded(String::new()));
    }

    #[test]
    fn empty_reason_unhealthy() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "");
        assert!(hc.is_unhealthy());
        assert_eq!(hc.status, HealthStatus::Unhealthy(String::new()));
    }

    #[test]
    fn unicode_service_name() {
        let hc = HealthCheck::healthy("繋ぐ", "0.1.0");
        assert_eq!(hc.service, "繋ぐ");
        let json = hc.to_json().unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.service, "繋ぐ");
    }

    #[test]
    fn unicode_reason() {
        let hc = HealthCheck::unhealthy("svc", "1.0", "接続エラー");
        let json = serde_json::to_string(&hc).unwrap();
        let deserialized: HealthCheck = serde_json::from_str(&json).unwrap();
        match &deserialized.status {
            HealthStatus::Unhealthy(reason) => assert_eq!(reason, "接続エラー"),
            other => panic!("expected Unhealthy, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_from_known_json_structure() {
        // Verify that the serde format is stable and expected
        let json = r#"{
            "service": "test",
            "status": "Healthy",
            "version": "1.0",
            "uptime_secs": null
        }"#;
        let hc: HealthCheck = serde_json::from_str(json).unwrap();
        assert!(hc.is_healthy());
        assert_eq!(hc.service, "test");
        assert_eq!(hc.uptime_secs, None);
    }

    #[test]
    fn deserialize_degraded_from_json() {
        let json = r#"{
            "service": "test",
            "status": {"Degraded": "slow query"},
            "version": "2.0",
            "uptime_secs": 300
        }"#;
        let hc: HealthCheck = serde_json::from_str(json).unwrap();
        assert!(hc.is_degraded());
        assert_eq!(hc.uptime_secs, Some(300));
    }

    #[test]
    fn deserialize_unhealthy_from_json() {
        let json = r#"{
            "service": "test",
            "status": {"Unhealthy": "disk full"},
            "version": "3.0",
            "uptime_secs": null
        }"#;
        let hc: HealthCheck = serde_json::from_str(json).unwrap();
        assert!(hc.is_unhealthy());
    }

    #[test]
    fn health_check_equality_same() {
        let a = HealthCheck::healthy("svc", "1.0").with_uptime(10);
        let b = HealthCheck::healthy("svc", "1.0").with_uptime(10);
        assert_eq!(a, b);
    }

    #[test]
    fn health_check_inequality_different_status() {
        let a = HealthCheck::healthy("svc", "1.0");
        let b = HealthCheck::unhealthy("svc", "1.0", "down");
        assert_ne!(a, b);
    }

    #[test]
    fn health_check_inequality_different_service() {
        let a = HealthCheck::healthy("svc-a", "1.0");
        let b = HealthCheck::healthy("svc-b", "1.0");
        assert_ne!(a, b);
    }

    #[test]
    fn health_check_inequality_different_version() {
        let a = HealthCheck::healthy("svc", "1.0");
        let b = HealthCheck::healthy("svc", "2.0");
        assert_ne!(a, b);
    }

    #[test]
    fn health_check_inequality_different_uptime() {
        let a = HealthCheck::healthy("svc", "1.0").with_uptime(10);
        let b = HealthCheck::healthy("svc", "1.0").with_uptime(20);
        assert_ne!(a, b);
    }

    #[test]
    fn health_check_inequality_uptime_some_vs_none() {
        let a = HealthCheck::healthy("svc", "1.0");
        let b = HealthCheck::healthy("svc", "1.0").with_uptime(0);
        assert_ne!(a, b);
    }

    #[test]
    fn serde_roundtrip_preserves_equality() {
        let original = HealthCheck::degraded("svc", "3.0", "high load").with_uptime(999);
        let json = serde_json::to_string(&original).unwrap();
        let restored: HealthCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn fromstr_healthy_roundtrips() {
        let status = HealthStatus::Healthy;
        let parsed: HealthStatus = status.to_string().parse().unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn fromstr_degraded_roundtrips() {
        let status = HealthStatus::Degraded("slow query".to_string());
        let parsed: HealthStatus = status.to_string().parse().unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn fromstr_unhealthy_roundtrips() {
        let status = HealthStatus::Unhealthy("disk full".to_string());
        let parsed: HealthStatus = status.to_string().parse().unwrap();
        assert_eq!(parsed, status);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        let result = "not a status".parse::<HealthStatus>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not a status"));
    }

    #[test]
    fn fromstr_rejects_empty_string() {
        assert!("".parse::<HealthStatus>().is_err());
    }

    #[test]
    fn parse_health_status_error_is_std_error() {
        let err: Box<dyn std::error::Error> = Box::new("bad".parse::<HealthStatus>().unwrap_err());
        assert!(err.to_string().contains("bad"));
    }

    #[test]
    fn health_check_usable_in_hash_set() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(HealthCheck::healthy("a", "1.0"));
        set.insert(HealthCheck::healthy("b", "1.0"));
        set.insert(HealthCheck::healthy("a", "1.0"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn health_status_usable_in_hash_set() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(HealthStatus::Healthy);
        set.insert(HealthStatus::Degraded("slow".into()));
        set.insert(HealthStatus::Healthy);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn status_reason_healthy_is_none() {
        assert_eq!(HealthStatus::Healthy.reason(), None);
    }

    #[test]
    fn status_reason_degraded() {
        let s = HealthStatus::Degraded("slow".into());
        assert_eq!(s.reason(), Some("slow"));
    }

    #[test]
    fn status_reason_unhealthy() {
        let s = HealthStatus::Unhealthy("crash".into());
        assert_eq!(s.reason(), Some("crash"));
    }

    #[test]
    fn status_is_healthy() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Degraded("x".into()).is_healthy());
    }

    #[test]
    fn status_is_degraded() {
        assert!(HealthStatus::Degraded("x".into()).is_degraded());
        assert!(!HealthStatus::Healthy.is_degraded());
    }

    #[test]
    fn status_is_unhealthy() {
        assert!(HealthStatus::Unhealthy("x".into()).is_unhealthy());
        assert!(!HealthStatus::Healthy.is_unhealthy());
    }

    #[test]
    fn try_from_str_healthy() {
        let status = HealthStatus::try_from("healthy").unwrap();
        assert_eq!(status, HealthStatus::Healthy);
    }

    #[test]
    fn try_from_str_rejects_garbage() {
        assert!(HealthStatus::try_from("nope").is_err());
    }

    #[test]
    fn health_status_default_is_healthy() {
        assert_eq!(HealthStatus::default(), HealthStatus::Healthy);
    }

    #[test]
    fn builder_defaults_to_healthy() {
        let hc = HealthCheck::builder("svc", "1.0").build();
        assert!(hc.is_healthy());
        assert_eq!(hc.service, "svc");
        assert_eq!(hc.version, "1.0");
        assert_eq!(hc.uptime_secs, None);
    }

    #[test]
    fn builder_with_status_and_uptime() {
        let hc = HealthCheck::builder("svc", "2.0")
            .status(HealthStatus::Degraded("slow".into()))
            .uptime_secs(300)
            .build();
        assert!(hc.is_degraded());
        assert_eq!(hc.uptime_secs, Some(300));
    }

    #[test]
    fn builder_unhealthy() {
        let hc = HealthCheck::builder("svc", "1.0")
            .status(HealthStatus::Unhealthy("crash".into()))
            .build();
        assert!(hc.is_unhealthy());
    }

    #[test]
    fn from_json_roundtrip() {
        let original = HealthCheck::healthy("svc", "1.0").with_uptime(42);
        let json = original.to_json().unwrap();
        let restored = HealthCheck::from_json(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn from_json_rejects_garbage() {
        let result = HealthCheck::from_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn from_json_returns_tsunagu_error() {
        let err = HealthCheck::from_json("{bad}").unwrap_err();
        assert!(err.to_string().contains("serialization error"));
    }

    #[test]
    fn builder_clone() {
        let b = HealthCheck::builder("svc", "1.0").uptime_secs(10);
        let hc1 = b.clone().status(HealthStatus::Healthy).build();
        let hc2 = b.status(HealthStatus::Unhealthy("x".into())).build();
        assert!(hc1.is_healthy());
        assert!(hc2.is_unhealthy());
        assert_eq!(hc1.uptime_secs, Some(10));
        assert_eq!(hc2.uptime_secs, Some(10));
    }

    // ----------------------------------------------------------------
    // HealthChecker trait + SimpleHealthChecker
    // ----------------------------------------------------------------

    #[test]
    fn simple_checker_starts_healthy() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        assert!(checker.check().is_healthy());
    }

    #[test]
    fn simple_checker_service_name() {
        let checker = SimpleHealthChecker::new("myapp", "2.0");
        assert_eq!(checker.service_name(), "myapp");
    }

    #[test]
    fn simple_checker_version() {
        let checker = SimpleHealthChecker::new("myapp", "2.0");
        assert_eq!(checker.version(), "2.0");
    }

    #[test]
    fn simple_checker_set_degraded() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        checker.set_degraded();
        assert!(checker.check().is_degraded());
    }

    #[test]
    fn simple_checker_set_unhealthy() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        checker.set_unhealthy();
        assert!(checker.check().is_unhealthy());
    }

    #[test]
    fn simple_checker_set_healthy_after_unhealthy() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        checker.set_unhealthy();
        assert!(checker.check().is_unhealthy());
        checker.set_healthy();
        assert!(checker.check().is_healthy());
    }

    #[test]
    fn simple_checker_set_healthy_after_degraded() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        checker.set_degraded();
        assert!(checker.check().is_degraded());
        checker.set_healthy();
        assert!(checker.check().is_healthy());
    }

    #[test]
    fn simple_checker_cycle_all_states() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        assert!(checker.check().is_healthy());
        checker.set_degraded();
        assert!(checker.check().is_degraded());
        checker.set_unhealthy();
        assert!(checker.check().is_unhealthy());
        checker.set_healthy();
        assert!(checker.check().is_healthy());
    }

    #[test]
    fn simple_checker_debug_format() {
        let checker = SimpleHealthChecker::new("svc", "1.0");
        let debug = format!("{checker:?}");
        assert!(debug.contains("SimpleHealthChecker"));
        assert!(debug.contains("svc"));
    }

    #[test]
    fn health_checker_as_trait_object() {
        let checker: Box<dyn HealthChecker> = Box::new(SimpleHealthChecker::new("svc", "1.0"));
        assert!(checker.check().is_healthy());
        assert_eq!(checker.service_name(), "svc");
        assert_eq!(checker.version(), "1.0");
    }

    #[test]
    fn health_checker_trait_object_degraded() {
        let simple = SimpleHealthChecker::new("svc", "1.0");
        simple.set_degraded();
        let checker: Box<dyn HealthChecker> = Box::new(simple);
        assert!(checker.check().is_degraded());
    }

    #[test]
    fn health_checker_trait_object_unhealthy() {
        let simple = SimpleHealthChecker::new("svc", "1.0");
        simple.set_unhealthy();
        let checker: Box<dyn HealthChecker> = Box::new(simple);
        assert!(checker.check().is_unhealthy());
    }

    #[test]
    fn health_checker_trait_object_dispatch() {
        // Verify dynamic dispatch works with multiple implementations
        struct AlwaysDegraded;
        impl HealthChecker for AlwaysDegraded {
            fn check(&self) -> HealthStatus {
                HealthStatus::Degraded("always".to_string())
            }
            #[allow(clippy::unnecessary_literal_bound)]
            fn service_name(&self) -> &str {
                "degraded-svc"
            }
            #[allow(clippy::unnecessary_literal_bound)]
            fn version(&self) -> &str {
                "0.0.1"
            }
        }

        let checkers: Vec<Box<dyn HealthChecker>> = vec![
            Box::new(SimpleHealthChecker::new("simple", "1.0")),
            Box::new(AlwaysDegraded),
        ];

        assert!(checkers[0].check().is_healthy());
        assert!(checkers[1].check().is_degraded());
        assert_eq!(checkers[0].service_name(), "simple");
        assert_eq!(checkers[1].service_name(), "degraded-svc");
    }

    #[test]
    fn process_checker_trait_object_dispatch() {
        use crate::daemon::ProcessChecker;

        struct AlwaysAlive;
        impl ProcessChecker for AlwaysAlive {
            fn is_alive(&self, _pid: u32) -> bool {
                true
            }
        }

        struct NeverAlive;
        impl ProcessChecker for NeverAlive {
            fn is_alive(&self, _pid: u32) -> bool {
                false
            }
        }

        let checkers: Vec<Box<dyn ProcessChecker>> =
            vec![Box::new(AlwaysAlive), Box::new(NeverAlive)];

        assert!(checkers[0].is_alive(1));
        assert!(!checkers[1].is_alive(1));
    }

    #[test]
    fn simple_checker_unicode_names() {
        let checker = SimpleHealthChecker::new("繋ぐ", "0.1.0");
        assert_eq!(checker.service_name(), "繋ぐ");
        assert_eq!(checker.version(), "0.1.0");
        assert!(checker.check().is_healthy());
    }

    #[test]
    fn simple_checker_empty_names() {
        let checker = SimpleHealthChecker::new("", "");
        assert_eq!(checker.service_name(), "");
        assert_eq!(checker.version(), "");
        assert!(checker.check().is_healthy());
    }
}
