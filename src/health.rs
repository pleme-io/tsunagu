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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
        let status = match &self.status {
            HealthStatus::Healthy => "healthy".to_string(),
            HealthStatus::Degraded(r) => format!("degraded: {r}"),
            HealthStatus::Unhealthy(r) => format!("unhealthy: {r}"),
        };
        write!(f, "{} v{} — {}", self.service, self.version, status)?;
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
}
