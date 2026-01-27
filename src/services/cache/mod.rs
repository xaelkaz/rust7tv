use crate::config::Config;
use redis::AsyncCommands;
use serde::Serialize;

pub struct CacheService {
    client: redis::Client,
}

impl CacheService {
    pub fn new(cfg: &Config) -> Self {
        let client = if !cfg.redis_url.is_empty() {
            redis::Client::open(cfg.redis_url.clone()).expect("Failed to open redis client")
        } else {
            let addr = format!("redis://{}:{}", cfg.redis_host, cfg.redis_port);
            redis::Client::open(addr).expect("Failed to open redis client")
        };
        Self { client }
    }

    pub fn get_cache_key(query: &str, limit: i32, animated_only: bool) -> String {
        format!("emote_search:{}:{}:{}", query, limit, animated_only)
    }

    pub fn get_trending_cache_key(period: &str, limit: i32, page: i32, animated_only: bool) -> String {
        format!("trending:{}:{}:{}:{}", period, limit, page, animated_only)
    }

    pub async fn get_from_cache(&self, key: &str) -> Option<Vec<u8>> {
        let mut conn = self.client.get_multiplexed_tokio_connection().await.ok()?;
        conn.get(key).await.ok()
    }

    pub async fn save_to_cache<T: Serialize>(
        &self,
        key: &str,
        data: &T,
        ttl_seconds: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.client.get_multiplexed_tokio_connection().await?;
        let bytes = serde_json::to_vec(data)?;
        conn.set_ex::<_, _, ()>(key, bytes, ttl_seconds).await?;
        Ok(())
    }

    pub async fn clear_cache(&self, pattern: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.client.get_multiplexed_tokio_connection().await?;
        let keys: Vec<String> = conn.keys(pattern).await?;
        if !keys.is_empty() {
            conn.del::<_, ()>(keys).await?;
        }
        Ok(())
    }
}
