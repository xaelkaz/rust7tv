//! Starter packs: curated sets of stickers the app offers with a one-tap
//! "Add to WhatsApp", so new users don't have to collect three stickers first.
//!
//! The admin builds them in the dashboard from search results; each emote is
//! stored as a snapshot. Only published packs are served to the app, in the
//! order set in the dashboard.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::models::EmoteResponse;
use crate::AppState;

/// WhatsApp rejects packs with fewer stickers than this.
const MIN_PUBLISHED_STICKERS: usize = 3;
/// WhatsApp's own maximum for a sticker pack.
const MAX_STICKERS: usize = 30;
const MAX_NAME_CHARS: usize = 40;
const MAX_EMOJI_CHARS: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterPackInput {
    pub name: String,
    pub emoji: Option<String>,
    pub animated: bool,
    pub published: bool,
    pub emotes: Vec<StarterPackEmoteInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterPackEmoteInput {
    pub emote_id: String,
    pub emote_name: String,
    pub file_name: String,
    pub url: String,
    pub animated: bool,
    pub animated_preview_url: Option<String>,
    pub poster_url: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterPack {
    pub id: i32,
    pub name: String,
    pub emoji: Option<String>,
    pub animated: bool,
    pub published: bool,
    pub position: i32,
    pub emotes: Vec<EmoteResponse>,
}

#[derive(Debug, Serialize)]
pub struct StarterPacksResponse {
    pub success: bool,
    pub packs: Vec<StarterPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StarterPackResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<StarterPack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderInput {
    pub ids: Vec<i32>,
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
    emoji: Option<String>,
    animated: bool,
    published: bool,
    position: i32,
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    pack_id: i32,
    emote_id: String,
    emote_name: String,
    file_name: String,
    url: String,
    animated: bool,
    animated_preview_url: Option<String>,
    poster_url: Option<String>,
    tags: Vec<String>,
}

/// Checks a pack before saving. Drafts may be empty; published packs must satisfy WhatsApp.
pub fn validate_pack(input: &StarterPackInput) -> Result<(), String> {
    let name_chars = input.name.trim().chars().count();
    if name_chars == 0 || name_chars > MAX_NAME_CHARS {
        return Err(format!("Name must be 1-{MAX_NAME_CHARS} characters"));
    }
    if let Some(emoji) = &input.emoji {
        if emoji.trim().chars().count() > MAX_EMOJI_CHARS {
            return Err("Emoji is too long".to_string());
        }
    }
    if input.emotes.len() > MAX_STICKERS {
        return Err(format!("A pack can have at most {MAX_STICKERS} stickers"));
    }
    if input.published && input.emotes.len() < MIN_PUBLISHED_STICKERS {
        return Err(format!(
            "A published pack needs at least {MIN_PUBLISHED_STICKERS} stickers"
        ));
    }

    let mut seen = HashSet::new();
    for emote in &input.emotes {
        if emote.animated != input.animated {
            return Err(format!(
                "\"{}\" is {}; this pack only takes {} stickers",
                emote.emote_name,
                kind(emote.animated),
                kind(input.animated),
            ));
        }
        if emote.emote_id.trim().is_empty() || emote.emote_name.trim().is_empty() {
            return Err("Every sticker needs an id and a name".to_string());
        }
        if !emote.url.starts_with("https://") {
            return Err(format!("\"{}\" has an invalid image URL", emote.emote_name));
        }
        if !seen.insert(emote.emote_id.as_str()) {
            return Err(format!("\"{}\" is in the pack twice", emote.emote_name));
        }
    }
    Ok(())
}

fn kind(animated: bool) -> &'static str {
    if animated {
        "animated"
    } else {
        "static"
    }
}

fn normalized_emoji(emoji: &Option<String>) -> Option<String> {
    emoji
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

async fn load_packs(
    db: &sqlx::Pool<sqlx::Postgres>,
    published_only: bool,
) -> Result<Vec<StarterPack>, sqlx::Error> {
    let packs = sqlx::query_as::<_, PackRow>(
        "SELECT id, name, emoji, animated, published, position
         FROM starter_packs
         WHERE published OR NOT $1
         ORDER BY position, id",
    )
    .bind(published_only)
    .fetch_all(db)
    .await?;

    let ids: Vec<i32> = packs.iter().map(|p| p.id).collect();
    let items = sqlx::query_as::<_, ItemRow>(
        "SELECT pack_id, emote_id, emote_name, file_name, url, animated,
                animated_preview_url, poster_url, tags
         FROM starter_pack_items
         WHERE pack_id = ANY($1)
         ORDER BY pack_id, position",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;

    let mut by_pack: HashMap<i32, Vec<EmoteResponse>> = HashMap::new();
    for item in items {
        by_pack.entry(item.pack_id).or_default().push(EmoteResponse {
            file_name: item.file_name,
            url: item.url,
            animated_preview_url: item.animated_preview_url,
            poster_url: item.poster_url,
            emote_id: item.emote_id,
            emote_name: item.emote_name,
            owner: None,
            animated: Some(item.animated),
            scale: None,
            mime: None,
            tags: Some(item.tags),
        });
    }

    Ok(packs
        .into_iter()
        .map(|p| StarterPack {
            emotes: by_pack.remove(&p.id).unwrap_or_default(),
            id: p.id,
            name: p.name,
            emoji: p.emoji,
            animated: p.animated,
            published: p.published,
            position: p.position,
        })
        .collect())
}

async fn replace_items(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    pack_id: i32,
    emotes: &[StarterPackEmoteInput],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM starter_pack_items WHERE pack_id = $1")
        .bind(pack_id)
        .execute(&mut **tx)
        .await?;
    for (position, emote) in emotes.iter().enumerate() {
        sqlx::query(
            "INSERT INTO starter_pack_items
                 (pack_id, position, emote_id, emote_name, file_name, url, animated,
                  animated_preview_url, poster_url, tags)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(pack_id)
        .bind(position as i32)
        .bind(&emote.emote_id)
        .bind(&emote.emote_name)
        .bind(&emote.file_name)
        .bind(&emote.url)
        .bind(emote.animated)
        .bind(&emote.animated_preview_url)
        .bind(&emote.poster_url)
        .bind(emote.tags.clone().unwrap_or_default())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn load_pack(db: &sqlx::Pool<sqlx::Postgres>, id: i32) -> Result<Option<StarterPack>, sqlx::Error> {
    Ok(load_packs(db, false).await?.into_iter().find(|p| p.id == id))
}

fn list_error(e: sqlx::Error) -> (StatusCode, Json<StarterPacksResponse>) {
    tracing::error!("Failed to load starter packs: {:?}", e);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(StarterPacksResponse {
            success: false,
            packs: vec![],
            message: Some("Database error".to_string()),
        }),
    )
}

fn pack_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<StarterPackResponse>) {
    (
        status,
        Json(StarterPackResponse {
            success: false,
            pack: None,
            message: Some(message.into()),
        }),
    )
}

/// Public: published packs, in dashboard order.
pub async fn list_published_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<StarterPacksResponse>) {
    match load_packs(&state.db, true).await {
        Ok(packs) => (
            StatusCode::OK,
            Json(StarterPacksResponse {
                success: true,
                packs,
                message: None,
            }),
        ),
        Err(e) => list_error(e),
    }
}

/// Admin: every pack, drafts included.
pub async fn admin_list_handler(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<StarterPacksResponse>) {
    match load_packs(&state.db, false).await {
        Ok(packs) => (
            StatusCode::OK,
            Json(StarterPacksResponse {
                success: true,
                packs,
                message: None,
            }),
        ),
        Err(e) => list_error(e),
    }
}

pub async fn create_handler(
    State(state): State<Arc<AppState>>,
    Json(input): Json<StarterPackInput>,
) -> (StatusCode, Json<StarterPackResponse>) {
    if let Err(message) = validate_pack(&input) {
        return pack_error(StatusCode::BAD_REQUEST, message);
    }
    let result: Result<i32, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let id: i32 = sqlx::query_scalar(
            "INSERT INTO starter_packs (name, emoji, animated, published, position)
             VALUES ($1, $2, $3, $4, (SELECT COALESCE(MAX(position) + 1, 0) FROM starter_packs))
             RETURNING id",
        )
        .bind(input.name.trim())
        .bind(normalized_emoji(&input.emoji))
        .bind(input.animated)
        .bind(input.published)
        .fetch_one(&mut *tx)
        .await?;
        replace_items(&mut tx, id, &input.emotes).await?;
        tx.commit().await?;
        Ok(id)
    }
    .await;

    match result {
        Ok(id) => match load_pack(&state.db, id).await {
            Ok(pack) => (
                StatusCode::OK,
                Json(StarterPackResponse {
                    success: true,
                    pack,
                    message: None,
                }),
            ),
            Err(e) => {
                tracing::error!("Failed to reload starter pack {}: {:?}", id, e);
                pack_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            }
        },
        Err(e) => {
            tracing::error!("Failed to create starter pack: {:?}", e);
            pack_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

pub async fn update_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Json(input): Json<StarterPackInput>,
) -> (StatusCode, Json<StarterPackResponse>) {
    if let Err(message) = validate_pack(&input) {
        return pack_error(StatusCode::BAD_REQUEST, message);
    }
    let result: Result<bool, sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let updated = sqlx::query(
            "UPDATE starter_packs
             SET name = $1, emoji = $2, animated = $3, published = $4,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = $5",
        )
        .bind(input.name.trim())
        .bind(normalized_emoji(&input.emoji))
        .bind(input.animated)
        .bind(input.published)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if updated {
            replace_items(&mut tx, id, &input.emotes).await?;
        }
        tx.commit().await?;
        Ok(updated)
    }
    .await;

    match result {
        Ok(false) => pack_error(StatusCode::NOT_FOUND, "Pack not found"),
        Ok(true) => match load_pack(&state.db, id).await {
            Ok(pack) => (
                StatusCode::OK,
                Json(StarterPackResponse {
                    success: true,
                    pack,
                    message: None,
                }),
            ),
            Err(e) => {
                tracing::error!("Failed to reload starter pack {}: {:?}", id, e);
                pack_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
            }
        },
        Err(e) => {
            tracing::error!("Failed to update starter pack {}: {:?}", id, e);
            pack_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error")
        }
    }
}

pub async fn delete_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> (StatusCode, Json<SimpleResponse>) {
    match sqlx::query("DELETE FROM starter_packs WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await
    {
        Ok(res) if res.rows_affected() == 0 => (
            StatusCode::NOT_FOUND,
            Json(SimpleResponse {
                success: false,
                message: Some("Pack not found".to_string()),
            }),
        ),
        Ok(_) => (
            StatusCode::OK,
            Json(SimpleResponse {
                success: true,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to delete starter pack {}: {:?}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SimpleResponse {
                    success: false,
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

/// Sets the display order: the first id shows first in the app.
pub async fn reorder_handler(
    State(state): State<Arc<AppState>>,
    Json(input): Json<ReorderInput>,
) -> (StatusCode, Json<SimpleResponse>) {
    let result: Result<(), sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        for (position, id) in input.ids.iter().enumerate() {
            sqlx::query("UPDATE starter_packs SET position = $1 WHERE id = $2")
                .bind(position as i32)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    .await;

    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(SimpleResponse {
                success: true,
                message: None,
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to reorder starter packs: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SimpleResponse {
                    success: false,
                    message: Some("Database error".to_string()),
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emote(id: &str, animated: bool) -> StarterPackEmoteInput {
        StarterPackEmoteInput {
            emote_id: id.to_string(),
            emote_name: format!("name-{id}"),
            file_name: format!("{id}.webp"),
            url: format!("https://cdn.example/{id}.webp"),
            animated,
            animated_preview_url: None,
            poster_url: None,
            tags: None,
        }
    }

    fn pack(published: bool, emotes: Vec<StarterPackEmoteInput>) -> StarterPackInput {
        StarterPackInput {
            name: "Risa".to_string(),
            emoji: Some("😂".to_string()),
            animated: true,
            published,
            emotes,
        }
    }

    #[test]
    fn drafts_can_be_empty_but_published_packs_need_three() {
        assert!(validate_pack(&pack(false, vec![])).is_ok());
        assert!(validate_pack(&pack(true, vec![emote("a", true), emote("b", true)])).is_err());
        assert!(validate_pack(&pack(true, vec![emote("a", true), emote("b", true), emote("c", true)])).is_ok());
    }

    #[test]
    fn packs_are_capped_at_thirty() {
        let emotes = (0..31).map(|i| emote(&i.to_string(), true)).collect();
        assert!(validate_pack(&pack(false, emotes)).is_err());
    }

    #[test]
    fn stickers_must_match_the_pack_kind() {
        let err = validate_pack(&pack(false, vec![emote("a", false)])).unwrap_err();
        assert!(err.contains("static"));
    }

    #[test]
    fn duplicates_and_bad_urls_are_rejected() {
        assert!(validate_pack(&pack(false, vec![emote("a", true), emote("a", true)])).is_err());
        let mut insecure = emote("b", true);
        insecure.url = "http://cdn.example/b.webp".to_string();
        assert!(validate_pack(&pack(false, vec![insecure])).is_err());
    }

    #[test]
    fn names_are_required_and_bounded() {
        let mut blank = pack(false, vec![]);
        blank.name = "   ".to_string();
        assert!(validate_pack(&blank).is_err());
        let mut long = pack(false, vec![]);
        long.name = "x".repeat(41);
        assert!(validate_pack(&long).is_err());
    }
}
