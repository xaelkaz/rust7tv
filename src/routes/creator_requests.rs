//! Streamer requests sent from the app, reviewed in the admin dashboard.
//!
//! Users submit a Twitch channel (plus an optional 7TV link). Requests are
//! deduplicated by channel and counted once per app install, so the dashboard
//! can sort by demand. Syncing a user whose folder matches a pending request
//! approves it (see `approve_matching_request`). Each install can read back the
//! status of its own requests through `/api/creator-requests/mine`.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;

const RATE_LIMIT_MAX_REQUESTS: i64 = 20;
const RATE_LIMIT_WINDOW_SECONDS: i64 = 3600;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCreatorRequest {
    pub channel_name: String,
    pub seven_tv_url: Option<String>,
    pub device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCreatorResponse {
    pub success: bool,
    /// "received", "already_requested" or "available"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_count: Option<i32>,
    /// Set when the creator is already in the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl CreateCreatorResponse {
    fn error(message: &str) -> Self {
        Self {
            success: false,
            outcome: None,
            request_count: None,
            folder_name: None,
            message: Some(message.to_string()),
        }
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CreatorRequestRecord {
    pub id: i32,
    pub channel_name: String,
    pub seven_tv_user_id: Option<String>,
    pub status: String,
    pub request_count: i32,
    pub created_at: DateTime<Utc>,
    pub last_requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatorRequestsListResponse {
    pub success: bool,
    pub requests: Vec<CreatorRequestRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyCreatorRequestsQuery {
    pub device_id: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MyCreatorRequestRecord {
    pub channel_name: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct MyCreatorRequestsResponse {
    pub success: bool,
    pub requests: Vec<MyCreatorRequestRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListCreatorRequestsQuery {
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCreatorRequestStatus {
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct UpdateCreatorRequestResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Strips an optional http(s) scheme and the given host prefixes, case-insensitively.
/// Returns the first path segment after `host_and_path`, or None if the host doesn't match.
fn path_segment_after<'a>(value: &'a str, prefixes: &[&str], host_and_path: &str) -> Option<&'a str> {
    // `get` instead of slicing: user input may put a multi-byte char at the cut point.
    fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
        s.get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &s[prefix.len()..])
    }

    let mut rest = value;
    if let Some(stripped) = ["https://", "http://"].iter().find_map(|p| strip_ci(rest, p)) {
        rest = stripped;
    }
    if let Some(stripped) = prefixes.iter().find_map(|p| strip_ci(rest, p)) {
        rest = stripped;
    }
    strip_ci(rest, host_and_path)?.split(['/', '?', '#']).next()
}

fn looks_like_url(value: &str) -> bool {
    value.contains('/') || value.contains(':')
}

/// Twitch logins are 4-25 chars of [A-Za-z0-9_]; stored lowercase.
/// Accepts "name", "@name" or a twitch.tv link (with or without scheme).
pub fn normalize_channel_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches('@');
    let channel = if looks_like_url(trimmed) {
        path_segment_after(trimmed, &["www.", "m."], "twitch.tv/")?
    } else {
        trimmed
    };

    let valid = (4..=25).contains(&channel.len())
        && channel.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| channel.to_ascii_lowercase())
}

/// Accepts a 7TV profile link (`https://7tv.app/users/<id>`) or a bare user ID.
/// Returns `Ok(None)` for an empty value and `Err(())` for anything unrecognized.
pub fn parse_seven_tv_user_id(raw: Option<&str>) -> Result<Option<String>, ()> {
    let Some(value) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    let candidate = if looks_like_url(value) {
        path_segment_after(value, &["www."], "7tv.app/users/").ok_or(())?
    } else {
        value
    };

    // Legacy ObjectIds are 24 hex chars; current IDs are 26-char ULIDs.
    let valid = (20..=32).contains(&candidate.len())
        && candidate.chars().all(|c| c.is_ascii_alphanumeric());
    if valid {
        Ok(Some(candidate.to_string()))
    } else {
        Err(())
    }
}

/// The app sends a random per-install UUID; it is only used to dedupe votes.
pub fn valid_device_id(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn valid_status(s: &str) -> bool {
    matches!(s, "pending" | "approved" | "rejected")
}

/// Railway's proxy puts the client address first in X-Forwarded-For.
pub(super) fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split(',').next())
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub async fn create_creator_request_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateCreatorRequest>,
) -> (StatusCode, Json<CreateCreatorResponse>) {
    let Some(channel) = normalize_channel_name(&payload.channel_name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(CreateCreatorResponse::error(
                "Invalid channel name: must be 4-25 chars of [A-Za-z0-9_]",
            )),
        );
    };
    let Ok(seven_tv_user_id) = parse_seven_tv_user_id(payload.seven_tv_url.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(CreateCreatorResponse::error("Invalid 7TV link")),
        );
    };
    if !valid_device_id(&payload.device_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(CreateCreatorResponse::error("Invalid device id")),
        );
    }

    let rate_key = format!("rate:creator_requests:{}", client_ip(&headers));
    match state
        .cache
        .increment_with_ttl(&rate_key, RATE_LIMIT_WINDOW_SECONDS)
        .await
    {
        Ok(count) if count > RATE_LIMIT_MAX_REQUESTS => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(CreateCreatorResponse::error("Too many requests, try again later")),
            );
        }
        Ok(_) => {}
        // Fail open: the per-device vote dedupe still bounds the damage.
        Err(e) => tracing::warn!("Creator request rate limit unavailable: {:?}", e),
    }

    match record_request(&state.db, &channel, seven_tv_user_id.as_deref(), &payload.device_id).await
    {
        Ok(response) => (StatusCode::OK, Json(response)),
        Err(e) => {
            tracing::error!("Failed to record creator request for {}: {:?}", channel, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(CreateCreatorResponse::error("Could not save the request")),
            )
        }
    }
}

async fn record_request(
    db: &sqlx::Pool<sqlx::Postgres>,
    channel: &str,
    seven_tv_user_id: Option<&str>,
    device_id: &str,
) -> Result<CreateCreatorResponse, sqlx::Error> {
    let existing_folder: Option<String> =
        sqlx::query_scalar("SELECT folder_name FROM users WHERE lower(folder_name) = $1 LIMIT 1")
            .bind(channel)
            .fetch_optional(db)
            .await?;
    if let Some(folder_name) = existing_folder {
        return Ok(CreateCreatorResponse {
            success: true,
            outcome: Some("available"),
            request_count: None,
            folder_name: Some(folder_name),
            message: None,
        });
    }

    let mut tx = db.begin().await?;

    let request_id: i32 = sqlx::query_scalar(
        "INSERT INTO creator_requests (channel_name, seven_tv_user_id)
         VALUES ($1, $2)
         ON CONFLICT (channel_name) DO UPDATE
             SET seven_tv_user_id = COALESCE(creator_requests.seven_tv_user_id, EXCLUDED.seven_tv_user_id)
         RETURNING id",
    )
    .bind(channel)
    .bind(seven_tv_user_id)
    .fetch_one(&mut *tx)
    .await?;

    let new_vote = sqlx::query(
        "INSERT INTO creator_request_votes (request_id, device_id)
         VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(request_id)
    .bind(device_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;

    let request_count: i32 = if new_vote {
        sqlx::query_scalar(
            "UPDATE creator_requests
             SET request_count = request_count + 1, last_requested_at = CURRENT_TIMESTAMP
             WHERE id = $1
             RETURNING request_count",
        )
        .bind(request_id)
        .fetch_one(&mut *tx)
        .await?
    } else {
        sqlx::query_scalar("SELECT request_count FROM creator_requests WHERE id = $1")
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await?
    };

    tx.commit().await?;

    Ok(CreateCreatorResponse {
        success: true,
        outcome: Some(if new_vote { "received" } else { "already_requested" }),
        request_count: Some(request_count),
        folder_name: None,
        message: None,
    })
}

/// Status of the requests a device voted for, so the app can show approved / rejected ones.
/// The device id is the random per-install UUID the app already sends with each request.
///
/// A vote cast after a rejection hasn't been reviewed yet, so that device sees the request as
/// pending; rejecting it again (which moves `resolved_at`) makes it rejected for everyone.
pub async fn my_creator_requests_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MyCreatorRequestsQuery>,
) -> (StatusCode, Json<MyCreatorRequestsResponse>) {
    if !valid_device_id(&params.device_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(MyCreatorRequestsResponse {
                success: false,
                requests: vec![],
                message: Some("Invalid device id".to_string()),
            }),
        );
    }

    let rows = sqlx::query_as::<_, MyCreatorRequestRecord>(
        "SELECT r.channel_name,
                CASE
                    WHEN r.status = 'rejected' AND v.created_at > r.resolved_at THEN 'pending'
                    ELSE r.status
                END AS status
         FROM creator_requests r
         JOIN creator_request_votes v ON v.request_id = r.id
         WHERE v.device_id = $1
         ORDER BY v.created_at DESC
         LIMIT 100",
    )
    .bind(&params.device_id)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(requests) => (
            StatusCode::OK,
            Json(MyCreatorRequestsResponse {
                success: true,
                requests,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to load creator requests for a device: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MyCreatorRequestsResponse {
                    success: false,
                    requests: vec![],
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

pub async fn list_creator_requests_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListCreatorRequestsQuery>,
) -> (StatusCode, Json<CreatorRequestsListResponse>) {
    let status = params.status.unwrap_or_else(|| "pending".to_string());
    if !valid_status(&status) {
        return (
            StatusCode::BAD_REQUEST,
            Json(CreatorRequestsListResponse {
                success: false,
                requests: vec![],
                message: Some("Invalid status".to_string()),
            }),
        );
    }

    let rows = sqlx::query_as::<_, CreatorRequestRecord>(
        "SELECT id, channel_name, seven_tv_user_id, status, request_count,
                created_at, last_requested_at, resolved_at
         FROM creator_requests
         WHERE status = $1
         ORDER BY request_count DESC, last_requested_at DESC
         LIMIT 500",
    )
    .bind(&status)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(requests) => (
            StatusCode::OK,
            Json(CreatorRequestsListResponse {
                success: true,
                requests,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to list creator requests: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(CreatorRequestsListResponse {
                    success: false,
                    requests: vec![],
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

pub async fn update_creator_request_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(payload): Json<UpdateCreatorRequestStatus>,
) -> (StatusCode, Json<UpdateCreatorRequestResponse>) {
    if !valid_status(&payload.status) {
        return (
            StatusCode::BAD_REQUEST,
            Json(UpdateCreatorRequestResponse {
                success: false,
                message: Some("Invalid status".to_string()),
            }),
        );
    }

    let result = sqlx::query(
        "UPDATE creator_requests
         SET status = $1,
             resolved_at = CASE WHEN $1 = 'pending' THEN NULL ELSE CURRENT_TIMESTAMP END
         WHERE id = $2",
    )
    .bind(&payload.status)
    .bind(id)
    .execute(&state.db)
    .await;

    match result {
        Ok(res) if res.rows_affected() == 0 => (
            StatusCode::NOT_FOUND,
            Json(UpdateCreatorRequestResponse {
                success: false,
                message: Some("Request not found".to_string()),
            }),
        ),
        Ok(_) => (
            StatusCode::OK,
            Json(UpdateCreatorRequestResponse {
                success: true,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to update creator request {}: {:?}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateCreatorRequestResponse {
                    success: false,
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

/// Marks the request for `folder_name` as approved once that creator is synced.
pub async fn approve_matching_request(db: &sqlx::Pool<sqlx::Postgres>, folder_name: &str) {
    let result = sqlx::query(
        "UPDATE creator_requests
         SET status = 'approved', resolved_at = CURRENT_TIMESTAMP
         WHERE channel_name = lower($1) AND status <> 'approved'",
    )
    .bind(folder_name)
    .execute(db)
    .await;

    if let Err(e) = result {
        tracing::warn!("Failed to approve creator request for {}: {:?}", folder_name, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_are_normalized() {
        assert_eq!(normalize_channel_name("  DN9N "), Some("dn9n".to_string()));
        assert_eq!(normalize_channel_name("@Bacond_"), Some("bacond_".to_string()));
        assert_eq!(
            normalize_channel_name("https://www.twitch.tv/Ibai?sr=a"),
            Some("ibai".to_string())
        );
        assert_eq!(
            normalize_channel_name("https://m.twitch.tv/auronplay/videos"),
            Some("auronplay".to_string())
        );
        assert_eq!(normalize_channel_name("twitch.tv/ElXokas"), Some("elxokas".to_string()));
    }

    #[test]
    fn invalid_channel_names_are_rejected() {
        assert_eq!(normalize_channel_name("abc"), None);
        assert_eq!(normalize_channel_name(&"a".repeat(26)), None);
        assert_eq!(normalize_channel_name("bad name"), None);
        assert_eq!(normalize_channel_name("<script>"), None);
        assert_eq!(normalize_channel_name("https://example.com/dn9n"), None);
        assert_eq!(normalize_channel_name("ñañañaña/x"), None);
        assert_eq!(normalize_channel_name("https://ñ.tv/x"), None);
    }

    #[test]
    fn seven_tv_links_and_ids_are_parsed() {
        let ulid = "01GDZTSXSG000AJC6Z7GSG0ANQ";
        assert_eq!(parse_seven_tv_user_id(None), Ok(None));
        assert_eq!(parse_seven_tv_user_id(Some("  ")), Ok(None));
        assert_eq!(parse_seven_tv_user_id(Some(ulid)), Ok(Some(ulid.to_string())));
        assert_eq!(
            parse_seven_tv_user_id(Some(&format!("https://7tv.app/users/{ulid}?tab=emotes"))),
            Ok(Some(ulid.to_string()))
        );
        assert_eq!(
            parse_seven_tv_user_id(Some(&format!("7tv.app/users/{ulid}"))),
            Ok(Some(ulid.to_string()))
        );
        assert_eq!(
            parse_seven_tv_user_id(Some("60ae3e98b2ecb0150535c6b7")),
            Ok(Some("60ae3e98b2ecb0150535c6b7".to_string()))
        );
    }

    #[test]
    fn unrelated_links_are_rejected() {
        assert_eq!(parse_seven_tv_user_id(Some("https://evil.example/users/01GDZTSXSG000AJC6Z7GSG0ANQ")), Err(()));
        assert_eq!(parse_seven_tv_user_id(Some("https://7tv.app/emotes/01GDZTSXSG000AJC6Z7GSG0ANQ")), Err(()));
        assert_eq!(parse_seven_tv_user_id(Some("short")), Err(()));
        assert_eq!(parse_seven_tv_user_id(Some("ñ7tv.app/users/x")), Err(()));
    }

    #[test]
    fn device_ids_are_bounded() {
        assert!(valid_device_id("3f2b8c1e-5d4a-4e7b-9c0d-1a2b3c4d5e6f"));
        assert!(!valid_device_id("short"));
        assert!(!valid_device_id(&"a".repeat(65)));
        assert!(!valid_device_id("has spaces in it"));
    }

    #[test]
    fn client_ip_uses_first_forwarded_address() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), "unknown");
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers), "203.0.113.7");
    }
}
