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
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const AUTH_BODY_LIMIT: usize = 8 * 1024;
const DOC_BODY_LIMIT: usize = 16 * 1024 * 1024;
const AGENT_BODY_LIMIT: usize = DOC_BODY_LIMIT;
const SETTINGS_BODY_LIMIT: usize = 1024 * 1024;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
const RETENTION_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    // Hidden parser-subprocess entry point. This must run before the tokio
    // runtime is built: `#[tokio::main]` used to spawn a multi-thread runtime
    // (one worker thread per core, each reserving a 64 MiB malloc arena) before
    // this dispatch, pushing the child past the parser RLIMIT_AS ceiling so it
    // aborted or hung at startup. The child extracts one blob and exits; it can
    // never start the runtime, HTTP server, retention job, or index/vision
    // workers.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--extract-worker")) {
        files::worker::run_extract_worker_cli()?;
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> anyhow::Result<()> {
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
    if let Some(static_dir) = &cfg.static_dir {
        if !static_dir.is_dir() {
            anyhow::bail!(
                "NIMBO_STATIC_DIR {:?} does not exist or is not a directory",
                static_dir
            );
        }
    }
    if cfg.non_loopback_plain_http() {
        tracing::warn!(
            bind = %cfg.bind,
            "listening beyond loopback without secure cookies; terminate TLS in a trusted proxy \
             before exposing credentials"
        );
    }

    let secret = crypto::load_or_create_secret(&cfg.data_dir)?;
    let pool = db::init(&cfg.data_dir).await?;
    db::ensure_settings(&pool).await?;
    if let Err(err) = db::retention_cleanup(&pool, cfg.audit_retention_days).await {
        tracing::warn!(error = %err, "startup retention cleanup failed");
    }

    let dummy_password_hash =
        auth::hash_password(&uuid::Uuid::new_v4().to_string()).map_err(|err| {
            anyhow::anyhow!("could not initialise password verifier: {}", err.message)
        })?;

    let state = Arc::new(state::AppState {
        pool: pool.clone(),
        cfg: cfg.clone(),
        secret,
        login_limiter: Default::default(),
        password_semaphore: Arc::new(tokio::sync::Semaphore::new(auth::PASSWORD_HASH_CONCURRENCY)),
        dummy_password_hash,
    });

    if let Err(err) = routes::docs::reindex_all(&state).await {
        tracing::warn!(error = %err, "initial index failed; search may be incomplete");
    }
    files::worker::spawn(state.clone());

    let retention_pool = pool.clone();
    let audit_retention_days = cfg.audit_retention_days;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RETENTION_INTERVAL).await;
            if let Err(err) = db::retention_cleanup(&retention_pool, audit_retention_days).await {
                tracing::warn!(error = %err, "periodic retention cleanup failed");
            }
        }
    });

    let upload_limit = cfg.upload_request_limit()?;

    let public = Router::new()
        .route("/livez", get(routes::health::livez))
        .route("/healthz", get(routes::health::healthz))
        .route("/api/bootstrap", get(routes::bootstrap::bootstrap))
        .route("/api/setup/status", get(routes::setup::status))
        .route(
            "/api/setup",
            post(routes::setup::complete_setup).layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)),
        )
        .route(
            "/api/auth/login",
            post(routes::auth::login).layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)),
        )
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
                .delete(routes::docs::delete_content)
                .layer(DefaultBodyLimit::max(DOC_BODY_LIMIT)),
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
        .route("/api/files/media", get(routes::files::media))
        .route("/api/files/search", get(routes::files::search))
        .route("/api/files/stats", get(routes::files::stats))
        .route(
            "/api/files/{id}",
            get(routes::files::get_file)
                .patch(routes::files::update_node)
                .delete(routes::files::delete_node),
        )
        .route(
            "/api/files/{id}/content",
            get(routes::files::content)
                .put(routes::files::replace_content)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
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
            get(routes::tokens::list)
                .post(routes::tokens::create)
                .layer(DefaultBodyLimit::max(AUTH_BODY_LIMIT)),
        )
        .route("/api/tokens/{id}", delete(routes::tokens::revoke))
        .route(
            "/api/settings/llm",
            get(routes::settings::get_llm)
                .put(routes::settings::put_llm)
                .layer(DefaultBodyLimit::max(SETTINGS_BODY_LIMIT)),
        )
        .route(
            "/api/settings/llm/models",
            post(routes::settings::list_llm_models)
                .layer(DefaultBodyLimit::max(SETTINGS_BODY_LIMIT)),
        )
        .route(
            "/api/settings/llm/test",
            post(routes::settings::test_llm).layer(DefaultBodyLimit::max(SETTINGS_BODY_LIMIT)),
        )
        .route(
            "/api/agent/chat",
            post(routes::agent::chat).layer(DefaultBodyLimit::max(AGENT_BODY_LIMIT)),
        )
        .route(
            "/api/agent/action",
            post(routes::agent::action).layer(DefaultBodyLimit::max(AGENT_BODY_LIMIT)),
        )
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
        ))
        .layer(middleware::from_fn(auth::json_error_shape));

    let mut app = api;
    if let Some(static_dir) = &cfg.static_dir {
        let index = static_dir.join("index.html");
        app = app.fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(index)));
        tracing::info!(dir = %static_dir.display(), "serving static frontend");
    }

    let app = app
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn(auth::security_headers));

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(addr = %cfg.bind, data = %cfg.data_dir.display(), "nimbo listening");
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());
    if let Err(err) = server.await {
        pool.close().await;
        return Err(err.into());
    }
    pool.close().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::warn!(error = %err, "could not install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received; draining requests");

    // Bound the drain from the moment the signal arrives. This timer must not
    // wrap the whole server future, or the process exits while still serving.
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_GRACE).await;
        tracing::warn!("graceful shutdown exceeded its drain budget; exiting");
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    /// The parser subprocess shares the server binary; the hidden dispatch must
    /// stay ahead of the tokio runtime build, tracing, config, and state
    /// initialization so a child can never spawn the runtime's per-core worker
    /// threads, acquire the database, bind the listener, or spawn workers.
    #[test]
    fn extract_worker_dispatch_precedes_server_initialization() {
        let source = include_str!("main.rs");
        let dispatch = source
            .find("--extract-worker")
            .expect("extract worker dispatch is missing");
        let runtime = source
            .find("new_multi_thread")
            .expect("runtime construction is missing");
        let tracing = source
            .find("tracing_subscriber::registry")
            .expect("tracing initialization is missing");
        let state = source
            .find("state::AppState {")
            .expect("state initialization is missing");
        assert!(
            dispatch < runtime,
            "extract worker dispatch must precede tokio runtime construction"
        );
        assert!(
            dispatch < tracing,
            "extract worker dispatch must precede tracing initialization"
        );
        assert!(
            dispatch < state,
            "extract worker dispatch must precede state/database initialization"
        );
    }
}
