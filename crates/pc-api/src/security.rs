//! Guards against DNS rebinding when the server is only meant to be reached
//! from the machine it runs on.
//!
//! Binding to a loopback address does not, by itself, keep a page open in an
//! ordinary browser tab from reaching this server. The attacker registers a
//! domain with a very short DNS TTL, has the victim load a page from it, then
//! re-points that same name at `127.0.0.1`. The follow-up request the page
//! makes is, as far as the browser is concerned, same-origin — no CORS
//! preflight applies, because CORS is keyed on the page's origin, not on
//! where the name actually resolves at request time. The one thing that
//! still carries the attacker's domain is the `Host` header: the browser
//! sets it from the name in the address bar, which rebinding never changes.
//! Checking it here closes the gap. It is only wired in when the listener
//! bound to loopback — a NAS bind is reachable from other machines by
//! design, and its operator already knows that.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

/// Reject anything whose `Host` header is not the loopback address or
/// `localhost`, both at the port actually listened on.
pub async fn require_loopback_host(local: SocketAddr, req: Request, next: Next) -> Response {
    if host_allowed(local, req.headers().get(axum::http::header::HOST)) {
        return next.run(req).await;
    }
    (
        StatusCode::MISDIRECTED_REQUEST,
        pc_core::tf!(
            "Отклонено: заголовок Host не совпадает с адресом {0}",
            "Rejected: the Host header does not match {0}",
            local,
        ),
    )
        .into_response()
}

fn host_allowed(local: SocketAddr, header: Option<&HeaderValue>) -> bool {
    let Some(value) = header.and_then(|h| h.to_str().ok()) else {
        return false;
    };
    // Bracketed IPv6 with a port ("[::1]:4317") still splits correctly here:
    // the last colon is the one before the port digits.
    let (name, port) = match value.rsplit_once(':') {
        Some((name, port)) => (name, port.parse::<u16>().ok()),
        None => (value, None),
    };
    let port_ok = match port {
        Some(p) => p == local.port(),
        // A client that trusts the default HTTP port omits it.
        None => local.port() == 80,
    };
    if !port_ok {
        return false;
    }
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let expected = if local.ip().is_ipv6() {
        format!("[{}]", local.ip())
    } else {
        local.ip().to_string()
    };
    name == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:4317".parse().unwrap()
    }

    fn header(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn accepts_the_loopback_ip_and_localhost_at_the_right_port() {
        assert!(host_allowed(addr(), Some(&header("127.0.0.1:4317"))));
        assert!(host_allowed(addr(), Some(&header("localhost:4317"))));
        assert!(host_allowed(addr(), Some(&header("LOCALHOST:4317"))));
    }

    #[test]
    fn rejects_a_rebound_domain_a_wrong_port_or_a_missing_header() {
        assert!(!host_allowed(addr(), Some(&header("evil.example:4317"))));
        assert!(!host_allowed(addr(), Some(&header("evil.example"))));
        assert!(!host_allowed(addr(), Some(&header("127.0.0.1:9999"))));
        assert!(!host_allowed(addr(), Some(&header("127.0.0.1"))));
        assert!(!host_allowed(addr(), None));
    }

    fn addr_v6() -> SocketAddr {
        "[::1]:4317".parse().unwrap()
    }

    #[test]
    fn accepts_bracketed_ipv6_loopback_and_localhost_at_the_right_port() {
        assert!(host_allowed(addr_v6(), Some(&header("[::1]:4317"))));
        assert!(host_allowed(addr_v6(), Some(&header("localhost:4317"))));
    }

    #[test]
    fn rejects_ipv6_host_without_a_port_or_with_the_wrong_port() {
        // No port: `rsplit_once(':')` on "[::1]" splits at the last `::`
        // colon, leaving a non-numeric "port" half, so this must not be
        // confused with a bare, portless host.
        assert!(!host_allowed(addr_v6(), Some(&header("[::1]"))));
        assert!(!host_allowed(addr_v6(), Some(&header("[::1]:9999"))));
    }

    #[test]
    fn rejects_the_ipv4_loopback_host_on_an_ipv6_bind() {
        assert!(!host_allowed(addr_v6(), Some(&header("127.0.0.1:4317"))));
    }

    #[tokio::test]
    async fn wired_into_the_real_router_rejects_a_foreign_host_and_passes_the_real_one() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let state = std::sync::Arc::new(
            crate::AppState::new(&root.join("test.db"), &root.join("thumbs"), None).unwrap(),
        );
        let local = addr();
        let app = crate::router(state).layer(axum::middleware::from_fn(move |req, next| {
            require_loopback_host(local, req, next)
        }));

        let rebound = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("host", "evil.example:4317")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rebound.status(), StatusCode::MISDIRECTED_REQUEST);

        let legit = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("host", "127.0.0.1:4317")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(legit.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ignores_x_forwarded_host_and_still_rejects_the_real_host() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let state = std::sync::Arc::new(
            crate::AppState::new(&root.join("test.db"), &root.join("thumbs"), None).unwrap(),
        );
        let local = addr();
        let app = crate::router(state).layer(axum::middleware::from_fn(move |req, next| {
            require_loopback_host(local, req, next)
        }));

        // A rebinding page can set arbitrary headers on its own fetch, so a
        // spoofed X-Forwarded-Host claiming the real address must not let a
        // foreign Host through.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("host", "evil.example:4317")
                    .header("x-forwarded-host", "127.0.0.1:4317")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    }
}
