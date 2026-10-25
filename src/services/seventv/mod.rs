use crate::models::{EmoteResponse, TrendingPeriod};
use crate::services::storage::StorageService;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use futures::stream::{self, StreamExt};
use reqwest::header::CONTENT_TYPE;

/// Emotes per request when reading a user's emote set; a set holds up to 1000.
const USER_SET_PAGE_SIZE: i32 = 250;

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
    pub tags: Option<Vec<String>>,
    /// Only requested by the user sync.
    pub flags: Option<EmoteFlags>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EmoteFlags {
    /// Made to be layered over another emote (rain, hands, a frame), not to be used on its own.
    pub default_zero_width: bool,
}

/// A user's active emote set, as the user sync mirrors it.
#[derive(Debug)]
pub struct UserEmoteSet {
    /// The channel's name on its main platform, e.g. "xQc".
    pub display_name: Option<String>,
    pub set_name: String,
    /// Every emote in the set, in 7TV's order, once each and without zero-width ones.
    pub emotes: Vec<Emote>,
    pub skipped_zero_width: usize,
}

/// One page of a user's active emote set.
#[derive(Debug)]
struct UserEmotePage {
    display_name: Option<String>,
    set_id: String,
    set_name: String,
    page_count: i32,
    emotes: Vec<Emote>,
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
            .expect("failed to build reqwest client for SevenTV");

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
                tags
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
        let body: serde_json::Value = serde_json::from_str(&body_text)?;
        
        if let Some(errors) = body.get("errors") {
            tracing::error!("7TV Search API GraphQL Errors: {:?}", errors);
            return Err(format!("7TV GraphQL Error: {}", errors).into());
        }

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
                        tags
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
        let body: serde_json::Value = serde_json::from_str(&body_text)?;

        if let Some(errors) = body.get("errors") {
            tracing::error!("7TV Trending API GraphQL Errors: {:?}", errors);
            return Err(format!("7TV GraphQL Error: {}", errors).into());
        }

        let items = body["data"]["emotes"]["search"]["items"]
            .as_array()
            .ok_or("Invalid response format: missing data.emotes.search.items")?;

        let emotes: Vec<Emote> = serde_json::from_value(serde_json::Value::Array(items.clone()))?;
        Ok(emotes)
    }

    /// Reads every page of the user's active emote set.
    pub async fn fetch_user_emotes(
        &self,
        user_id: &str,
    ) -> Result<UserEmoteSet, Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Fetching user emotes: user_id={}", user_id);

        let mut pages: Vec<UserEmotePage> = Vec::new();
        let mut page = 1;
        loop {
            let body = self.fetch_user_emote_page(user_id, page).await?;
            let current = parse_user_emote_page(&body)?;
            if pages.first().is_some_and(|first| first.set_id != current.set_id) {
                return Err("The user's active emote set changed during the sync; try again".into());
            }
            let page_count = current.page_count;
            pages.push(current);
            if page >= page_count {
                break;
            }
            page += 1;
        }

        Ok(collect_user_emote_set(pages))
    }

    async fn fetch_user_emote_page(
        &self,
        user_id: &str,
        page: i32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let gql = r#"
        query ActiveEmoteSetPage($userId: Id!, $page: Int!, $perPage: Int!) {
          users {
            user(id: $userId) {
              mainConnection {
                platformDisplayName
              }
              style {
                activeEmoteSet {
                  id
                  name
                  emotes(page: $page, perPage: $perPage) {
                    pageCount
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
                        tags
                        flags {
                          defaultZeroWidth
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
            "page": page,
            "perPage": USER_SET_PAGE_SIZE,
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
        
        if let Some(errors) = body.get("errors") {
            tracing::error!("7TV User Emotes API GraphQL Errors: {:?}", errors);
            return Err(format!("7TV GraphQL Error: {}", errors).into());
        }

        Ok(body)
    }

    /// Downloads each emote and stores it under `folder`, keeping the order they came in.
    /// An emote that can't be downloaded or stored is logged and left out.
    pub async fn process_emotes_batch(
        &self,
        emotes: Vec<Emote>,
        folder: &str,
    ) -> Vec<EmoteResponse> {
        let total = emotes.len();
        let storage = Arc::clone(&self.storage);
        let folder = folder.to_string();

        let mut stored: Vec<(usize, EmoteResponse)> = stream::iter(emotes.into_iter().enumerate())
            .map(|(index, e)| {
                let storage = Arc::clone(&storage);
                let folder = folder.clone();
                let client = self.client.clone();
                async move {
                    let id = e.id.clone();
                    let name = e.default_name.clone().or_else(|| e.name.clone()).unwrap_or_default();
                    match process_single_emote(client, e, storage, &folder).await {
                        Ok(emote) => Some((index, emote)),
                        Err(reason) => {
                            tracing::warn!("Skipped emote {} ({}) for {}: {}", id, name, folder, reason);
                            None
                        }
                    }
                }
            })
            .buffer_unordered(5) // Reduced concurrency to prevent timeouts
            .filter_map(|res| async move { res })
            .collect()
            .await;

        if stored.len() < total {
            tracing::warn!("Stored {} of {} emotes for {}", stored.len(), total, folder);
        }
        stored.sort_by_key(|(index, _)| *index);
        stored.into_iter().map(|(_, emote)| emote).collect()
    }

    pub fn process_emotes_batch_no_storage(&self, emotes: Vec<Emote>) -> Vec<EmoteResponse> {
        emotes.into_iter()
            .filter_map(|e| {
                let images = if let Some(imgs) = &e.images {
                    imgs.clone()
                } else if let Some(host) = &e.host {
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
                let animated_preview_url = select_animated_preview(&images).map(normalize_url);
                let poster_url = select_poster(&images).map(normalize_url);

                let extension = match best_image.mime.as_str() {
                    "image/webp" => ".webp",
                    "image/gif" => ".gif",
                    "image/avif" => ".avif",
                    _ => ".png",
                };

                let name = e.default_name.as_deref().or(e.name.as_deref())?;
                let safe_name: String = name.chars()
                    .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ' ' { c } else { '_' })
                    .collect();
                
                let file_name = format!("{}_{}{}", safe_name, e.id, extension);
                
                let mut url = best_image.url.clone();
                if url.starts_with("//") {
                    url = format!("https:{}", url);
                }

                Some(EmoteResponse {
                    file_name,
                    url,
                    animated_preview_url,
                    poster_url,
                    emote_id: e.id,
                    emote_name: name.to_string(),
                    owner: e.owner.and_then(|o| o.main_connection.map(|c| c.platform_display_name)),
                    animated: Some(best_image.frame_count > 1),
                    scale: Some(best_image.scale),
                    mime: Some(best_image.mime.clone()),
                    tags: e.tags.clone(),
                })
            })
            .collect()
    }
}

/// Reads one page of `users.user.style.activeEmoteSet`.
fn parse_user_emote_page(
    body: &serde_json::Value,
) -> Result<UserEmotePage, Box<dyn std::error::Error + Send + Sync>> {
    let user = &body["data"]["users"]["user"];
    if user.is_null() {
        return Err("7TV user not found".into());
    }
    let set = &user["style"]["activeEmoteSet"];
    if set.is_null() {
        return Err("This 7TV user has no active emote set".into());
    }

    let items_wrapper = set["emotes"]["items"]
        .as_array()
        .ok_or("Invalid response format: missing emotes list")?;

    // Extract "emote" field from each item to get the actual emote data
    let emotes_json: Vec<serde_json::Value> = items_wrapper.iter()
        .filter_map(|item| item.get("emote").cloned())
        .collect();

    Ok(UserEmotePage {
        display_name: user["mainConnection"]["platformDisplayName"].as_str().map(str::to_string),
        set_id: set["id"].as_str().unwrap_or_default().to_string(),
        set_name: set["name"].as_str().unwrap_or_default().to_string(),
        page_count: set["emotes"]["pageCount"].as_i64().unwrap_or(1) as i32,
        emotes: serde_json::from_value(serde_json::Value::Array(emotes_json))?,
    })
}

fn collect_user_emote_set(pages: Vec<UserEmotePage>) -> UserEmoteSet {
    let display_name = pages.first().and_then(|page| page.display_name.clone());
    let set_name = pages.first().map(|page| page.set_name.clone()).unwrap_or_default();

    let mut seen = HashSet::new();
    let mut emotes = Vec::new();
    let mut skipped_zero_width = 0;
    for emote in pages.into_iter().flat_map(|page| page.emotes) {
        // A set can hold the same emote twice under different aliases.
        if !seen.insert(emote.id.clone()) {
            continue;
        }
        if emote.flags.as_ref().is_some_and(|flags| flags.default_zero_width) {
            skipped_zero_width += 1;
            continue;
        }
        emotes.push(emote);
    }

    UserEmoteSet {
        display_name,
        set_name,
        emotes,
        skipped_zero_width,
    }
}

/// Downloads the emote's best image and stores it, or says why it couldn't.
async fn process_single_emote(
    client: reqwest::Client,
    e: Emote,
    storage: Arc<StorageService>,
    folder: &str,
) -> Result<EmoteResponse, String> {
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
        return Err("no images".to_string());
    };

    let best_image = select_best_image(&images).ok_or("no images")?;
    let name = e.default_name.as_deref().or(e.name.as_deref()).ok_or("no name")?;

    let resp = client.get(&best_image.url).send().await
        .map_err(|err| format!("download failed: {}", err))?;
    if !resp.status().is_success() {
        return Err(format!("download returned {}", resp.status()));
    }
    let data = resp.bytes().await
        .map_err(|err| format!("download failed: {}", err))?
        .to_vec();

    let extension = match best_image.mime.as_str() {
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        "image/avif" => ".avif",
        _ => ".png",
    };

    // sanitize name
    let safe_name: String = name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect();

    // Append ID to prevent filename collisions (e.g. multiple "lol" emotes overwriting each other)
    let file_name = format!("{}_{}{}", safe_name, e.id, extension);
    let blob_name = format!("{}/{}", folder, file_name);

    let url = storage.upload_blob(data, &blob_name, &best_image.mime).await
        .map_err(|err| format!("upload failed: {}", err))?;

    Ok(EmoteResponse {
        file_name,
        url,
        animated_preview_url: None,
        poster_url: None,
        emote_id: e.id,
        emote_name: name.to_string(),
        owner: e.owner.and_then(|o| o.main_connection.map(|c| c.platform_display_name)),
        animated: Some(best_image.frame_count > 1),
        scale: Some(best_image.scale),
        mime: Some(best_image.mime.clone()),
        tags: e.tags.clone(),
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

fn select_animated_preview(images: &[Image]) -> Option<&Image> {
    let animated_webp = images
        .iter()
        .filter(|image| image.mime == "image/webp" && image.frame_count > 1);

    animated_webp
        .clone()
        .find(|image| image.scale == 2)
        .or_else(|| {
            animated_webp.min_by(|a, b| {
                let a_distance = (a.scale - 2).abs();
                let b_distance = (b.scale - 2).abs();
                a_distance
                    .cmp(&b_distance)
                    .then_with(|| b.scale.cmp(&a.scale))
            })
        })
}

fn select_poster(images: &[Image]) -> Option<&Image> {
    images
        .iter()
        .filter(|image| image.mime == "image/webp" && image.frame_count == 1)
        .max_by_key(|image| image.scale)
}

fn normalize_url(image: &Image) -> String {
    if image.url.starts_with("//") {
        format!("https:{}", image.url)
    } else {
        image.url.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_user_emote_set, parse_user_emote_page, select_animated_preview, select_best_image,
        select_poster, Image, USER_SET_PAGE_SIZE,
    };
    use crate::config::Config;
    use crate::services::storage::StorageService;
    use serde_json::json;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn user_page(page_count: i32, items: serde_json::Value) -> serde_json::Value {
        json!({ "data": { "users": { "user": {
            "mainConnection": { "platformDisplayName": "xQc" },
            "style": { "activeEmoteSet": {
                "id": "01SET",
                "name": "Halloween Emotes 2026",
                "emotes": { "pageCount": page_count, "items": items }
            } }
        } } } })
    }

    fn set_emote(id: &str, zero_width: bool) -> serde_json::Value {
        json!({ "emote": {
            "id": id,
            "defaultName": id,
            "images": [],
            "tags": [],
            "flags": { "defaultZeroWidth": zero_width }
        } })
    }

    #[test]
    fn user_set_keeps_order_without_zero_width_or_repeats() {
        let pages = vec![
            parse_user_emote_page(&user_page(2, json!([
                set_emote("GAMBA", false),
                set_emote("RainTime", true),
                set_emote("xqcWOT", false),
            ]))).unwrap(),
            parse_user_emote_page(&user_page(2, json!([
                set_emote("GAMBA", false),
                set_emote("XMART", false),
            ]))).unwrap(),
        ];

        let set = collect_user_emote_set(pages);

        let ids: Vec<&str> = set.emotes.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["GAMBA", "xqcWOT", "XMART"]);
        assert_eq!(set.skipped_zero_width, 1);
        assert_eq!(set.display_name.as_deref(), Some("xQc"));
        assert_eq!(set.set_name, "Halloween Emotes 2026");
    }

    #[test]
    fn user_page_reads_page_count_and_explains_a_missing_user_or_set() {
        assert_eq!(parse_user_emote_page(&user_page(4, json!([]))).unwrap().page_count, 4);

        let no_user = json!({ "data": { "users": { "user": null } } });
        assert_eq!(
            parse_user_emote_page(&no_user).unwrap_err().to_string(),
            "7TV user not found"
        );

        let no_set = json!({ "data": { "users": { "user": { "style": { "activeEmoteSet": null } } } } });
        assert_eq!(
            parse_user_emote_page(&no_set).unwrap_err().to_string(),
            "This 7TV user has no active emote set"
        );
    }

    fn live_service() -> super::SevenTVService {
        let config = Config {
            port: "0".to_string(),
            redis_host: String::new(),
            redis_port: String::new(),
            redis_password: String::new(),
            redis_db: 0,
            redis_url: String::new(),
            azure_conn_str: String::new(),
            container_name: "unused".to_string(),
            cache_ttl: 0,
            trending_cache_ttl: 0,
            api_title: String::new(),
            api_description: String::new(),
            api_version: String::new(),
            database_url: String::new(),
            admin_token: String::new(),
            admin_user: String::new(),
            admin_password: String::new(),
            local_blob_dir: String::new(),
            public_base_url: String::new(),
        };
        super::SevenTVService::new(Arc::new(StorageService::new(&config)))
    }

    fn image(name: &str, scale: i32, frame_count: i32) -> Image {
        Image {
            url: format!("https://cdn.7tv.app/{name}"),
            mime: "image/webp".to_string(),
            size: 1,
            scale,
            width: scale * 32,
            frame_count,
        }
    }

    #[test]
    fn selects_distinct_source_preview_and_poster_variants() {
        let images = vec![
            image("1x.webp", 1, 198),
            image("2x.webp", 2, 198),
            image("4x.webp", 4, 198),
            image("2x_static.webp", 2, 1),
            image("4x_static.webp", 4, 1),
        ];

        assert_eq!(
            select_best_image(&images).unwrap().url,
            "https://cdn.7tv.app/4x.webp"
        );
        assert_eq!(
            select_animated_preview(&images).unwrap().url,
            "https://cdn.7tv.app/2x.webp"
        );
        assert_eq!(
            select_poster(&images).unwrap().url,
            "https://cdn.7tv.app/4x_static.webp"
        );
    }

    #[test]
    fn static_emote_has_no_animated_preview() {
        let images = vec![
            image("2x_static.webp", 2, 1),
            image("4x_static.webp", 4, 1),
        ];

        assert!(select_animated_preview(&images).is_none());
        assert_eq!(
            select_poster(&images).unwrap().url,
            "https://cdn.7tv.app/4x_static.webp"
        );
    }

    #[tokio::test]
    #[ignore = "requires the live 7TV API"]
    async fn live_user_sync_reads_every_page_of_the_set() {
        // xQc's channel set is several pages long.
        let set = live_service().fetch_user_emotes("01FE9DRF000009TR6M9N941CYW").await.unwrap();

        assert_eq!(set.display_name.as_deref(), Some("xQc"));
        assert!(set.emotes.len() > USER_SET_PAGE_SIZE as usize, "only {} emotes", set.emotes.len());
        assert!(set.emotes.iter().all(|e| e.flags.as_ref().is_some_and(|f| !f.default_zero_width)));
        let ids: HashSet<&str> = set.emotes.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), set.emotes.len());
        println!(
            "{} emotes from {:?}, {} zero-width skipped",
            set.emotes.len(),
            set.set_name,
            set.skipped_zero_width
        );
    }

    #[tokio::test]
    #[ignore = "requires the live 7TV API"]
    async fn live_search_serializes_source_preview_and_poster() {
        let service = live_service();

        let emotes = service.search_emotes("GIGACHAD", 1, 1, true).await.unwrap();
        let response = service.process_emotes_batch_no_storage(emotes);
        let first = response.first().unwrap();

        assert!(first.url.ends_with("/4x.webp"));
        assert!(first
            .animated_preview_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/2x.webp")));
        assert!(first
            .poster_url
            .as_deref()
            .is_some_and(|url| url.ends_with("/4x_static.webp")));
        println!("{}", serde_json::to_string_pretty(first).unwrap());
    }
}
