mod agent;
mod auth;
mod config;
mod crypto;
mod db;
mod error;
mod files;
mod routes;
mod state;
mod util;

use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{any, delete, get, post},
    Router,
};
use std::sync::Arc;
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nimbo=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = config::Config::from_env()?;
    std::fs::create_dir_all(cfg.vault_dir())?;
    std::fs::create_dir_all(cfg.files_dir())?;
    std::fs::create_dir_all(cfg.upload_tmp_dir())?;
    let secret = crypto::load_or_create_secret(&cfg.data_dir)?;
    let pool = db::init(&cfg.data_dir).await?;
    db::ensure_settings(&pool).await?;

    let state = Arc::new(state::AppState {
        pool,
        cfg: cfg.clone(),
        secret,
        login_failures: Default::default(),
    });

    if let Err(err) = routes::docs::reindex_all(&state).await {
        tracing::warn!(error = %err, "initial index failed; search may be incomplete");
    }
    files::worker::spawn(state.clone());

    let upload_limit = cfg.max_upload_bytes as usize + 1024 * 1024;
    let public = Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/api/bootstrap", get(routes::bootstrap::bootstrap))
        .route("/api/setup/status", get(routes::setup::status))
        .route("/api/setup", post(routes::setup::complete_setup))
        .route("/api/auth/login", post(routes::auth::login))
        .route("/api/public/shares/{token}", get(routes::public::share))
        .route(
            "/api/public/shares/{token}/nodes",
            get(routes::public::nodes),
        )
        .route(
            "/api/public/shares/{token}/files/{id}/download",
            get(routes::public::download),
        )
        .route(
            "/api/public/shares/{token}/files/{id}/content",
            get(routes::public::content),
        )
        .route(
            "/api/public/shares/{token}/files/{id}/thumbnail",
            get(routes::public::thumbnail),
        )
        .route(
            "/api/public/shares/{token}/download.zip",
            get(routes::public::download_zip),
        )
        .with_state(state.clone());

    let protected = Router::new()
        .route("/api/auth/logout", post(routes::auth::logout))
        .route("/api/auth/me", get(routes::auth::me))
        .route("/api/instance", get(routes::instance::instance))
        .route("/api/docs/tree", get(routes::docs::tree))
        .route(
            "/api/docs/content",
            get(routes::docs::get_content)
                .put(routes::docs::put_content)
                .delete(routes::docs::delete_content),
        )
        .route("/api/docs/rename", post(routes::docs::rename))
        .route("/api/docs/backlinks", get(routes::docs::backlinks))
        .route("/api/search", get(routes::docs::search))
        .route("/api/files", get(routes::files::list))
        .route("/api/files/folders", post(routes::files::create_folder))
        .route(
            "/api/files/upload",
            post(routes::files::upload).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/api/files/recent", get(routes::files::recent))
        .route("/api/files/search", get(routes::files::search))
        .route("/api/files/stats", get(routes::files::stats))
        .route(
            "/api/files/{id}",
            get(routes::files::get_file)
                .patch(routes::files::update_node)
                .delete(routes::files::delete_node),
        )
        .route("/api/files/{id}/content", get(routes::files::content))
        .route("/api/files/{id}/download", get(routes::files::download))
        .route("/api/files/{id}/thumbnail", get(routes::files::thumbnail))
        .route("/api/files/{id}/vision", post(routes::files::retry_vision))
        .route("/api/files/{id}/reindex", post(routes::files::retry_index))
        .route(
            "/api/shares",
            get(routes::shares::list_shares).post(routes::shares::create_share),
        )
        .route(
            "/api/shares/{id}",
            axum::routing::patch(routes::shares::update_share).delete(routes::shares::revoke_share),
        )
        .route(
            "/api/tokens",
            get(routes::tokens::list).post(routes::tokens::create),
        )
        .route("/api/tokens/{id}", delete(routes::tokens::revoke))
        .route(
            "/api/settings/llm",
            get(routes::settings::get_llm).put(routes::settings::put_llm),
        )
        .route("/api/settings/llm/test", post(routes::settings::test_llm))
        .route("/api/agent/chat", post(routes::agent::chat))
        .route("/api/agent/action", post(routes::agent::action))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ))
        .with_state(state.clone());

    let api = public
        .merge(protected)
        .route("/api/{*rest}", any(routes::health::api_not_found))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::origin_check,
        ));

    let mut app = api;
    if let Some(static_dir) = &cfg.static_dir {
        let index = static_dir.join("index.html");
        app = app.fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index)));
        tracing::info!(dir = %static_dir.display(), "serving static frontend");
    }

    let app = app
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new());

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, data = %cfg.data_dir.display(), "nimbo listening");
    axum::serve(listener, app).await?;
    Ok(())
}
