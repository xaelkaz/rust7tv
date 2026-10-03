//! Community packs: sticker packs users publish from the app.
//!
//! A published pack waits in the dashboard until the admin approves it; approved packs are
//! served to the app most liked first, or most recently approved first. Packs carry the same
//! random per-install id as creator requests, so each install can read back the review status
//! of its own packs, and likes and adds are counted once per device.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::creator_requests::{client_ip, valid_device_id};
use crate::models::EmoteResponse;
use crate::AppState;

/// WhatsApp rejects packs with fewer stickers than this.
const MIN_STICKERS: usize = 3;
/// WhatsApp's own maximum for a sticker pack.
const MAX_STICKERS: usize = 30;
const MAX_NAME_CHARS: usize = 40;
const MAX_AUTHOR_CHARS: usize = 24;
const MAX_EMOTE_NAME_CHARS: usize = 100;
/// Packs one install can have waiting for review at the same time.
const MAX_PENDING_PER_DEVICE: i64 = 5;
const PUBLISH_RATE_LIMIT: i64 = 10;
const ADD_RATE_LIMIT: i64 = 60;
const LIKE_RATE_LIMIT: i64 = 120;
const RATE_LIMIT_WINDOW_SECONDS: i64 = 3600;
/// Approved packs served to the app per request; the Community tab lists them all.
const PUBLIC_LIMIT: i64 = 50;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishInput {
    pub device_id: String,
    pub name: String,
    pub author_name: Option<String>,
    pub animated: bool,
    pub emotes: Vec<PublishEmoteInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishEmoteInput {
    pub emote_id: String,
    pub emote_name: String,
    pub animated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityPack {
    pub id: i32,
    pub name: String,
    pub author: Option<String>,
    pub animated: bool,
    pub status: String,
    pub add_count: i32,
    pub like_count: i32,
    /// Whether the install that asked (`deviceId`) likes the pack; false when it didn't say.
    pub liked_by_me: bool,
    pub created_at: DateTime<Utc>,
    pub emotes: Vec<EmoteResponse>,
}

#[derive(Debug, Serialize)]
pub struct CommunityPacksResponse {
    pub success: bool,
    pub packs: Vec<CommunityPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What an install knows about a pack it published.
#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MyCommunityPack {
    pub id: i32,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct PublishResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<MyCommunityPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MyCommunityPacksResponse {
    pub success: bool,
    pub packs: Vec<MyCommunityPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceQuery {
    pub device_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddInput {
    pub device_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicListQuery {
    /// "popular" (default) or "new"
    pub sort: Option<String>,
    /// Lets the app show which packs this install liked.
    pub device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeInput {
    pub device_id: String,
    /// True to like the pack, false to take the like back.
    pub liked: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// How a list of packs is ordered.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Order {
    /// Most liked first, then most added: the app's default view.
    Popular,
    /// Most recently approved first, so new packs get seen too.
    New,
    /// Most recently submitted first: the dashboard's review queue.
    Submitted,
}

impl Order {
    fn sql(self) -> &'static str {
        match self {
            Order::Popular => "like_count DESC, add_count DESC, reviewed_at DESC NULLS LAST, id DESC",
            Order::New => "reviewed_at DESC NULLS LAST, id DESC",
            Order::Submitted => "created_at DESC, id DESC",
        }
    }

    fn from_public(sort: Option<&str>) -> Order {
        match sort {
            Some("new") => Order::New,
            _ => Order::Popular,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    /// "pending" (default), "approved", "rejected" or "all"
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StatusInput {
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct SimpleResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PackRow {
    id: i32,
    name: String,
    author_name: Option<String>,
    animated: bool,
    status: String,
    add_count: i32,
    like_count: i32,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    pack_id: i32,
    emote_id: String,
    emote_name: String,
}

fn valid_status(s: &str) -> bool {
    matches!(s, "pending" | "approved" | "rejected")
}

/// The dashboard's status filter: None lists every pack ("all"), pending is the default.
fn admin_status_filter(raw: Option<&str>) -> Result<Option<&str>, ()> {
    match raw.unwrap_or("pending") {
        "all" => Ok(None),
        status if valid_status(status) => Ok(Some(status)),
        _ => Err(()),
    }
}

/// 7TV ids: legacy ObjectIds are 24 hex chars, current ones 26-char ULIDs.
fn valid_emote_id(id: &str) -> bool {
    (20..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Trimmed text of 1..=max chars without control characters (they would break the layout).
fn clean_text(raw: &str, max_chars: usize) -> Option<&str> {
    let text = raw.trim();
    let chars = text.chars().count();
    ((1..=max_chars).contains(&chars) && !text.chars().any(char::is_control)).then_some(text)
}

/// Checks a submission and returns its trimmed name and author.
pub fn validate_publish(input: &PublishInput) -> Result<(String, Option<String>), String> {
    if !valid_device_id(&input.device_id) {
        return Err("Invalid device id".to_string());
    }
    let name = clean_text(&input.name, MAX_NAME_CHARS)
        .ok_or_else(|| format!("Name must be 1-{MAX_NAME_CHARS} characters"))?;
    let author = match input.author_name.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        None => None,
        Some(raw) => Some(
            clean_text(raw, MAX_AUTHOR_CHARS)
                .ok_or_else(|| format!("Your name can have at most {MAX_AUTHOR_CHARS} characters"))?,
        ),
    };
    if !(MIN_STICKERS..=MAX_STICKERS).contains(&input.emotes.len()) {
        return Err(format!("A pack needs {MIN_STICKERS}-{MAX_STICKERS} stickers"));
    }

    let mut seen = HashSet::new();
    for emote in &input.emotes {
        if !valid_emote_id(&emote.emote_id) {
            return Err("A sticker has an invalid id".to_string());
        }
        if clean_text(&emote.emote_name, MAX_EMOTE_NAME_CHARS).is_none() {
            return Err("A sticker has an invalid name".to_string());
        }
        if emote.animated != input.animated {
            return Err("Animated and static stickers can't share a pack".to_string());
        }
        if !seen.insert(emote.emote_id.as_str()) {
            return Err("A sticker is in the pack twice".to_string());
        }
    }
    Ok((name.to_string(), author.map(str::to_string)))
}

/// Builds the sticker the app expects, pointing only at 7TV's CDN.
fn emote_response(emote_id: String, emote_name: String, animated: bool) -> EmoteResponse {
    let base = format!("https://cdn.7tv.app/emote/{emote_id}");
    EmoteResponse {
        file_name: format!("{emote_id}.webp"),
        url: format!("{base}/4x.webp"),
        animated_preview_url: animated.then(|| format!("{base}/2x.webp")),
        poster_url: Some(if animated {
            format!("{base}/4x_static.webp")
        } else {
            format!("{base}/4x.webp")
        }),
        emote_id,
        emote_name,
        owner: None,
        animated: Some(animated),
        scale: None,
        mime: None,
        tags: None,
    }
}

/// Packs with the given status (every pack when None), in the given order. With a device id,
/// each pack says whether that install likes it.
async fn load_packs(
    db: &sqlx::Pool<sqlx::Postgres>,
    status: Option<&str>,
    order: Order,
    limit: i64,
    device_id: Option<&str>,
) -> Result<Vec<CommunityPack>, sqlx::Error> {
    let order = order.sql();
    let packs = sqlx::query_as::<_, PackRow>(&format!(
        "SELECT id, name, author_name, animated, status, add_count, like_count, created_at
         FROM community_packs
         WHERE $1::text IS NULL OR status = $1
         ORDER BY {order}
         LIMIT $2"
    ))
    .bind(status)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let ids: Vec<i32> = packs.iter().map(|p| p.id).collect();
    let items = sqlx::query_as::<_, ItemRow>(
        "SELECT pack_id, emote_id, emote_name
         FROM community_pack_items
         WHERE pack_id = ANY($1)
         ORDER BY pack_id, position",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;

    let liked: HashSet<i32> = match device_id {
        Some(device_id) => sqlx::query_scalar(
            "SELECT pack_id FROM community_pack_likes WHERE device_id = $1 AND pack_id = ANY($2)",
        )
        .bind(device_id)
        .bind(&ids)
        .fetch_all(db)
        .await?
        .into_iter()
        .collect(),
        None => HashSet::new(),
    };

    let animated_by_pack: HashMap<i32, bool> = packs.iter().map(|p| (p.id, p.animated)).collect();
    let mut by_pack: HashMap<i32, Vec<EmoteResponse>> = HashMap::new();
    for item in items {
        let animated = animated_by_pack.get(&item.pack_id).copied().unwrap_or(false);
        by_pack
            .entry(item.pack_id)
            .or_default()
            .push(emote_response(item.emote_id, item.emote_name, animated));
    }

    Ok(packs
        .into_iter()
        .map(|p| CommunityPack {
            emotes: by_pack.remove(&p.id).unwrap_or_default(),
            id: p.id,
            name: p.name,
            author: p.author_name,
            animated: p.animated,
            status: p.status,
            add_count: p.add_count,
            like_count: p.like_count,
            liked_by_me: liked.contains(&p.id),
            created_at: p.created_at,
        })
        .collect())
}

/// Counts a request against a per-IP budget. Fails open: the per-device limits still apply.
async fn over_rate_limit(state: &AppState, bucket: &str, headers: &HeaderMap, max: i64) -> bool {
    let key = format!("rate:{bucket}:{}", client_ip(headers));
    match state.cache.increment_with_ttl(&key, RATE_LIMIT_WINDOW_SECONDS).await {
        Ok(count) => count > max,
        Err(e) => {
            tracing::warn!("Community pack rate limit unavailable: {:?}", e);
            false
        }
    }
}

fn publish_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<PublishResponse>) {
    (
        status,
        Json(PublishResponse {
            success: false,
            pack: None,
            message: Some(message.into()),
        }),
    )
}

/// Public: an install submits one of its packs for review.
pub async fn publish_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<PublishInput>,
) -> (StatusCode, Json<PublishResponse>) {
    let (name, author) = match validate_publish(&input) {
        Ok(cleaned) => cleaned,
        Err(message) => return publish_error(StatusCode::BAD_REQUEST, message),
    };
    if over_rate_limit(&state, "community_publish", &headers, PUBLISH_RATE_LIMIT).await {
        return publish_error(StatusCode::TOO_MANY_REQUESTS, "Too many packs, try again later");
    }

    let result: Result<Option<i32>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM community_packs WHERE device_id = $1 AND status = 'pending'",
        )
        .bind(&input.device_id)
        .fetch_one(&mut *tx)
        .await?;
        if pending >= MAX_PENDING_PER_DEVICE {
            return Ok(None);
        }

        let id: i32 = sqlx::query_scalar(
            "INSERT INTO community_packs (device_id, name, author_name, animated)
             VALUES ($1, $2, $3, $4)
             RETURNING id",
        )
        .bind(&input.device_id)
        .bind(&name)
        .bind(&author)
        .bind(input.animated)
        .fetch_one(&mut *tx)
        .await?;
        for (position, emote) in input.emotes.iter().enumerate() {
            sqlx::query(
                "INSERT INTO community_pack_items (pack_id, position, emote_id, emote_name)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(id)
            .bind(position as i32)
            .bind(&emote.emote_id)
            .bind(emote.emote_name.trim())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Some(id))
    }
    .await;

    match result {
        Ok(Some(id)) => (
            StatusCode::OK,
            Json(PublishResponse {
                success: true,
                pack: Some(MyCommunityPack {
                    id,
                    name,
                    status: "pending".to_string(),
                }),
                message: None,
            }),
        ),
        Ok(None) => publish_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many packs waiting for review, try again later",
        ),
        Err(e) => {
            tracing::error!("Failed to save community pack: {:?}", e);
            publish_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save the pack")
        }
    }
}

/// Public: approved packs, most liked first or (`?sort=new`) most recently approved first.
/// With `?deviceId=`, each pack also says whether that install likes it.
pub async fn list_approved_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PublicListQuery>,
) -> (StatusCode, Json<CommunityPacksResponse>) {
    let order = Order::from_public(params.sort.as_deref());
    let device_id = params.device_id.as_deref().filter(|id| valid_device_id(id));
    list_response(load_packs(&state.db, Some("approved"), order, PUBLIC_LIMIT, device_id).await)
}

/// Public: the review status of the packs this install published.
pub async fn my_packs_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DeviceQuery>,
) -> (StatusCode, Json<MyCommunityPacksResponse>) {
    if !valid_device_id(&params.device_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(MyCommunityPacksResponse {
                success: false,
                packs: vec![],
                message: Some("Invalid device id".to_string()),
            }),
        );
    }

    let rows = sqlx::query_as::<_, MyCommunityPack>(
        "SELECT id, name, status
         FROM community_packs
         WHERE device_id = $1
         ORDER BY created_at DESC
         LIMIT 100",
    )
    .bind(&params.device_id)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(packs) => (
            StatusCode::OK,
            Json(MyCommunityPacksResponse {
                success: true,
                packs,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to load community packs for a device: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MyCommunityPacksResponse {
                    success: false,
                    packs: vec![],
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

fn add_error(status: StatusCode, message: &str) -> (StatusCode, Json<AddResponse>) {
    (
        status,
        Json(AddResponse {
            success: false,
            add_count: None,
            message: Some(message.to_string()),
        }),
    )
}

/// Public: an install added an approved pack. Counted once per device.
pub async fn record_add_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i32>,
    Json(input): Json<AddInput>,
) -> (StatusCode, Json<AddResponse>) {
    if !valid_device_id(&input.device_id) {
        return add_error(StatusCode::BAD_REQUEST, "Invalid device id");
    }
    if over_rate_limit(&state, "community_adds", &headers, ADD_RATE_LIMIT).await {
        return add_error(StatusCode::TOO_MANY_REQUESTS, "Too many requests, try again later");
    }

    let result: Result<Option<i32>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let approved: Option<bool> =
            sqlx::query_scalar("SELECT status = 'approved' FROM community_packs WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if approved != Some(true) {
            return Ok(None);
        }
        let new_add = sqlx::query(
            "INSERT INTO community_pack_adds (pack_id, device_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(&input.device_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        let add_count: i32 = if new_add {
            sqlx::query_scalar(
                "UPDATE community_packs SET add_count = add_count + 1 WHERE id = $1 RETURNING add_count",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?
        } else {
            sqlx::query_scalar("SELECT add_count FROM community_packs WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
        };
        tx.commit().await?;
        Ok(Some(add_count))
    }
    .await;

    match result {
        Ok(Some(add_count)) => (
            StatusCode::OK,
            Json(AddResponse {
                success: true,
                add_count: Some(add_count),
                message: None,
            }),
        ),
        Ok(None) => add_error(StatusCode::NOT_FOUND, "Pack not found"),
        Err(e) => {
            tracing::error!("Failed to record an add for community pack {}: {:?}", id, e);
            add_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

fn like_error(status: StatusCode, message: &str) -> (StatusCode, Json<LikeResponse>) {
    (
        status,
        Json(LikeResponse {
            success: false,
            like_count: None,
            liked: None,
            message: Some(message.to_string()),
        }),
    )
}

/// Public: an install likes an approved pack, or takes its like back. One like per install.
pub async fn set_like_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i32>,
    Json(input): Json<LikeInput>,
) -> (StatusCode, Json<LikeResponse>) {
    if !valid_device_id(&input.device_id) {
        return like_error(StatusCode::BAD_REQUEST, "Invalid device id");
    }
    if over_rate_limit(&state, "community_likes", &headers, LIKE_RATE_LIMIT).await {
        return like_error(StatusCode::TOO_MANY_REQUESTS, "Too many requests, try again later");
    }

    let result: Result<Option<i32>, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let approved: Option<bool> =
            sqlx::query_scalar("SELECT status = 'approved' FROM community_packs WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if approved != Some(true) {
            return Ok(None);
        }
        let changed = if input.liked {
            sqlx::query(
                "INSERT INTO community_pack_likes (pack_id, device_id)
                 VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
            )
        } else {
            sqlx::query("DELETE FROM community_pack_likes WHERE pack_id = $1 AND device_id = $2")
        }
        .bind(id)
        .bind(&input.device_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        // Liking twice or unliking a pack that wasn't liked leaves the count alone.
        let like_count: i32 = if changed {
            sqlx::query_scalar(
                "UPDATE community_packs SET like_count = GREATEST(like_count + $2, 0)
                 WHERE id = $1
                 RETURNING like_count",
            )
            .bind(id)
            .bind(if input.liked { 1 } else { -1 })
            .fetch_one(&mut *tx)
            .await?
        } else {
            sqlx::query_scalar("SELECT like_count FROM community_packs WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?
        };
        tx.commit().await?;
        Ok(Some(like_count))
    }
    .await;

    match result {
        Ok(Some(like_count)) => (
            StatusCode::OK,
            Json(LikeResponse {
                success: true,
                like_count: Some(like_count),
                liked: Some(input.liked),
                message: None,
            }),
        ),
        Ok(None) => like_error(StatusCode::NOT_FOUND, "Pack not found"),
        Err(e) => {
            tracing::error!("Failed to update a like on community pack {}: {:?}", id, e);
            like_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

fn list_response(
    result: Result<Vec<CommunityPack>, sqlx::Error>,
) -> (StatusCode, Json<CommunityPacksResponse>) {
    match result {
        Ok(packs) => (
            StatusCode::OK,
            Json(CommunityPacksResponse {
                success: true,
                packs,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to load community packs: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(CommunityPacksResponse {
                    success: false,
                    packs: vec![],
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

/// Admin: packs with a review status (pending by default, or every pack), newest first.
pub async fn admin_list_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<StatusQuery>,
) -> (StatusCode, Json<CommunityPacksResponse>) {
    let Ok(status) = admin_status_filter(params.status.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(CommunityPacksResponse {
                success: false,
                packs: vec![],
                message: Some("Invalid status".to_string()),
            }),
        );
    };
    list_response(load_packs(&state.db, status, Order::Submitted, 500, None).await)
}

fn simple(status: StatusCode, success: bool, message: Option<&str>) -> (StatusCode, Json<SimpleResponse>) {
    (
        status,
        Json(SimpleResponse {
            success,
            message: message.map(str::to_string),
        }),
    )
}

/// Admin: approve, reject or reopen a pack.
pub async fn admin_update_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(input): Json<StatusInput>,
) -> (StatusCode, Json<SimpleResponse>) {
    if !valid_status(&input.status) {
        return simple(StatusCode::BAD_REQUEST, false, Some("Invalid status"));
    }
    let result = sqlx::query(
        "UPDATE community_packs
         SET status = $1,
             reviewed_at = CASE WHEN $1 = 'pending' THEN NULL ELSE CURRENT_TIMESTAMP END
         WHERE id = $2",
    )
    .bind(&input.status)
    .bind(id)
    .execute(&state.db)
    .await;

    match result {
        Ok(res) if res.rows_affected() == 0 => simple(StatusCode::NOT_FOUND, false, Some("Pack not found")),
        Ok(_) => simple(StatusCode::OK, true, None),
        Err(e) => {
            tracing::error!("Failed to update community pack {}: {:?}", id, e);
            simple(StatusCode::INTERNAL_SERVER_ERROR, false, Some("Database error"))
        }
    }
}

pub async fn admin_delete_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> (StatusCode, Json<SimpleResponse>) {
    match sqlx::query("DELETE FROM community_packs WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await
    {
        Ok(res) if res.rows_affected() == 0 => simple(StatusCode::NOT_FOUND, false, Some("Pack not found")),
        Ok(_) => simple(StatusCode::OK, true, None),
        Err(e) => {
            tracing::error!("Failed to delete community pack {}: {:?}", id, e);
            simple(StatusCode::INTERNAL_SERVER_ERROR, false, Some("Database error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "3f2b8c1e-5d4a-4e7b-9c0d-1a2b3c4d5e6f";

    fn emote(n: usize, animated: bool) -> PublishEmoteInput {
        PublishEmoteInput {
            emote_id: format!("01GTKRP9R00000ZMA1SVXGK9{n:02}"),
            emote_name: format!("emote{n}"),
            animated,
        }
    }

    fn input(count: usize) -> PublishInput {
        PublishInput {
            device_id: DEVICE.to_string(),
            name: "  Clásicos  ".to_string(),
            author_name: Some(" xael ".to_string()),
            animated: true,
            emotes: (0..count).map(|n| emote(n, true)).collect(),
        }
    }

    #[test]
    fn valid_packs_are_trimmed() {
        let (name, author) = validate_publish(&input(3)).unwrap();
        assert_eq!(name, "Clásicos");
        assert_eq!(author.as_deref(), Some("xael"));
    }

    #[test]
    fn blank_author_is_dropped() {
        let mut pack = input(3);
        pack.author_name = Some("   ".to_string());
        assert_eq!(validate_publish(&pack).unwrap().1, None);
    }

    #[test]
    fn packs_need_three_to_thirty_stickers() {
        assert!(validate_publish(&input(2)).is_err());
        assert!(validate_publish(&input(30)).is_ok());
        assert!(validate_publish(&input(31)).is_err());
    }

    #[test]
    fn stickers_must_match_the_pack_kind() {
        let mut pack = input(3);
        pack.emotes[1].animated = false;
        assert!(validate_publish(&pack).is_err());
    }

    #[test]
    fn duplicates_and_bad_ids_are_rejected() {
        let mut duplicated = input(3);
        duplicated.emotes[2].emote_id = duplicated.emotes[0].emote_id.clone();
        assert!(validate_publish(&duplicated).is_err());

        let mut url_as_id = input(3);
        url_as_id.emotes[0].emote_id = "https://evil.example/x.webp".to_string();
        assert!(validate_publish(&url_as_id).is_err());
    }

    #[test]
    fn names_are_bounded_and_single_line() {
        let mut long = input(3);
        long.name = "x".repeat(41);
        assert!(validate_publish(&long).is_err());

        let mut multiline = input(3);
        multiline.name = "a\nb".to_string();
        assert!(validate_publish(&multiline).is_err());

        let mut long_author = input(3);
        long_author.author_name = Some("y".repeat(25));
        assert!(validate_publish(&long_author).is_err());
    }

    #[test]
    fn device_id_is_required() {
        let mut pack = input(3);
        pack.device_id = "short".to_string();
        assert!(validate_publish(&pack).is_err());
    }

    #[test]
    fn admin_filter_lists_every_pack_with_all() {
        assert_eq!(admin_status_filter(None), Ok(Some("pending")));
        assert_eq!(admin_status_filter(Some("approved")), Ok(Some("approved")));
        assert_eq!(admin_status_filter(Some("all")), Ok(None));
        assert_eq!(admin_status_filter(Some("deleted")), Err(()));
    }

    #[test]
    fn popular_ranks_by_likes_before_adds() {
        assert!(Order::Popular.sql().starts_with("like_count DESC, add_count DESC"));
    }

    #[test]
    fn public_sort_defaults_to_popular() {
        assert_eq!(Order::from_public(None), Order::Popular);
        assert_eq!(Order::from_public(Some("popular")), Order::Popular);
        assert_eq!(Order::from_public(Some("new")), Order::New);
        // Anything else never reaches the SQL: it falls back to the default order.
        assert_eq!(Order::from_public(Some("id; DROP TABLE x")), Order::Popular);
    }

    #[test]
    fn emotes_point_at_the_7tv_cdn() {
        let animated = emote_response("abc".to_string(), "KEK".to_string(), true);
        assert_eq!(animated.url, "https://cdn.7tv.app/emote/abc/4x.webp");
        assert_eq!(animated.animated_preview_url.as_deref(), Some("https://cdn.7tv.app/emote/abc/2x.webp"));
        assert_eq!(animated.poster_url.as_deref(), Some("https://cdn.7tv.app/emote/abc/4x_static.webp"));

        let still = emote_response("abc".to_string(), "KEK".to_string(), false);
        assert_eq!(still.animated_preview_url, None);
        assert_eq!(still.poster_url.as_deref(), Some("https://cdn.7tv.app/emote/abc/4x.webp"));
    }
}
