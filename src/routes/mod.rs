use axum::{
    routing::{get, post, delete},
    Router,
    Json,
    middleware,
    extract::{State, Query, Path},
    http::StatusCode,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use crate::AppState;
use crate::models::{TrendingPeriod, SearchResponse, SyncTrendingRequest, EmoteResponse, TrendingTagResponse, TrendingTag};
use serde::{Deserialize, Serialize};

mod auth;
mod dashboard;

pub fn create_router(state: Arc<AppState>) -> Router {
    let public = Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/api/search-emotes", post(search_emotes_handler))
        .route("/api/trending/tags", get(trending_tags_handler))
        .route("/api/trending/emotes", get(trending_emotes_handler))
        .route("/api/trending/synced", get(synced_trending_emotes_handler))
        .route("/api/user/emotes/saved", get(get_saved_user_emotes_handler))
        // Listing users is a read-only feature the client app needs, so it is
        // public. `/api/users` is the canonical path; `/api/admin/users` is kept
        // as a public alias so existing app builds keep working. The destructive
        // DELETE on a user stays admin-protected below.
        .route("/api/users", get(list_users_handler))
        .route("/api/admin/users", get(list_users_handler));

    let admin = Router::new()
        .route("/admin/dashboard", get(dashboard::dashboard_handler))
        .route("/api/admin/sync-trending", post(sync_trending_handler))
        .route("/api/admin/sync-user-emotes", post(sync_user_emotes_handler))
        .route("/api/admin/users/:folder_name", delete(delete_user_handler))
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

/// Folder names are used as Azure blob prefixes and as SQL bind params.
/// Restrict to a safe ASCII subset so an empty/malicious value can't wipe the
/// whole container (e.g. `folder_name=""` → `prefix="/"` would list all blobs).
fn valid_folder_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

/// Upsert the user record and all of their stickers atomically.
async fn persist_user_emotes(
    pool: &sqlx::PgPool,
    user_id: &str,
    folder: &str,
    display_name: &str,
    emotes: &[EmoteResponse],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        r#"
        INSERT INTO users (seven_tv_id, folder_name, display_name, last_synced_at, emote_count)
        VALUES ($1, $2, $3, NOW(), $4)
        ON CONFLICT (folder_name)
        DO UPDATE SET
            seven_tv_id = EXCLUDED.seven_tv_id,
            display_name = EXCLUDED.display_name,
            last_synced_at = NOW(),
            emote_count = EXCLUDED.emote_count
        "#,
    )
    .bind(user_id)
    .bind(folder)
    .bind(display_name)
    .bind(emotes.len() as i32)
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
                .push_bind(folder);
        });
        qb.push(
            " ON CONFLICT (seven_tv_id, folder_name) DO UPDATE SET \
             emote_name = EXCLUDED.emote_name, \
             file_name = EXCLUDED.file_name, \
             url = EXCLUDED.url, \
             owner_name = EXCLUDED.owner_name, \
             tags = EXCLUDED.tags, \
             animated = EXCLUDED.animated",
        );
        qb.build().execute(&mut *tx).await?;
    }

    tx.commit().await?;
    Ok(())
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
    let cache_key = crate::services::cache::CacheService::get_cache_key(&payload.query, limit, animated_only);
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
    Json(payload): Json<crate::models::SyncUserEmotesRequest>,
) -> (StatusCode, Json<SearchResponse>) {
    let limit = payload.limit.unwrap_or(100);
    let folder = payload.folder_name;

    if !valid_folder_name(&folder) {
        return (
            StatusCode::BAD_REQUEST,
            Json(SearchResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                message: Some(
                    "Invalid folder_name: must be 1-64 chars of [A-Za-z0-9_-]".to_string(),
                ),
                cached: Some(false),
                processing_time: None,
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            }),
        );
    }

    // Removed: We no longer cleanup existing blobs in that user folder to prevent breaking user favorites.
    // The ON CONFLICT DO UPDATE handles database logical cleanup and override.

    match state.seventv.fetch_user_emotes(&payload.user_id, limit).await {
        Ok(emotes) => {
            let processed = state.seventv.process_emotes_batch(emotes, &folder).await;
            
            // Save to Redis with a custom key: "user_emotes:{folder_name}"
            let cache_key = format!("user_emotes:{}", folder);
            let ttl = 86400 * 30; // 30 days retention for user syncs? or indefinite?
            
            if let Err(e) = state.cache.save_to_cache(&cache_key, &processed, ttl).await {
                tracing::error!("Failed to save synced user emotes to cache: {:?}", e);
            }

            // Update Database
            let user_display_name = if let Some(first_emote) = processed.first() {
                first_emote.owner.clone().unwrap_or_else(|| "Unknown".to_string())
            } else {
                "Unknown".to_string()
            };

            if let Err(e) = persist_user_emotes(
                &state.db,
                &payload.user_id,
                &folder,
                &user_display_name,
                &processed,
            )
            .await
            {
                tracing::error!("Failed to persist user emotes (rolled back): {:?}", e);
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
                    message: Some("User emotes synced successfully".to_string()),
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
            tracing::error!("Failed to sync user emotes: {:?}", e);
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

async fn get_saved_user_emotes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<crate::models::SavedUserEmotesQuery>,
) -> Json<SearchResponse> {
    let limit = params.limit.unwrap_or(100) as i64;
    
    // Query stickers from database
    let rows = sqlx::query_as::<_, StickerRow>(
        "SELECT seven_tv_id, emote_name, file_name, url, owner_name, tags, animated FROM stickers WHERE folder_name = $1 LIMIT $2"
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
                owner: s.owner_name,
                tags: s.tags,
                animated: Some(s.animated),
                scale: None, // We don't store scale in DB yet, but can be added if needed
                mime: None, // Mime can be inferred or added to DB
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
        Ok(_) => {
            Json(SearchResponse {
                success: false,
                total_found: 0,
                emotes: vec![],
                message: Some("No saved emotes found for this folder name".to_string()),
                cached: Some(false),
                processing_time: None,
                page: None,
                total_pages: None,
                results_per_page: None,
                has_next_page: None,
            })
        },
        Err(e) => {
            tracing::error!("Failed to fetch stickers from DB: {:?}", e);
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
            })
        }
    }
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

async fn list_users_handler(
    State(state): State<Arc<AppState>>,
) -> Json<UsersListResponse> {
    let rows = sqlx::query_as::<_, UserRecord>(
        "SELECT id, seven_tv_id, folder_name, display_name, last_synced_at, emote_count FROM users ORDER BY last_synced_at DESC"
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
