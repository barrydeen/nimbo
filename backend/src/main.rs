mod agent;
mod auth;
mod config;
mod crypto;
mod db;
mod error;
mod routes;
mod state;
mod util;

use axum::{
    middleware,
    routing::{any, get, post},
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

    let public = Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/api/bootstrap", get(routes::bootstrap::bootstrap))
        .route("/api/setup/status", get(routes::setup::status))
        .route("/api/setup", post(routes::setup::complete_setup))
        .route("/api/auth/login", post(routes::auth::login))
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
