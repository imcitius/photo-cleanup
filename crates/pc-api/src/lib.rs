//! The web interface.
//!
//! Serving this from the machine that holds the photographs is the whole
//! point: the archive lives on a NAS, and moving several hundred gigabytes
//! across the network to look at it would be absurd. The browser receives
//! thumbnails measured in kilobytes, and full frames only when asked.

mod jobs;
mod review;
mod routes;
mod service;
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
        .route("/api/jobs", get(jobs::list).post(jobs::start))
        .route("/api/jobs/{id}", get(jobs::detail))
        .route("/api/jobs/{id}/cancel", post(jobs::cancel))
        .route("/api/jobs/{id}/events", get(jobs::events))
        .route("/api/preview", post(service::preview))
        .route("/api/fs", get(service::fs))
        .route("/api/tree", get(service::tree))
        .route("/api/tree/files", get(service::tree_files))
        .route("/api/originals", post(service::set_original_folder))
        .route("/api/catalogs", get(service::catalogs))
        .route("/api/journal", get(service::journal))
        .route("/api/runs", get(service::runs))
        .route(
            "/api/settings",
            get(service::settings).put(service::save_settings),
        )
        .route("/api/reset", post(service::reset))
        .route("/api/recent", get(service::recent))
        .route("/api/families/{id}/split", post(service::split_family))
        .route("/api/families/{id}/keep-only", post(service::keep_only))
        .route(
            "/api/families/{id}/keep-all-versions",
            post(service::keep_all_versions),
        )
        .route("/api/keepers/prefer-folder", post(service::prefer_folder))
        .route(
            "/api/keepers/keep-folder-only",
            post(service::keep_folder_only),
        )
        .route("/api/series/{id}/best", post(service::best))
        .route("/api/series/{id}/reject-rest", post(service::reject_rest))
        .route("/api/series/{id}/keep-all", post(service::keep_all))
        .route("/api/files/{id}/reject", post(service::reject))
        .route("/api/files/{id}/category", post(service::category))
        .route("/api/files/{id}/date", post(service::date))
        .route("/api/status", get(routes::status))
        .route("/api/review", get(review::queue))
        .route("/api/review/decisions", get(review::decisions))
        .route("/api/review/{id}", post(review::decide))
        .route("/api/review/batch-preview", post(review::batch_preview))
        .route("/api/review/batch", post(review::batch))
        .route("/api/review/undo", post(review::undo))
        .route("/api/families", get(routes::families))
        .route("/api/families/{id}", get(routes::family))
        .route("/api/families/{id}/keeper", post(routes::set_keeper))
        .route("/api/series", get(routes::series))
        .route("/api/categories", get(routes::categories))
        .route("/api/derived", get(routes::derived))
        .route("/api/organize", get(routes::organize))
        .route("/api/plan", get(routes::plan))
        .route("/api/plan/apply", post(routes::apply_plan))
        .route("/api/quarantine", get(routes::quarantine))
        .route("/api/quarantine/{id}/undo", post(routes::undo))
        .route("/api/quarantine/orphans", get(service::quarantine_orphans))
        .route("/api/thumb/{key}", get(routes::thumb))
        .route("/api/file/{id}", get(routes::original))
        .route("/api/file/{id}/preview", get(routes::full_preview))
        .route("/api/file/{id}/details", get(service::file_details))
        .with_state(state)
}

/// Show the interface in the operator's browser.
///
/// Called once the socket is listening, so the first page load does not race
/// the server. A failure here is not worth reporting: the address is printed
/// on the line above either way.
fn open_in_browser(url: &str) {
    let (cmd, args): (&str, &[&str]) = if cfg!(windows) {
        ("cmd", &["/C", "start", ""])
    } else if cfg!(target_vendor = "apple") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let _ = std::process::Command::new(cmd).args(args).arg(url).spawn();
}

pub async fn serve(
    db_path: &Path,
    thumbs: &Path,
    quarantine: Option<std::path::PathBuf>,
    bind: SocketAddr,
    open: bool,
) -> Result<()> {
    let mut state = AppState::new(db_path, thumbs, quarantine)?;
    {
        // Before the first request, so an error during start-up is already in
        // the language the operator chose.
        let db = state.db.lock().unwrap();
        let settings = crate::service::settings_value(&state, &db)?;
        crate::service::apply_language(&settings);
    }
    state.network = !bind.ip().is_loopback();
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| pc_core::tf!("не занять адрес {0}", "cannot bind {0}", bind))?;
    let local = listener.local_addr()?;
    if open {
        open_in_browser(&format!("http://{local}"));
    }
    println!(
        "{}",
        pc_core::tf!("Интерфейс: http://{0}", "Interface: http://{0}", local)
    );
    println!("{}", pc_core::tr!("Остановить: Ctrl+C", "Stop with Ctrl+C"));
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\n{}", pc_core::tr!("Остановлено.", "Stopped."));
}

#[cfg(test)]
mod tests;
