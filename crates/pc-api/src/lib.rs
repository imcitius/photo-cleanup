//! The web interface.
//!
//! Serving this from the machine that holds the photographs is the whole
//! point: the archive lives on a NAS, and moving several hundred gigabytes
//! across the network to look at it would be absurd. The browser receives
//! thumbnails measured in kilobytes, and full frames only when asked.

mod routes;
mod state;

pub use state::AppState;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(routes::index))
        .route("/static/{file}", get(routes::asset))
        .route("/api/status", get(routes::status))
        .route("/api/families", get(routes::families))
        .route("/api/families/{id}", get(routes::family))
        .route("/api/families/{id}/keeper", post(routes::set_keeper))
        .route("/api/series", get(routes::series))
        .route("/api/derived", get(routes::derived))
        .route("/api/plan", get(routes::plan))
        .route("/api/plan/apply", post(routes::apply_plan))
        .route("/api/quarantine", get(routes::quarantine))
        .route("/api/quarantine/{id}/undo", post(routes::undo))
        .route("/api/thumb/{key}", get(routes::thumb))
        .route("/api/file/{id}", get(routes::original))
        .with_state(state)
}

pub async fn serve(
    db_path: &Path,
    thumbs: &Path,
    quarantine: Option<std::path::PathBuf>,
    bind: SocketAddr,
) -> Result<()> {
    let state = Arc::new(AppState::new(db_path, thumbs, quarantine)?);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("не занять адрес {bind}"))?;
    let local = listener.local_addr()?;
    println!("Интерфейс: http://{local}");
    println!("Остановить: Ctrl+C");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nОстановлено.");
}
