use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EmoteResponse {
    pub file_name: String,
    pub url: String,
    pub emote_id: String,
    pub emote_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub success: bool,
    pub total_found: i32,
    pub emotes: Vec<EmoteResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results_per_page: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_next_page: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(alias = "perPage")]
    pub limit: Option<i32>,
    pub animated_only: Option<bool>,
    pub page: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrendingPeriod {
    #[serde(rename = "trending_daily")]
    Daily,
    #[serde(rename = "trending_weekly")]
    Weekly,
    #[serde(rename = "trending_monthly")]
    Monthly,
    #[serde(rename = "popularity")]
    AllTime,
}

impl Default for TrendingPeriod {
    fn default() -> Self {
        Self::Weekly
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncTrendingRequest {
    pub period: Option<String>,
    pub animated_only: Option<bool>,
    pub limit: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncUserEmotesRequest {
    pub user_id: String,
    pub limit: Option<i32>,
    pub folder_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedUserEmotesQuery {
    pub folder_name: String,
    pub limit: Option<i32>,
}
