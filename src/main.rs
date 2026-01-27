mod config;
mod models;
mod routes;
mod services;

use crate::config::Config;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = Config::from_env();
    let port = cfg.port.parse::<u16>().unwrap_or(8000);
    
    let storage = Arc::new(services::storage::StorageService::new(&cfg));
    let cache = Arc::new(services::cache::CacheService::new(&cfg));
    let seventv = Arc::new(services::seventv::SevenTVService::new(Arc::clone(&storage)));

    let app_state = AppState {
        config: cfg,
        storage,
        cache,
        seventv,
    };

    let shared_state = Arc::new(app_state);

    let app = routes::create_router(shared_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Starting server on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

pub struct AppState {
    pub config: Config,
    pub storage: Arc<services::storage::StorageService>,
    pub cache: Arc<services::cache::CacheService>,
    pub seventv: Arc<services::seventv::SevenTVService>,
}
