use axum::{
    routing::{get, post, put, delete, patch},
    Router,
    Json,
    middleware,
    extract::{DefaultBodyLimit, State, Query, Path},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use tower_http::cors::{Any, CorsLayer};
use crate::AppState;
use crate::models::{TrendingPeriod, SearchResponse, SyncTrendingRequest, SyncUserEmotesRequest, UpdateUserImageRequest, EmoteResponse, TrendingTagResponse, TrendingTag};
use serde::{Deserialize, Serialize};

mod auth;
mod community_packs;
mod creator_requests;
mod dashboard;
mod starter_packs;
mod sticker_files;

pub fn create_router(state: Arc<AppState>) -> Router {
    let public = Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/api/search-emotes", post(search_emotes_handler))
        .route("/api/trending/tags", get(trending_tags_handler))
        .route("/api/trending/emotes", get(trending_emotes_handler))
        .route("/api/trending/synced", get(synced_trending_emotes_handler))
        .route("/api/user/emotes/saved", get(get_saved_user_emotes_handler))
        .route("/api/users", get(list_users_handler))
        .route("/api/admin/users", get(list_users_handler))
        .route(
            "/api/creator-requests",
            post(creator_requests::create_creator_request_handler),
        )
        .route(
            "/api/creator-requests/mine",
            get(creator_requests::my_creator_requests_handler),
        )
        .route("/api/starter-packs", get(starter_packs::list_published_handler))
        .route(
            "/api/community-packs",
            get(community_packs::list_approved_handler).post(community_packs::publish_handler),
        )
        .route(
            "/api/community-packs/mine",
            get(community_packs::my_packs_handler),
        )
        .route(
            "/api/community-packs/:id/adds",
            post(community_packs::record_add_handler),
        )
        .route(
            "/api/community-packs/:id/like",
            post(community_packs::set_like_handler),
        )
        .route(
            "/api/community-packs/:id/stickers/:emote_id",
            put(community_packs::upload_sticker_handler)
                .layer(DefaultBodyLimit::max(community_packs::MAX_UPLOAD_BYTES)),
        );
    // Development only: serves files the local storage stand-in wrote (`LOCAL_BLOB_DIR`).
    let public = if state.storage.serves_local_blobs() {
        public.route("/dev-blobs/*path", get(dev_blob_handler))
    } else {
        public
    };

    let admin = Router::new()
        .route("/admin/dashboard", get(dashboard::dashboard_handler))
        .route("/api/admin/sync-trending", post(sync_trending_handler))
        .route("/api/admin/sync-user-emotes", post(sync_user_emotes_handler))
        .route("/api/admin/users/:folder_name/image", patch(update_user_image_handler))
        .route("/api/admin/users/:folder_name", delete(delete_user_handler))
        .route(
            "/api/admin/creator-requests",
            get(creator_requests::list_creator_requests_handler),
        )
        .route(
            "/api/admin/creator-requests/:id",
            patch(creator_requests::update_creator_request_handler),
        )
        .route(
            "/api/admin/starter-packs",
            get(starter_packs::admin_list_handler).post(starter_packs::create_handler),
        )
        .route(
            "/api/admin/starter-packs/order",
            put(starter_packs::reorder_handler),
        )
        .route(
            "/api/admin/starter-packs/:id",
            put(starter_packs::update_handler).delete(starter_packs::delete_handler),
        )
        .route(
            "/api/admin/community-packs",
            get(community_packs::admin_list_handler),
        )
        .route(
            "/api/admin/community-packs/:id",
            patch(community_packs::admin_update_handler).delete(community_packs::admin_delete_handler),
        )
        .route(
            "/api/admin/community-packs/:id/promote",
            post(community_packs::admin_promote_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::require_admin,
        ));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .merge(public)
        .merge(admin)
        .layer(cors)
        .with_state(state)
}

/// Creators whose sync is running. A sync keeps going after its request is dropped, so this
/// keeps a second one for the same creator from racing it.
#[derive(Default)]
pub struct RunningUserSyncs(Mutex<HashSet<String>>);

/// Marks a creator's sync as running until it's dropped.
pub struct UserSyncGuard {
    syncs: Arc<RunningUserSyncs>,
    folder: String,
}

impl RunningUserSyncs {
    /// None while another sync of `folder` is running.
    pub fn begin(syncs: &Arc<Self>, folder: &str) -> Option<UserSyncGuard> {
        let mut running = syncs.0.lock().unwrap_or_else(PoisonError::into_inner);
        running.insert(folder.to_string()).then(|| UserSyncGuard {
            syncs: Arc::clone(syncs),
            folder: folder.to_string(),
        })
    }
}

impl Drop for UserSyncGuard {
    fn drop(&mut self) {
        self.syncs.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.folder);
    }
}

/// Folder names are used as Azure blob prefixes and as SQL bind params.
/// Restrict to a safe ASCII subset so an empty/malicious value can't wipe the
/// whole container (e.g. `folder_name=""` → `prefix="/"` would list all blobs).
fn valid_folder_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn normalize_image_url(image_url: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(image_url) = image_url else {
        return Ok(None);
    };

    let image_url = image_url.trim();
    if image_url.is_empty() {
        return Ok(None);
    }
    if image_url.len() > 2048 {
        return Err("Invalid imageUrl: must be 2048 characters or less");
    }
    if image_url.chars().any(char::is_whitespace) {
        return Err("Invalid imageUrl: must not contain spaces");
    }

    let lower = image_url.to_ascii_lowercase();
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err("Invalid imageUrl: must start with http:// or https://");
    }

    Ok(Some(image_url.to_string()))
}

/// Atomic replace: `DELETE` existing stickers for the folder and `INSERT` the
/// new batch. All-or-nothing — a mid-loop failure rolls back via the tx Drop.
async fn persist_trending_stickers(
    pool: &sqlx::PgPool,
    db_folder: &str,
    emotes: &[EmoteResponse],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM stickers WHERE folder_name = $1")
        .bind(db_folder)
        .execute(&mut *tx)
        .await?;

    if !emotes.is_empty() {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO stickers (seven_tv_id, emote_name, file_name, url, owner_name, tags, animated, folder_name) ",
        );
        qb.push_values(emotes, |mut b, emote| {
            b.push_bind(&emote.emote_id)
                .push_bind(&emote.emote_name)
                .push_bind(&emote.file_name)
                .push_bind(&emote.url)
                .push_bind(&emote.owner)
                .push_bind(&emote.tags)
                .push_bind(emote.animated.unwrap_or(false))
                .push_bind(db_folder);
        });
        qb.build().execute(&mut *tx).await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Mirror the user's set atomically: upsert their stickers, delete the ones whose emote isn't in
/// `set_ids` any more (it left the set, or is skipped now, like zero-width ones), then upsert the
/// user record with the resulting count. Returns how many stickers were deleted.
///
/// An emote still in the set whose image failed to download keeps its previous sticker.
async fn persist_user_emotes(
    pool: &sqlx::PgPool,
    user_id: &str,
    folder: &str,
    display_name: &str,
    image_url: Option<&str>,
    emotes: &[EmoteResponse],
    set_ids: &[String],
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;

    if !emotes.is_empty() {
        // Where each emote sits in the set, so they're served in the channel's order.
        let positions: HashMap<&str, i32> = set_ids
            .iter()
            .enumerate()
            .map(|(position, id)| (id.as_str(), position as i32))
            .collect();
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO stickers (seven_tv_id, emote_name, file_name, url, owner_name, tags, animated, folder_name, position) ",
        );
        qb.push_values(emotes, |mut b, emote| {
            b.push_bind(&emote.emote_id)
                .push_bind(&emote.emote_name)
                .push_bind(&emote.file_name)
                .push_bind(&emote.url)
                .push_bind(&emote.owner)
                .push_bind(&emote.tags)
                .push_bind(emote.animated.unwrap_or(false))
                .push_bind(folder)
                .push_bind(positions.get(emote.emote_id.as_str()).copied());
        });
        qb.push(
            " ON CONFLICT (seven_tv_id, folder_name) DO UPDATE SET \
             emote_name = EXCLUDED.emote_name, \
             file_name = EXCLUDED.file_name, \
             url = EXCLUDED.url, \
             owner_name = EXCLUDED.owner_name, \
             tags = EXCLUDED.tags, \
             animated = EXCLUDED.animated, \
             position = EXCLUDED.position",
        );
        qb.build().execute(&mut *tx).await?;
    }

    // Only the rows go: their blobs stay, so stickers people already saved keep working.
    let removed = sqlx::query(
        "DELETE FROM stickers WHERE folder_name = $1 AND NOT (seven_tv_id = ANY($2))",
    )
    .bind(folder)
    .bind(set_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    sqlx::query(
        r#"
        INSERT INTO users (seven_tv_id, folder_name, display_name, image_url, last_synced_at, emote_count)
        VALUES ($1, $2, $3, $4, NOW(), (SELECT COUNT(*)::int FROM stickers WHERE folder_name = $2))
        ON CONFLICT (folder_name)
        DO UPDATE SET
            seven_tv_id = EXCLUDED.seven_tv_id,
            display_name = EXCLUDED.display_name,
            image_url = COALESCE(EXCLUDED.image_url, users.image_url),
            last_synced_at = NOW(),
            emote_count = EXCLUDED.emote_count
        "#,
    )
    .bind(user_id)
    .bind(folder)
    .bind(display_name)
    .bind(image_url)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(removed)
}

/// What a user sync did, for the dashboard log.
fn user_sync_summary(set_name: &str, stored: usize, zero_width: usize, failed: usize, removed: u64) -> String {
    let mut summary = format!("Synced {} emotes from the 7TV set \"{}\"", stored, set_name);
    if zero_width > 0 {
        summary.push_str(&format!(", skipped {} zero-width", zero_width));
    }
    if failed > 0 {
        summary.push_str(&format!(", {} failed to download or store (see the server logs)", failed));
    }
    if removed > 0 {
        let noun = if removed == 1 { "sticker" } else { "stickers" };
        summary.push_str(&format!(", removed {} {} no longer in the set", removed, noun));
    }
    summary
}

fn user_sync_failed(status: StatusCode, message: String) -> (StatusCode, Json<SearchResponse>) {
    (
        status,
        Json(SearchResponse {
            success: false,
            total_found: 0,
            emotes: vec![],
            message: Some(message),
            cached: Some(false),
            processing_time: None,
            page: None,
            total_pages: None,
            results_per_page: None,
            has_next_page: None,
        }),
    )
}

async fn root_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "title": state.config.api_title,
        "description": state.config.api_description,
        "version": state.config.api_version
    }))
}

async fn health_handler() -> &'static str {
    "OK"
}

async fn search_emotes_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<crate::models::SearchRequest>,
) -> Json<SearchResponse> {
    let limit = payload.limit.unwrap_or(20);
    let page = payload.page.unwrap_or(1);
    let animated_only = payload.animated_only.unwrap_or(false);
    
    // Check cache
    let cache_key = crate::services::cache::CacheService::get_cache_key(
        &payload.query,
        page,
        limit,
        animated_only,
    );
    if let Some(cached_data) = state.cache.get_from_cache(&cache_key).await {
        if let Ok(mut response) = serde_json::from_slice::<SearchResponse>(&cached_data) {
            response.cached = Some(true);
            return Json(response);
        }
    }

    // Fetch from 7TV
    let result = state.seventv.search_emotes(&payload.query, page, limit, animated_only).await;
    match result {
        Ok(emotes) => {
            let processed = state.seventv.process_emotes_batch_no_storage(emotes);
            let response = SearchResponse {
                success: true,
                total_found: processed.len() as i32,
                emotes: processed,
                message: None,
                cached: Some(false),
                processing_time: None,
                page: Some(page),
                total_pages: Some(1), // TODO: fetch from 7TV if needed
                results_per_page: Some(limit),
                has_next_page: Some(false),
            };
            
            // Save to cache
            let _ = state.cache.save_to_cache(&cache_key, &response, state.config.cache_ttl).await;
            
            Json(response)
        },
        Err(e) => {
            Json(SearchResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                message: Some(e.to_string()),
                cached: Some(false),
                processing_time: None,
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            })
        }
    }
}

#[derive(Deserialize)]
struct TrendingQuery {
    period: Option<String>,
    limit: Option<i32>,
    animated_only: Option<bool>,
    emote_type: Option<String>,
}

async fn trending_emotes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TrendingQuery>,
) -> Json<SearchResponse> {
    let limit = params.limit.unwrap_or(20);
    let animated_only = params.animated_only.unwrap_or(false) || params.emote_type.as_deref() == Some("animated");
    let period_str = params.period.unwrap_or_else(|| "trending_weekly".to_string());
    
    let period = match period_str.as_str() {
        "trending_daily" => TrendingPeriod::Daily,
        "trending_monthly" => TrendingPeriod::Monthly,
        "popularity" => TrendingPeriod::AllTime,
        _ => TrendingPeriod::Weekly,
    };

    // Construct cache key
    let cache_key = crate::services::cache::CacheService::get_trending_cache_key(
        &period_str, limit, 1, animated_only
    );

    if let Some(cached_data) = state.cache.get_from_cache(&cache_key).await {
        if let Ok(mut response) = serde_json::from_slice::<SearchResponse>(&cached_data) {
            response.cached = Some(true);
            return Json(response);
        }
    }

    match state.seventv.fetch_trending_emotes(&period, limit, animated_only).await {
        Ok(emotes) => {
            let processed = state.seventv.process_emotes_batch(emotes, "trending-emotes").await;
            let response = SearchResponse {
                success: true,
                total_found: processed.len() as i32,
                emotes: processed,
                message: None,
                cached: Some(false),
                processing_time: None,
                page: Some(1),
                total_pages: Some(1),
                results_per_page: Some(limit),
                has_next_page: Some(false),
            };

            let _ = state.cache.save_to_cache(&cache_key, &response, state.config.trending_cache_ttl).await;
            Json(response)
        },
        Err(e) => {
            tracing::error!("Failed to fetch trending emotes: {:?}", e);
            Json(SearchResponse {
            success: false,
            total_found: 0,
            emotes: vec![],
            message: Some(e.to_string()),
            cached: Some(false),
            processing_time: None,
            page: None,
            total_pages: None,
            results_per_page: None,
            has_next_page: None,
        })
    }
}
}

async fn sync_trending_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SyncTrendingRequest>,
) -> (StatusCode, Json<SearchResponse>) {
    let animated_only = payload.animated_only.unwrap_or(false);
    let period_str = payload.period.unwrap_or_else(|| "trending_weekly".to_string());
    
    let period = match period_str.as_str() {
        "trending_daily" => TrendingPeriod::Daily,
        "trending_monthly" => TrendingPeriod::Monthly,
        "popularity" => TrendingPeriod::AllTime,
        _ => TrendingPeriod::Weekly,
    };

    // Use limit from payload if provided, otherwise default to 100
    let limit = payload.limit.unwrap_or(100);

    // Define dynamic folder path: trending/{period}/{type}/
    let type_str = if animated_only { "animated" } else { "static" };
    let folder = format!("trending/{}/{}", period_str, type_str);

    // Removed: We no longer cleanup existing blobs in that folder to prevent breaking user favorites. 
    // The database DELETE handles logical trending cleanup.

    match state.seventv.fetch_trending_emotes(&period, limit, animated_only).await {
        Ok(emotes) => {
            let processed = state.seventv.process_emotes_batch(emotes, &folder).await;
            
            // Save to Redis with a special sync key and long TTL (e.g. 24 hours)
            let sync_key = crate::services::cache::CacheService::get_trending_sync_key(&period_str, animated_only);
            // 24 hours = 86400 seconds
            let ttl = 86400; 
            
            if let Err(e) = state.cache.save_to_cache(&sync_key, &processed, ttl).await {
                tracing::error!("Failed to save synced trending emotes to cache: {:?}", e);
            }

            // Save metadata manifest to Azure
            let metadata_blob_name = format!("{}/_metadata.json", folder);
            if let Ok(json_data) = serde_json::to_vec(&processed) {
                if let Err(e) = state.storage.upload_blob(json_data, &metadata_blob_name, "application/json").await {
                    tracing::error!("Failed to save metadata to Azure: {:?}", e);
                }
            }

            // Save trending stickers to database with a special folder name
            let db_folder = format!("trending_sync:{}:{}", period_str, animated_only);

            if let Err(e) = persist_trending_stickers(&state.db, &db_folder, &processed).await {
                tracing::error!("Failed to persist trending stickers (rolled back): {:?}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(SearchResponse {
                        success: false,
                        total_found: 0,
                        emotes: vec![],
                        message: Some(format!("Database error: {}", e)),
                        cached: Some(false),
                        processing_time: None,
                        page: None,
                        total_pages: None,
                        results_per_page: None,
                        has_next_page: None,
                    }),
                );
            }

            (
                StatusCode::OK,
                Json(SearchResponse {
                    success: true,
                    total_found: processed.len() as i32,
                    emotes: processed,
                    message: Some("Synced successfully".to_string()),
                    cached: Some(false),
                    processing_time: None,
                    page: Some(1),
                    total_pages: Some(1),
                    results_per_page: Some(limit),
                    has_next_page: Some(false),
                }),
            )
        },
        Err(e) => {
            tracing::error!("Failed to sync trending emotes: {:?}", e);
            (
                StatusCode::BAD_GATEWAY,
                Json(SearchResponse {
                    success: false,
                    total_found: 0,
                    emotes: vec![],
                    message: Some(e.to_string()),
                    cached: Some(false),
                    processing_time: None,
                    page: None,
                    total_pages: None,
                    results_per_page: None,
                    has_next_page: None,
                }),
            )
        }
    }
}

async fn trending_tags_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TrendingQuery>,
) -> Json<TrendingTagResponse> {
    let limit = params.limit.unwrap_or(10) as i64;
    let animated_only = params.animated_only.unwrap_or(false) || params.emote_type.as_deref() == Some("animated");
    let period_str = params.period.unwrap_or_else(|| "trending_weekly".to_string());

    let db_folder = format!("trending_sync:{}:{}", period_str, animated_only);

    // Unnest tags array, group by tag name, count occurrences, and return top N
    let query = r#"
        SELECT tag, count(*) as frequency 
        FROM (
            SELECT unnest(tags) as tag
            FROM stickers 
            WHERE folder_name = $1
        ) sub
        GROUP BY tag 
        ORDER BY frequency DESC 
        LIMIT $2
    "#;

    let rows = sqlx::query_as::<_, (String, i64)>(query)
        .bind(&db_folder)
        .bind(limit)
        .fetch_all(&state.db)
        .await;

    match rows {
        Ok(results) => {
            let tags = results.into_iter().map(|(tag, count)| TrendingTag {
                tag,
                count,
            }).collect();

            Json(TrendingTagResponse {
                success: true,
                tags,
            })
        },
        Err(e) => {
            tracing::error!("Failed to fetch trending tags from DB: {:?}", e);
            Json(TrendingTagResponse {
                success: false,
                tags: vec![],
            })
        }
    }
}

async fn synced_trending_emotes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TrendingQuery>,
) -> Json<SearchResponse> {
    let limit = params.limit.unwrap_or(20) as i64;
    let animated_only = params.animated_only.unwrap_or(false) || params.emote_type.as_deref() == Some("animated");
    let period_str = params.period.unwrap_or_else(|| "trending_weekly".to_string());

    let db_folder = format!("trending_sync:{}:{}", period_str, animated_only);

    // Query stickers from database
    let rows = sqlx::query_as::<_, StickerRow>(
        "SELECT seven_tv_id, emote_name, file_name, url, owner_name, tags, animated FROM stickers WHERE folder_name = $1 LIMIT $2"
    )
    .bind(&db_folder)
    .bind(limit)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(stickers) if !stickers.is_empty() => {
            let emotes: Vec<EmoteResponse> = stickers.into_iter().map(|s| EmoteResponse {
                emote_id: s.seven_tv_id,
                emote_name: s.emote_name,
                file_name: s.file_name,
                url: s.url,
                animated_preview_url: None,
                poster_url: None,
                owner: s.owner_name,
                tags: s.tags,
                animated: Some(s.animated),
                scale: None,
                mime: None,
            }).collect();

            Json(SearchResponse {
                success: true,
                total_found: emotes.len() as i32,
                emotes,
                message: None,
                cached: Some(false),
                processing_time: None,
                page: Some(1),
                total_pages: Some(1),
                results_per_page: Some(limit as i32),
                has_next_page: Some(false),
            })
        },
        _ => {
            // Fallback to Redis sync key logic if DB is empty
            let sync_key = crate::services::cache::CacheService::get_trending_sync_key(&period_str, animated_only);
            if let Some(cached_data) = state.cache.get_from_cache(&sync_key).await {
                if let Ok(all_emotes) = serde_json::from_slice::<Vec<EmoteResponse>>(&cached_data) {
                    return return_paginated_response(all_emotes, limit as usize);
                }
            }

            Json(SearchResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                message: Some("No synced data found in DB or Cache. Please run admin sync.".to_string()),
                cached: Some(false),
                processing_time: None,
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            })
        }
    }
}

fn return_paginated_response(all_emotes: Vec<EmoteResponse>, limit: usize) -> Json<SearchResponse> {
    let total = all_emotes.len();
    let start_index = 0; 
    let end_index = std::cmp::min(start_index + limit, total);
    
    let slice = if start_index < total {
        all_emotes[start_index..end_index].to_vec()
    } else {
        vec![]
    };

    Json(SearchResponse {
        success: true,
        total_found: slice.len() as i32,
        emotes: slice,
        message: None,
        cached: Some(true),
        processing_time: None,
        page: Some(1),
        total_pages: Some(1),
        results_per_page: Some(limit as i32),
        has_next_page: Some(false),
    })
}

async fn sync_user_emotes_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SyncUserEmotesRequest>,
) -> (StatusCode, Json<SearchResponse>) {
    let SyncUserEmotesRequest { user_id, folder_name, image_url } = payload;
    let folder = folder_name;

    if !valid_folder_name(&folder) {
        return user_sync_failed(
            StatusCode::BAD_REQUEST,
            "Invalid folder_name: must be 1-64 chars of [A-Za-z0-9_-]".to_string(),
        );
    }

    let image_url = match normalize_image_url(image_url) {
        Ok(image_url) => image_url,
        Err(message) => return user_sync_failed(StatusCode::BAD_REQUEST, message.to_string()),
    };

    let Some(guard) = RunningUserSyncs::begin(&state.user_syncs, &folder) else {
        return user_sync_failed(
            StatusCode::CONFLICT,
            format!("A sync for {} is already running; it finishes on its own", folder),
        );
    };

    // A big set takes minutes to sync. It runs in its own task so it still finishes and saves
    // when the dashboard or a proxy drops this request.
    let sync = tokio::spawn(async move {
        let _running = guard;
        sync_user_emotes(&state, &user_id, &folder, image_url).await
    });
    sync.await.unwrap_or_else(|e| {
        tracing::error!("User sync task failed: {:?}", e);
        user_sync_failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The sync stopped unexpectedly; see the server logs".to_string(),
        )
    })
}

/// Mirrors the user's active 7TV set into `folder`.
async fn sync_user_emotes(
    state: &AppState,
    user_id: &str,
    folder: &str,
    image_url: Option<String>,
) -> (StatusCode, Json<SearchResponse>) {
    tracing::info!("User sync for {} started", folder);
    let set = match state.seventv.fetch_user_emotes(user_id).await {
        Ok(set) => set,
        Err(e) => {
            tracing::error!("Failed to sync user emotes: {:?}", e);
            return user_sync_failed(StatusCode::BAD_GATEWAY, e.to_string());
        }
    };
    // Syncing an empty result would delete every sticker the user has.
    if set.emotes.is_empty() {
        return user_sync_failed(
            StatusCode::BAD_GATEWAY,
            format!("7TV returned no emotes in the set \"{}\"; nothing was changed", set.set_name),
        );
    }

    let set_ids: Vec<String> = set.emotes.iter().map(|e| e.id.clone()).collect();
    let processed = state.seventv.process_emotes_batch(set.emotes, folder).await;
    if processed.is_empty() {
        return user_sync_failed(
            StatusCode::BAD_GATEWAY,
            format!("None of the {} emotes could be downloaded and stored; nothing was changed", set_ids.len()),
        );
    }

    // Save to Redis with a custom key: "user_emotes:{folder_name}"
    let cache_key = format!("user_emotes:{}", folder);
    let ttl = 86400 * 30; // 30 days retention for user syncs? or indefinite?

    if let Err(e) = state.cache.save_to_cache(&cache_key, &processed, ttl).await {
        tracing::error!("Failed to save synced user emotes to cache: {:?}", e);
    }

    let display_name = set.display_name.unwrap_or_else(|| folder.to_string());
    let removed = match persist_user_emotes(
        &state.db,
        user_id,
        folder,
        &display_name,
        image_url.as_deref(),
        &processed,
        &set_ids,
    )
    .await
    {
        Ok(removed) => removed,
        Err(e) => {
            tracing::error!("Failed to persist user emotes (rolled back): {:?}", e);
            return user_sync_failed(StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e));
        }
    };

    creator_requests::approve_matching_request(&state.db, folder).await;

    let summary = user_sync_summary(
        &set.set_name,
        processed.len(),
        set.skipped_zero_width,
        set_ids.len() - processed.len(),
        removed,
    );
    tracing::info!("User sync for {}: {}", folder, summary);

    (
        StatusCode::OK,
        Json(SearchResponse {
            success: true,
            total_found: processed.len() as i32,
            emotes: processed,
            message: Some(summary),
            cached: Some(false),
            processing_time: None,
            page: Some(1),
            total_pages: Some(1),
            results_per_page: None,
            has_next_page: Some(false),
        }),
    )
}

async fn get_saved_user_emotes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<crate::models::SavedUserEmotesQuery>,
) -> Json<SavedUserEmotesResponse> {
    let limit = params.limit.unwrap_or(100) as i64;

    let user = match sqlx::query_as::<_, SavedUserInfo>(
        "SELECT seven_tv_id, folder_name, display_name, image_url FROM users WHERE folder_name = $1"
    )
    .bind(&params.folder_name)
    .fetch_optional(&state.db)
    .await
    {
        Ok(user) => user,
        Err(e) => {
            tracing::error!("Failed to fetch user metadata from DB: {:?}", e);
            return Json(SavedUserEmotesResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                user: None,
                message: Some(format!("Database error: {}", e)),
                cached: Some(false),
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            });
        }
    };
    
    // In the channel's set order; stickers synced before positions were stored come last.
    let rows = sqlx::query_as::<_, StickerRow>(
        "SELECT seven_tv_id, emote_name, file_name, url, owner_name, tags, animated FROM stickers WHERE folder_name = $1 ORDER BY position NULLS LAST, id LIMIT $2"
    )
    .bind(&params.folder_name)
    .bind(limit)
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(stickers) if !stickers.is_empty() => {
            let emotes: Vec<EmoteResponse> = stickers.into_iter().map(|s| EmoteResponse {
                emote_id: s.seven_tv_id,
                emote_name: s.emote_name,
                file_name: s.file_name,
                url: s.url,
                animated_preview_url: None,
                poster_url: None,
                owner: s.owner_name,
                tags: s.tags,
                animated: Some(s.animated),
                scale: None, // We don't store scale in DB yet, but can be added if needed
                mime: None, // Mime can be inferred or added to DB
            }).collect();

            Json(SavedUserEmotesResponse {
                success: true,
                total_found: emotes.len() as i32,
                emotes,
                user,
                message: None,
                cached: Some(false),
                page: Some(1),
                total_pages: Some(1),
                results_per_page: Some(limit as i32),
                has_next_page: Some(false),
            })
        },
        Ok(_) => {
            Json(SavedUserEmotesResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                user,
                message: Some("No saved emotes found for this folder name".to_string()),
                cached: Some(false),
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            })
        },
        Err(e) => {
            tracing::error!("Failed to fetch stickers from DB: {:?}", e);
            Json(SavedUserEmotesResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                user,
                message: Some(format!("Database error: {}", e)),
                cached: Some(false),
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            })
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedUserEmotesResponse {
    success: bool,
    total_found: i32,
    emotes: Vec<EmoteResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<SavedUserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_pages: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    results_per_page: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_next_page: Option<bool>,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct SavedUserInfo {
    seven_tv_id: String,
    folder_name: String,
    display_name: String,
    image_url: Option<String>,
}

#[derive(sqlx::FromRow)]
struct StickerRow {
    seven_tv_id: String,
    emote_name: String,
    file_name: String,
    url: String,
    owner_name: Option<String>,
    tags: Option<Vec<String>>,
    animated: bool,
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct UserRecord {
    id: i32,
    seven_tv_id: String,
    folder_name: String,
    display_name: String,
    image_url: Option<String>,
    last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    emote_count: Option<i32>,
}


#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsersListResponse {
    success: bool,
    users: Vec<UserRecord>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateUserImageResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<SavedUserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

async fn list_users_handler(
    State(state): State<Arc<AppState>>,
) -> Json<UsersListResponse> {
    let rows = sqlx::query_as::<_, UserRecord>(
        "SELECT id, seven_tv_id, folder_name, display_name, image_url, last_synced_at, emote_count FROM users ORDER BY last_synced_at DESC"
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(users) => Json(UsersListResponse {
            success: true,
            users,
        }),
        Err(e) => {
            tracing::error!("Failed to fetch users: {:?}", e);
            Json(UsersListResponse {
                success: false,
                users: vec![],
            })
        }
    }
}

async fn update_user_image_handler(
    State(state): State<Arc<AppState>>,
    Path(folder_name): Path<String>,
    Json(payload): Json<UpdateUserImageRequest>,
) -> (StatusCode, Json<UpdateUserImageResponse>) {
    if !valid_folder_name(&folder_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(UpdateUserImageResponse {
                success: false,
                user: None,
                message: Some(
                    "Invalid folder_name: must be 1-64 chars of [A-Za-z0-9_-]".to_string(),
                ),
            }),
        );
    }

    let image_url = match normalize_image_url(payload.image_url) {
        Ok(image_url) => image_url,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(UpdateUserImageResponse {
                    success: false,
                    user: None,
                    message: Some(message.to_string()),
                }),
            );
        }
    };

    let updated = sqlx::query_as::<_, SavedUserInfo>(
        "UPDATE users SET image_url = $1 WHERE folder_name = $2 RETURNING seven_tv_id, folder_name, display_name, image_url"
    )
    .bind(image_url.as_deref())
    .bind(&folder_name)
    .fetch_optional(&state.db)
    .await;

    match updated {
        Ok(Some(user)) => (
            StatusCode::OK,
            Json(UpdateUserImageResponse {
                success: true,
                user: Some(user),
                message: Some("User image URL updated".to_string()),
            }),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(UpdateUserImageResponse {
                success: false,
                user: None,
                message: Some("User not found".to_string()),
            }),
        ),
        Err(e) => {
            tracing::error!("Failed to update image URL for {}: {:?}", folder_name, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateUserImageResponse {
                    success: false,
                    user: None,
                    message: Some(format!("Database error: {}", e)),
                }),
            )
        }
    }
}

async fn delete_user_handler(
    State(state): State<Arc<AppState>>,
    Path(folder_name): Path<String>,
) -> (StatusCode, Json<DeleteResponse>) {
    if !valid_folder_name(&folder_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(DeleteResponse {
                success: false,
                message: Some(
                    "Invalid folder_name: must be 1-64 chars of [A-Za-z0-9_-]".to_string(),
                ),
            }),
        );
    }

    if let Err(e) = sqlx::query("DELETE FROM stickers WHERE folder_name = $1")
        .bind(&folder_name)
        .execute(&state.db)
        .await
    {
        tracing::error!("Failed to delete stickers for {}: {:?}", folder_name, e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DeleteResponse {
                success: false,
                message: Some(format!("Failed to delete stickers: {}", e)),
            }),
        );
    }

    let user_delete = sqlx::query("DELETE FROM users WHERE folder_name = $1")
        .bind(&folder_name)
        .execute(&state.db)
        .await;

    let rows_affected = match user_delete {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            tracing::error!("Failed to delete user {}: {:?}", folder_name, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(DeleteResponse {
                    success: false,
                    message: Some(format!("Failed to delete user: {}", e)),
                }),
            );
        }
    };

    if rows_affected == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(DeleteResponse {
                success: false,
                message: Some("User not found".to_string()),
            }),
        );
    }

    let blob_prefix = format!("{}/", folder_name);
    let mut warnings: Vec<String> = Vec::new();

    if let Err(e) = state.storage.delete_blobs_by_prefix(&blob_prefix).await {
        tracing::error!("Failed to delete blobs for {}: {:?}", folder_name, e);
        warnings.push(format!("blob cleanup failed: {}", e));
    }

    let cache_key = format!("user_emotes:{}", folder_name);
    if let Err(e) = state.cache.clear_cache(&cache_key).await {
        tracing::warn!("Failed to clear cache key {}: {:?}", cache_key, e);
        warnings.push(format!("cache cleanup failed: {}", e));
    }

    (
        StatusCode::OK,
        Json(DeleteResponse {
            success: true,
            message: if warnings.is_empty() {
                Some(format!("User '{}' deleted", folder_name))
            } else {
                Some(format!(
                    "User '{}' deleted with warnings: {}",
                    folder_name,
                    warnings.join("; ")
                ))
            },
        }),
    )
}

/// Development only: a file written by the local storage stand-in, or 404.
async fn dev_blob_handler(State(state): State<Arc<AppState>>, Path(path): Path<String>) -> Response {
    let Some(file) = state.storage.local_blob_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = if path.ends_with(".webp") { "image/webp" } else { "application/octet-stream" };
    match tokio::fs::read(&file).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{user_sync_summary, RunningUserSyncs};
    use std::sync::Arc;

    #[test]
    fn a_creator_syncs_once_at_a_time() {
        let syncs = Arc::new(RunningUserSyncs::default());

        let running = RunningUserSyncs::begin(&syncs, "xQc").expect("the first sync starts");
        assert!(RunningUserSyncs::begin(&syncs, "xQc").is_none());
        assert!(RunningUserSyncs::begin(&syncs, "Vinesauce").is_some());

        drop(running);
        assert!(RunningUserSyncs::begin(&syncs, "xQc").is_some());
    }

    #[test]
    fn user_sync_summary_mentions_only_what_happened() {
        assert_eq!(
            user_sync_summary("Emotes", 120, 0, 0, 0),
            "Synced 120 emotes from the 7TV set \"Emotes\""
        );
        assert_eq!(
            user_sync_summary("Dona emotes", 77, 1, 0, 1),
            "Synced 77 emotes from the 7TV set \"Dona emotes\", skipped 1 zero-width, \
             removed 1 sticker no longer in the set"
        );
        assert_eq!(
            user_sync_summary("Halloween", 950, 34, 3, 12),
            "Synced 950 emotes from the 7TV set \"Halloween\", skipped 34 zero-width, \
             3 failed to download or store (see the server logs), removed 12 stickers no longer in the set"
        );
    }
}
