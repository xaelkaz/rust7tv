use crate::models::{EmoteResponse, TrendingPeriod};
use crate::services::storage::StorageService;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use futures::stream::{self, StreamExt};
use reqwest::header::CONTENT_TYPE;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Emote {
    pub id: String,
    pub default_name: Option<String>,
    pub name: Option<String>,
    pub owner: Option<Owner>,
    pub images: Option<Vec<Image>>,
    pub host: Option<TrendingHost>,
    pub animated: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub main_connection: Option<MainConnection>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MainConnection {
    pub platform_display_name: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub url: String,
    pub mime: String,
    pub size: i32,
    pub scale: i32,
    pub width: i32,
    pub frame_count: i32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrendingHost {
    pub url: String,
    pub files: Vec<TrendingFile>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrendingFile {
    pub name: String,
    pub format: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Serialize)]
struct GqlRequest<'a> {
    query: &'a str,
    variables: serde_json::Value,
}

pub struct SevenTVService {
    client: reqwest::Client,
    storage: Arc<StorageService>,
}

impl SevenTVService {
    pub fn new(storage: Arc<StorageService>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            storage,
        }
    }

    pub async fn search_emotes(
        &self,
        query: &str,
        page: i32,
        limit: i32,
        animated_only: bool,
    ) -> Result<Vec<Emote>, Box<dyn std::error::Error + Send + Sync>> {
        let gql = r#"
        query EmoteSearch($query: String, $tags: [String!]!, $sortBy: SortBy!, $filters: Filters, $page: Int, $perPage: Int!, $isDefaultSetSet: Boolean!, $defaultSetId: Id!) {
          emotes {
            search(
              query: $query
              tags: { tags: $tags, match: ANY }
              sort: { sortBy: $sortBy, order: DESCENDING }
              filters: $filters
              page: $page
              perPage: $perPage
            ) {
              items {
                id
                defaultName
                owner {
                  mainConnection {
                    platformDisplayName
                  }
                }
                images {
                  url
                  mime
                  size
                  scale
                  width
                  frameCount
                }
                ranking(ranking: TRENDING_WEEKLY)
                inEmoteSets(emoteSetIds: [$defaultSetId]) @include(if: $isDefaultSetSet) {
                  emoteSetId
                  emote {
                    id
                    alias
                  }
                }
              }
              totalCount
              pageCount
            }
          }
        }
        "#;

        let variables = serde_json::json!({
            "defaultSetId": "",
            "filters": { "animated": animated_only },
            "isDefaultSetSet": false,
            "page": page,
            "perPage": limit,
            "query": query,
            "sortBy": "TOP_ALL_TIME",
            "tags": []
        });

        let resp = self.client.post("https://api.7tv.app/v4/gql")
            .header(CONTENT_TYPE, "application/json")
            .json(&GqlRequest { query: gql, variables })
            .send()
            .await?;

        let status = resp.status();
        tracing::info!("7TV Search API Response Status: {}", status);

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            tracing::error!("7TV Search API Error Body: {}", error_text);
            return Err(format!("7TV API Error: {} - {}", status, error_text).into());
        }

        let body_text = resp.text().await?;
        // tracing::debug!("7TV Search API Response Body: {}", body_text);

        let body: serde_json::Value = serde_json::from_str(&body_text)?;
        let items = body["data"]["emotes"]["search"]["items"]
            .as_array()
            .ok_or("Invalid response format: missing data.emotes.search.items")?;
        
        let emotes: Vec<Emote> = serde_json::from_value(serde_json::Value::Array(items.clone()))?;
        Ok(emotes)
    }

    pub async fn fetch_trending_emotes(
        &self,
        period: &TrendingPeriod,
        limit: i32,
        animated_only: bool
    ) -> Result<Vec<Emote>, Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Fetching trending emotes: period={:?}, limit={}, animated={}", period, limit, animated_only);
        
        let gql = r#"
        query GetTrendingEmotes($perPage: Int, $filters: Filters, $sortBy: SortBy!) {
            emotes {
                search(query: "", perPage: $perPage, filters: $filters, sort: { sortBy: $sortBy, order: DESCENDING }) {
                    items {
                        id
                        defaultName
                        images {
                            url
                            mime
                            size
                            scale
                            width
                            frameCount
                        }
                        owner {
                            mainConnection {
                                platformDisplayName
                            }
                        }
                    }
                }
            }
        }
        "#;

        let sort_by = match period {
            TrendingPeriod::Daily => "TRENDING_DAILY",
            TrendingPeriod::Weekly => "TRENDING_WEEKLY",
            TrendingPeriod::Monthly => "TRENDING_MONTHLY",
            TrendingPeriod::AllTime => "TOP_ALL_TIME",
        };

        let variables = serde_json::json!({
            "perPage": limit,
            "filters": { "animated": animated_only },
            "sortBy": sort_by,
        });

        let resp = self.client.post("https://api.7tv.app/v4/gql")
            .header(CONTENT_TYPE, "application/json")
            .json(&GqlRequest { query: gql, variables })
            .send()
            .await?;

        let status = resp.status();
        tracing::info!("7TV API Response Status: {}", status);

        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            tracing::error!("7TV API Error Body: {}", error_text);
            return Err(format!("7TV API Error: {} - {}", status, error_text).into());
        }

        let body_text = resp.text().await?;
        // tracing::debug!("7TV API Response Body: {}", body_text);

        let body: serde_json::Value = serde_json::from_str(&body_text)?;
        let items = body["data"]["emotes"]["search"]["items"]
            .as_array()
            .ok_or("Invalid response format: missing data.emotes.search.items")?;

        let emotes: Vec<Emote> = serde_json::from_value(serde_json::Value::Array(items.clone()))?;
        Ok(emotes)
    }

    pub async fn fetch_user_emotes(
        &self,
        user_id: &str,
        limit: i32,
    ) -> Result<Vec<Emote>, Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Fetching user emotes: user_id={}, limit={}", user_id, limit);

        let gql = r#"
        query SearchEmotesInActiveSet($userId: Id!, $query: String, $page: Int!, $isDefaultSetSet: Boolean!, $defaultSetId: Id!, $perPage: Int!) {
          users {
            user(id: $userId) {
              style {
                activeEmoteSet {
                  id
                  emotes(query: $query, page: $page, perPage: $perPage) {
                    items {
                      id
                      defaultName
                      owner {
                        mainConnection {
                          platformDisplayName
                        }
                      }
                      images {
                        url
                        mime
                        size
                        scale
                        width
                        frameCount
                      }
                      animated: flags {
                        animated: zeroWidth 
                      }
                    }
                  }
                }
              }
            }
          }
        }
        "#;

        // Note: The original GraphQL query had "animated" inside flags? No, the user provided query uses "flags { zeroWidth }" but standard emote structure has "animated". 
        // Actually, looking at the user payload, `animated` isn't directly on `items` -> `emote`. 
        // Wait, the user provided query returns `items { emote { ... } }`, but my `Emote` struct is flat.
        // The `SearchEmotesInActiveSet` query structure returning `items { emote { ... } }` is different from `fetch_trending_emotes` which returns `items { ... }` directly?
        // Let's re-examine the user provided query carefully.
        // It returns `items { emote { id ... } }`.
        // However, my `Emote` struct expects fields at the top level.
        // I should probably adjust the query aliases or post-process the JSON to match `Emote` struct, OR adapt `Emote` struct (but it's shared).
        // Let's look at `fetch_trending_emotes` results. It returns a list of emotes.
        // User query: `items` is a list of objects containing `emote`.
        // I will write a custom struct for this response internally or just decode to Value and map. 
        // Mapping is safer.
        //
        // Re-writing the query to be simpler and closer to what we need if possible, OR just use the user provided one and parse manually.
        // User provided one:
        /*
        items {
              emote {
                id
                defaultName
                owner { ... }
                images { ... }
              }
        }
        */
        
        let gql = r#"
        query SearchEmotesInActiveSet($userId: Id!, $perPage: Int!) {
          users {
            user(id: $userId) {
              style {
                activeEmoteSet {
                  emotes(page: 1, perPage: $perPage) {
                    items {
                      emote {
                        id
                        defaultName
                        owner {
                          mainConnection {
                            platformDisplayName
                          }
                        }
                        images {
                            url
                            mime
                            size
                            scale
                            width
                            frameCount
                        }
                      }
                    }
                  }
                }
              }
            }
          }
        }
        "#;

        let variables = serde_json::json!({
            "userId": user_id,
            "perPage": limit,
        });

        let resp = self.client.post("https://api.7tv.app/v4/gql")
            .header(CONTENT_TYPE, "application/json")
            .json(&GqlRequest { query: gql, variables })
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(format!("7TV API Error: {} - {}", status, error_text).into());
        }

        let body_text = resp.text().await?;
        let body: serde_json::Value = serde_json::from_str(&body_text)?;
        
        // Traverse path: data.users.user.style.activeEmoteSet.emotes.items
        let items_wrapper = body["data"]["users"]["user"]["style"]["activeEmoteSet"]["emotes"]["items"]
            .as_array()
            .ok_or("Invalid response format: missing emotes list")?;

        // Extract "emote" field from each item to get the actual emote data
        let emotes_json: Vec<serde_json::Value> = items_wrapper.iter()
            .filter_map(|item| item.get("emote").cloned())
            .collect();

        let emotes: Vec<Emote> = serde_json::from_value(serde_json::Value::Array(emotes_json))?;
        Ok(emotes)
    }

    pub async fn process_emotes_batch(
        &self,
        emotes: Vec<Emote>,
        folder: &str,
    ) -> Vec<EmoteResponse> {
        let storage = Arc::clone(&self.storage);
        let folder = folder.to_string();
        
        stream::iter(emotes)
            .map(|e| {
                let storage = Arc::clone(&storage);
                let folder = folder.clone();
                let client = self.client.clone();
                async move {
                    process_single_emote(client, e, storage, &folder).await
                }
            })
            .buffer_unordered(5) // Reduced concurrency to prevent timeouts
            .filter_map(|res| async move { res })
            .collect()
            .await
    }
}

async fn process_single_emote(
    client: reqwest::Client,
    e: Emote,
    storage: Arc<StorageService>,
    folder: &str,
) -> Option<EmoteResponse> {
    let images = if let Some(imgs) = &e.images {
        imgs.clone()
    } else if let Some(host) = &e.host {
        // Construct images from host files if images array is missing (trending endpoint)
        let animated = e.animated.unwrap_or(false);
        host.files.iter().map(|f| {
            let scale_str = f.name.trim_end_matches(&format!("x.{}", f.format)); // simplistic parsing
            let scale = scale_str.parse().unwrap_or(1);
            let mime = format!("image/{}", f.format);
            let url = format!("https:{}/{}", host.url, f.name);
            Image {
                url,
                mime,
                size: 0,
                scale,
                width: f.width,
                frame_count: if animated { 2 } else { 1 },
            }
        }).collect()
    } else {
        return None;
    };

    let best_image = select_best_image(&images)?;

    let resp = client.get(&best_image.url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data = resp.bytes().await.ok()?.to_vec();

    let extension = match best_image.mime.as_str() {
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        "image/avif" => ".avif",
        _ => ".png",
    };

    let name = e.default_name.as_deref().or(e.name.as_deref())?;
    // sanitize name
    let safe_name: String = name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect();
    
    // Append ID to prevent filename collisions (e.g. multiple "lol" emotes overwriting each other)
    let file_name = format!("{}_{}{}", safe_name, e.id, extension);
    let blob_name = format!("{}/{}", folder, file_name);

    let url = storage.upload_blob(data, &blob_name, &best_image.mime).await.ok()?;

    Some(EmoteResponse {
        file_name,
        url,
        emote_id: e.id,
        emote_name: name.to_string(),
        owner: e.owner.and_then(|o| o.main_connection.map(|c| c.platform_display_name)),
        animated: Some(best_image.frame_count > 1),
        scale: Some(best_image.scale),
        mime: Some(best_image.mime.clone()),
    })
}

fn select_best_image(images: &[Image]) -> Option<&Image> {
    if images.is_empty() { return None; }
    
    // Sort by checking if animated first, then mime preference, then scale
    // This is a simplified logic compared to Go but sufficient
    let preferred_mimes = ["image/webp", "image/gif", "image/avif", "image/png"];
    
    images.iter().max_by(|a, b| {
        let a_anim = a.frame_count > 1;
        let b_anim = b.frame_count > 1;
        if a_anim != b_anim {
            return a_anim.cmp(&b_anim); 
        }
        
        // Both same animation status
        let a_score = preferred_mimes.iter().position(|&m| m == a.mime).unwrap_or(100);
        let b_score = preferred_mimes.iter().position(|&m| m == b.mime).unwrap_or(100);
        
        if a_score != b_score {
            // Lower index is better (preferred_mimes is best-first)
            return b_score.cmp(&a_score);
        }
        
        a.scale.cmp(&b.scale)
    })
}
