//! Axum integration: `/health`, `/readiness`, `/liveness` router.
//!
//! Feature-gated behind `axum`. Non-axum consumers (CLI daemons without
//! an HTTP surface) don't pay the dep cost.
//!
//! Every pleme-io daemon with an HTTP surface (kindling, hanabi, shinka,
//! kenshi, hiroba, taimen) was mounting its own `/health` handler. The
//! mappings are identical:
//!
//! ```text
//! HealthStatus::Healthy   → HTTP 200 + serialized HealthCheck JSON
//! HealthStatus::Degraded  → HTTP 200 + serialized HealthCheck JSON
//! HealthStatus::Unhealthy → HTTP 503 + serialized HealthCheck JSON
//! ```
//!
//! Kubernetes + systemd distinguish readiness (takes traffic / fully up)
//! from liveness (process is functioning, restart if not). We expose both
//! — routed through the same [`HealthChecker`] — because the signal is
//! the same; the K8s probe config decides which endpoint to poll.
//!
//! # Usage
//!
//! ```no_run
//! # #[cfg(feature = "axum")]
//! # {
//! use std::sync::Arc;
//! use tsunagu::{HealthChecker, SimpleHealthChecker};
//! use tsunagu::axum::health_router;
//!
//! let checker: Arc<dyn HealthChecker> =
//!     Arc::new(SimpleHealthChecker::new("myapp", "1.0.0"));
//!
//! let app = ::axum::Router::new()
//!     .merge(health_router::<()>(checker));
//! // app now serves GET /health, /readiness, /liveness
//! # }
//! ```

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::health::{HealthChecker, HealthStatus};
use crate::HealthCheck;

/// Router with `/health`, `/readiness`, `/liveness` endpoints.
///
/// Generic over the outer router's state `S` so consumers can `.merge()`
/// this router into their app without state-type conflicts. The health
/// checker is captured in each handler via closure; the outer state
/// remains what the consumer already wired.
///
/// All three endpoints currently share the same handler — the distinction
/// lives in K8s probe config (which endpoint to poll, at what interval).
/// Daemons with genuinely different readiness vs liveness semantics
/// compose their own router with separate checkers.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "axum")]
/// # {
/// use std::sync::Arc;
/// use tsunagu::{HealthChecker, SimpleHealthChecker};
/// use tsunagu::axum::health_router;
///
/// #[derive(Clone)]
/// struct AppState;
///
/// let checker: Arc<dyn HealthChecker> =
///     Arc::new(SimpleHealthChecker::new("myapp", "1.0"));
///
/// let app: ::axum::Router<AppState> = ::axum::Router::<AppState>::new()
///     .merge(health_router::<AppState>(checker))
///     .with_state(AppState);
/// # }
/// ```
pub fn health_router<S>(checker: Arc<dyn HealthChecker>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let c1 = checker.clone();
    let c2 = checker.clone();
    let c3 = checker;
    Router::<S>::new()
        .route(
            "/health",
            get(move || {
                let c = c1.clone();
                async move { health_response(c).await }
            }),
        )
        .route(
            "/readiness",
            get(move || {
                let c = c2.clone();
                async move { health_response(c).await }
            }),
        )
        .route(
            "/liveness",
            get(move || {
                let c = c3.clone();
                async move { health_response(c).await }
            }),
        )
}

/// Response-building helper — serialize HealthCheck, map Unhealthy → 503.
///
/// Exposed `pub(crate)` for tests and `pub` for consumers that want to
/// inline a health handler with custom state wiring.
pub async fn health_response(checker: Arc<dyn HealthChecker>) -> Response {
    let status = checker.check();
    let service = checker.service_name();
    let version = checker.version();
    let (code, body) = match status {
        HealthStatus::Healthy => (
            StatusCode::OK,
            HealthCheck::healthy(service, version),
        ),
        HealthStatus::Degraded(reason) => (
            StatusCode::OK,
            HealthCheck::degraded(service, version, &reason),
        ),
        HealthStatus::Unhealthy(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            HealthCheck::unhealthy(service, version, &reason),
        ),
    };
    (code, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SimpleHealthChecker;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn checker() -> Arc<dyn HealthChecker> {
        Arc::new(SimpleHealthChecker::new("tsunagu-axum-test", "0.0.1"))
    }

    async fn send(
        app: Router<()>,
        path: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let code = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (code, json)
    }

    #[tokio::test]
    async fn health_endpoint_returns_200_when_healthy() {
        let app = health_router::<()>(checker());
        let (code, body) = send(app, "/health").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["service"], "tsunagu-axum-test");
        assert_eq!(body["version"], "0.0.1");
        // HealthStatus serializes as the variant name (serde default).
        assert_eq!(body["status"], "Healthy");
    }

    #[tokio::test]
    async fn readiness_endpoint_returns_200_when_healthy() {
        let app = health_router::<()>(checker());
        let (code, _) = send(app, "/readiness").await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn liveness_endpoint_returns_200_when_healthy() {
        let app = health_router::<()>(checker());
        let (code, _) = send(app, "/liveness").await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn degraded_state_still_returns_200() {
        let c = Arc::new(SimpleHealthChecker::new("svc", "1.0"));
        c.set_degraded();
        let app = health_router::<()>(c as Arc<dyn HealthChecker>);
        let (code, body) = send(app, "/health").await;
        assert_eq!(code, StatusCode::OK);
        // Tuple variant serializes as { "Degraded": "reason" }
        assert!(body["status"].get("Degraded").is_some());
    }

    #[tokio::test]
    async fn unhealthy_state_returns_503() {
        let c = Arc::new(SimpleHealthChecker::new("svc", "1.0"));
        c.set_unhealthy();
        let app = health_router::<()>(c as Arc<dyn HealthChecker>);
        let (code, body) = send(app, "/health").await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body["status"].get("Unhealthy").is_some());
    }

    #[tokio::test]
    async fn state_transitions_propagate_through_router() {
        let c = Arc::new(SimpleHealthChecker::new("svc", "1.0"));
        let arc: Arc<dyn HealthChecker> = c.clone() as Arc<dyn HealthChecker>;

        let app = health_router::<()>(arc.clone());
        let (code, _) = send(app, "/health").await;
        assert_eq!(code, StatusCode::OK);

        c.set_unhealthy();
        let app = health_router::<()>(arc.clone());
        let (code, _) = send(app, "/health").await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);

        c.set_healthy();
        let app = health_router::<()>(arc);
        let (code, _) = send(app, "/health").await;
        assert_eq!(code, StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let app = health_router::<()>(checker());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_carries_service_and_version_fields() {
        let app = health_router::<()>(checker());
        let (_, body) = send(app, "/health").await;
        // HealthCheck schema: { service, version, status, ... }
        assert!(body.get("service").is_some());
        assert!(body.get("version").is_some());
        assert!(body.get("status").is_some());
    }
}
