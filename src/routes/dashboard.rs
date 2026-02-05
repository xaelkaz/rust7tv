use axum::{
    response::Html,
};

pub async fn dashboard_handler() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}
