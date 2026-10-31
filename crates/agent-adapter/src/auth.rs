//! Bearer-token authentication as an axum middleware.
//!
//! The middleware is the only place a connection binding enters a request.
//! Tool handlers read it from the request extensions; they never accept an
//! actor or project from tool arguments.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::registry::{ConnectionBinding, ConnectionRegistry};

/// Reject a request whose `Authorization` header is missing or invalid.
///
/// On success the resolved binding is inserted into the request extensions,
/// where the MCP transport copies it (inside `http::request::Parts`) into the
/// tool handler's `RequestContext`.
pub async fn authorize(
    State(registry): State<Arc<ConnectionRegistry>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(binding) = bearer_binding(&registry, &request) else {
        return unauthorized();
    };
    request.extensions_mut().insert(binding);
    next.run(request).await
}

fn bearer_binding(registry: &ConnectionRegistry, request: &Request) -> Option<ConnectionBinding> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return None;
    }
    registry.resolve(token)
}

fn unauthorized() -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ConnectionScope, MIN_TOKEN_BYTES};
    use axum::body::Body;
    use reimagine_core::model::ProjectId;

    fn registry() -> (Arc<ConnectionRegistry>, String) {
        let registry = Arc::new(ConnectionRegistry::new());
        let binding = ConnectionBinding {
            id: "mcp-test".to_owned(),
            label: "MCP test".to_owned(),
            scope: ConnectionScope::Project(ProjectId::new("p1")),
        };
        let token = registry.issue(binding);
        (registry, token)
    }

    fn request(authorization: Option<&str>) -> Request {
        let mut builder = axum::http::Request::builder().uri("/mcp");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder.body(Body::empty()).expect("request")
    }

    #[test]
    fn parses_a_bearer_token_case_insensitively() {
        let (registry, token) = registry();
        let binding =
            bearer_binding(&registry, &request(Some(&format!("bEaReR {token}")))).expect("binding");
        assert_eq!(binding.id, "mcp-test");
    }

    #[test]
    fn rejects_missing_foreign_or_malformed_headers() {
        let (registry, token) = registry();
        assert!(bearer_binding(&registry, &request(None)).is_none());
        assert!(bearer_binding(&registry, &request(Some(&format!("Basic {token}")))).is_none());
        assert!(bearer_binding(&registry, &request(Some("Bearer "))).is_none());
        assert!(
            bearer_binding(
                &registry,
                &request(Some(&format!("Bearer {}", "0".repeat(MIN_TOKEN_BYTES))))
            )
            .is_none()
        );
    }
}
