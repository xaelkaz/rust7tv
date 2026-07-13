use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EmoteResponse {
    pub file_name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animated_preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
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
#[serde(rename_all = "camelCase")]
pub struct TrendingTagResponse {
    pub success: bool,
    pub tags: Vec<TrendingTag>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrendingTag {
    pub tag: String,
    pub count: i64,
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
    pub image_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUserImageRequest {
    pub image_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedUserEmotesQuery {
    pub folder_name: String,
    pub limit: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::EmoteResponse;

    #[test]
    fn emote_response_serializes_optional_search_urls_as_camel_case() {
        let response = EmoteResponse {
            file_name: "GIGACHAD_id.webp".to_string(),
            url: "https://cdn.7tv.app/emote/id/4x.webp".to_string(),
            animated_preview_url: Some("https://cdn.7tv.app/emote/id/2x.webp".to_string()),
            poster_url: Some("https://cdn.7tv.app/emote/id/4x_static.webp".to_string()),
            emote_id: "id".to_string(),
            emote_name: "GIGACHAD".to_string(),
            owner: None,
            animated: Some(true),
            scale: Some(4),
            mime: Some("image/webp".to_string()),
            tags: Some(vec![]),
        };

        let json = serde_json::to_value(response).unwrap();

        assert_eq!(json["url"], "https://cdn.7tv.app/emote/id/4x.webp");
        assert_eq!(
            json["animatedPreviewUrl"],
            "https://cdn.7tv.app/emote/id/2x.webp"
        );
        assert_eq!(
            json["posterUrl"],
            "https://cdn.7tv.app/emote/id/4x_static.webp"
        );
    }
}
