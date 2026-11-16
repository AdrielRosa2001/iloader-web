//! Headless iloader: serves the web UI and an HTTP/WebSocket API on top of
//! `iloader-core`. Configuration is done through environment variables; see
//! README-SERVER.md.

mod api;
mod auth;
mod config;
mod events;
mod logging;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    http::{HeaderName, HeaderValue, header},
    middleware,
    routing::{get, post},
};
use iloader_core::{AppConfig, AppContext, EventSink};
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};
use tracing_subscriber::{
    Layer, Registry,
    filter::{LevelFilter, Targets},
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::{auth::Auth, config::ServerConfig, events::BroadcastSink};

pub struct AppState {
    pub config: ServerConfig,
    pub core: Arc<AppContext>,
    pub sink: Arc<BroadcastSink>,
    pub auth: Auth,
    /// Held while a command that uses the Apple account runs (see api::EXCLUSIVE).
    pub op_lock: Arc<tokio::sync::Mutex<()>>,
    pub uploads_dir: PathBuf,
}

impl AppState {
    pub fn new(config: ServerConfig, sink: Arc<BroadcastSink>) -> Result<Self, String> {
        let core = AppContext::new(
            AppConfig {
                data_dir: config.data_dir.clone(),
                temp_dir: config.temp_dir.clone(),
                credential_key: config.credential_key,
            },
            sink.clone() as Arc<dyn EventSink>,
        )
        .map_err(|e| e.to_string())?;
        let uploads_dir = config.temp_dir.join("uploads");
        iloader_core::storage::create_private_dir(&uploads_dir).map_err(|e| e.to_string())?;
        Ok(Self {
            auth: Auth::new(&config.token),
            core: Arc::new(core),
            sink,
            op_lock: Arc::new(tokio::sync::Mutex::new(())),
            uploads_dir,
            config,
        })
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/api/auth/session", get(auth::session))
        .route("/api/invoke/{command}", post(api::invoke))
        .route("/api/emit/{event}", post(api::emit))
        .route("/api/upload", post(api::upload))
        .route("/api/pairing/export", get(api::export_pairing))
        .route("/api/events", get(events::ws_handler))
        .route("/api/version", get(api::version))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let public_api = Router::new()
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout));

    let index = state.config.static_dir.join("index.html");
    let static_files = ServeDir::new(&state.config.static_dir).fallback(ServeFile::new(index));

    Router::new()
        .merge(protected)
        .merge(public_api)
        .fallback_service(static_files)
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data:; \
                 style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; \
                 form-action 'self'",
            ),
        ))
        .with_state(state)
}

fn init_logging(config: &ServerConfig, sink: Arc<BroadcastSink>) {
    let log_dir = config.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();

    // dependencies of the HTTP stack are noisy at DEBUG
    let quiet = |default: LevelFilter| {
        Targets::new()
            .with_default(default)
            .with_target("hyper", LevelFilter::WARN)
            .with_target("hyper_util", LevelFilter::WARN)
            .with_target("h2", LevelFilter::WARN)
            .with_target("tower", LevelFilter::INFO)
            .with_target("tower_http", LevelFilter::INFO)
            .with_target("axum", LevelFilter::INFO)
            .with_target("tungstenite", LevelFilter::WARN)
            .with_target("tokio_tungstenite", LevelFilter::WARN)
            .with_target("rustls", LevelFilter::WARN)
            .with_target("reqwest", LevelFilter::INFO)
    };

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("iloader")
        .filename_suffix("log")
        .max_log_files(2)
        .build(&log_dir)
        .expect("failed to create log file appender");

    let file_layer = fmt::layer()
        .with_writer(file_appender)
        .with_target(true)
        .with_ansi(false)
        .with_filter(quiet(LevelFilter::DEBUG));

    let stdout_layer = fmt::layer()
        .with_target(true)
        .with_filter(quiet(LevelFilter::INFO));

    let ui_layer = logging::FrontendLoggingLayer::new(sink as Arc<dyn EventSink>)
        .with_filter(quiet(LevelFilter::from_level(config.ui_log_level)));

    Registry::default()
        .with(file_layer)
        .with(stdout_layer)
        .with(ui_layer)
        .init();

    std::panic::set_hook(Box::new(|panic_info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let location = panic_info
            .location()
            .map(|loc| format!("{}:{}", loc.file(), loc.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let backtrace = std::backtrace::Backtrace::capture();
        tracing::error!(
            target: "panic",
            thread = thread_name,
            location = location,
            message = message,
            backtrace = %backtrace,
            "panic captured"
        );
    }));
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("Shutting down");
}

#[tokio::main]
async fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    isideload::init().expect("Failed to initialize error reporting");

    let config = match ServerConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("iloader-server: configuration error: {e}");
            std::process::exit(2);
        }
    };

    let sink = Arc::new(BroadcastSink::new());
    init_logging(&config, sink.clone());

    let state = match AppState::new(config.clone(), sink) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!("Failed to initialise: {e}");
            std::process::exit(1);
        }
    };
    // leftovers from a previous run
    api::cleanup_uploads(&state.uploads_dir, std::time::Duration::ZERO).await;

    if !config.static_dir.join("index.html").is_file() {
        tracing::warn!(
            "No web UI found at {} (set ILOADER_STATIC_DIR or run `npm run build`)",
            config.static_dir.display()
        );
    }
    // inside a container the port is published (and restricted) by Docker
    let in_container = std::env::var_os("ILOADER_IN_CONTAINER").is_some();
    if !config.bind.ip().is_loopback() && !in_container {
        tracing::warn!(
            "Listening on {}, reachable from the network. Make sure it is behind TLS \
             (reverse proxy) or only reachable through an SSH tunnel.",
            config.bind
        );
    }
    tracing::info!(
        "Data dir: {} | credential saving: {}",
        config.data_dir.display(),
        if config.credential_key.is_some() { "available" } else { "disabled" }
    );

    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind {}: {e}", config.bind);
            std::process::exit(1);
        }
    };
    tracing::info!("iloader-server listening on http://{}", config.bind);

    let app = build_router(state);
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        tracing::error!("Server error: {e}");
    }
}

#[cfg(test)]
mod tests;
