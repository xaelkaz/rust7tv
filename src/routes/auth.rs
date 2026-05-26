use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::Arc;

use crate::AppState;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn check_bearer(header_value: &str, expected_token: &str) -> bool {
    header_value
        .strip_prefix("Bearer ")
        .map(|t| constant_time_eq(t.as_bytes(), expected_token.as_bytes()))
        .unwrap_or(false)
}

fn check_basic(header_value: &str, expected_user: &str, expected_pass: &str) -> bool {
    let Some(encoded) = header_value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded_bytes) = STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded_bytes) else {
        return false;
    };
    let Some((user, pass)) = decoded.split_once(':') else {
        return false;
    };
    constant_time_eq(user.as_bytes(), expected_user.as_bytes())
        && constant_time_eq(pass.as_bytes(), expected_pass.as_bytes())
}

/// Admin auth: accepts either `Authorization: Bearer <ADMIN_TOKEN>` (for
/// curl/scripts) or `Authorization: Basic <user:pass>` (so the browser can
/// authenticate the dashboard once and have all subsequent fetch() calls
/// to /api/admin/* be auto-authenticated by the same cached credentials).
pub async fn require_admin(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let header_value = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let authorized = match header_value {
        Some(h) => {
            check_bearer(h, &state.config.admin_token)
                || check_basic(h, &state.config.admin_user, &state.config.admin_password)
        }
        None => false,
    };

    if authorized {
        return next.run(req).await;
    }

    // Always advertise Basic so the browser pops the credential prompt for
    // the dashboard. Scripts that send Bearer just see a 401.
    let mut resp = (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(r#"Basic realm="admin", charset="UTF-8""#),
    );
    resp
}
