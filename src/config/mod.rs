use serde::Deserialize;
use std::env;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub port: String,
    pub redis_host: String,
    pub redis_port: String,
    pub redis_password: String,
    pub redis_db: i32,
    pub redis_url: String,
    pub azure_conn_str: String,
    pub container_name: String,
    pub cache_ttl: u64,
    pub trending_cache_ttl: u64,
    pub api_title: String,
    pub api_description: String,
    pub api_version: String,
    pub database_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();

        Self {
            port: env::var("PORT").unwrap_or_else(|_| "8000".to_string()),
            redis_host: env::var("REDIS_HOST").unwrap_or_else(|_| "localhost".to_string()),
            redis_port: env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string()),
            redis_password: env::var("REDIS_PASSWORD").unwrap_or_default(),
            redis_db: env::var("REDIS_DB")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0),
            redis_url: env::var("REDIS_URL").unwrap_or_default(),
            azure_conn_str: env::var("AZURE_CONNECTION_STRING").unwrap_or_default(),
            container_name: env::var("CONTAINER_NAME").unwrap_or_else(|_| "emotes".to_string()),
            cache_ttl: env::var("CACHE_TTL")
                .unwrap_or_else(|_| "3600".to_string())
                .parse()
                .unwrap_or(3600),
            trending_cache_ttl: env::var("TRENDING_CACHE_TTL")
                .unwrap_or_else(|_| "900".to_string())
                .parse()
                .unwrap_or(900),
            api_title: env::var("API_TITLE").unwrap_or_else(|_| "7TV Emote API".to_string()),
            api_description: env::var("API_DESCRIPTION")
                .unwrap_or_else(|_| "API for fetching and storing 7TV emotes".to_string()),
            api_version: env::var("API_VERSION").unwrap_or_else(|_| "1.0.0".to_string()),
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
        }
    }
}
