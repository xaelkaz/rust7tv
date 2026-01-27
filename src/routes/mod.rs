use axum::{
    routing::{get, post},
    Router,
    Json,
    extract::{State, Query},
};
use std::sync::Arc;
use crate::AppState;
use crate::models::{TrendingPeriod, SearchResponse};
use serde::Deserialize;

pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/api/search-emotes", post(search_emotes_handler))
        .route("/api/trending/emotes", get(trending_emotes_handler))
        .with_state(state)
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
            let processed = state.seventv.process_emotes_batch(emotes, "emotes").await;
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
