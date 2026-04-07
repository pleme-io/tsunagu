use std::fmt;

/// Health status for a daemon service.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HealthStatus {
    /// Service is fully operational.
    Healthy,
    /// Service is running but with degraded functionality.
    Degraded(String),
    /// Service is not operational.
    Unhealthy(String),
}

/// Standardized health check response for daemon services.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        matches!(self.status, HealthStatus::Healthy)
    }

    /// Whether the service is degraded (running but impaired).
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        matches!(self.status, HealthStatus::Degraded(_))
    }

    /// Whether the service is unhealthy.
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        matches!(self.status, HealthStatus::Unhealthy(_))
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
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
        assert!(s.contains("v0.3.7"), "display should show 'v' prefix on version: {s}");
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
}
