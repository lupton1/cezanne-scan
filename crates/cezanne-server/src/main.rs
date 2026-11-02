//! `cezanne-server`: runs on the computer the scanner is plugged into (for example a Mac
//! mini) and serves the Cezanne Scan interface as a web page. Any browser on the network can
//! prescan, set up frames and scan, and the finished TIFF files download to that browser's
//! computer.

mod api;
mod state;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use cezanne_core::connect::Connection;
use clap::Parser;
use state::{AppState, Config};
use std::path::PathBuf;
use std::sync::Arc;

pub const DEFAULT_PORT: u16 = 7820;

#[derive(Parser)]
#[command(version, about = "Serves the Cezanne Scan interface to browsers on your network")]
struct Args {
    /// Address and port to listen on. 0.0.0.0 accepts other computers on your network.
    #[arg(long, default_value_t = format!("0.0.0.0:{DEFAULT_PORT}"))]
    listen: String,

    /// Access token browsers must enter once. Recommended on a shared network.
    #[arg(long, env = "CEZANNE_SERVER_TOKEN")]
    token: Option<String>,

    /// Folder for finished scans on this computer, kept until removed from the web page.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Connect at start up: `sim`, `auto` (first scanner on this computer), a device
    /// address, or `bridge:HOST` for a cezanne-bridge on another computer.
    #[arg(long)]
    connect: Option<String>,

    /// Token for `--connect bridge:HOST`.
    #[arg(long, env = "CEZANNE_BRIDGE_TOKEN", default_value = "")]
    bridge_token: String,
}

fn default_output_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pictures")
        .join("Cezanne Scans")
}

fn parse_connection(s: &str, bridge_token: &str) -> Connection {
    match s.trim() {
        "sim" | "simulator" => Connection::Simulator,
        "auto" | "" => Connection::AutoLocal,
        other => match other.strip_prefix("bridge:") {
            Some(host) => Connection::Bridge { host: host.to_string(), token: bridge_token.to_string() },
            None => Connection::Local(other.to_string()),
        },
    }
}

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");
const ICON: &[u8] = include_bytes!("../../../packaging/icons/128x128.png");

fn page(content_type: &'static str, body: &'static [u8]) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

async fn require_token(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if api::authorised(&st, req.headers()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({ "error": "Access token needed" }))).into_response()
    }
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let a = Args::parse();
    let token = a.token.filter(|t| !t.trim().is_empty()).map(|t| t.trim().to_string());
    if token.is_none() && !a.listen.starts_with("127.") && !a.listen.starts_with("localhost") {
        log::warn!("no --token set: anyone on your network can use the scanner from a browser");
    }
    let output_dir = a.output_dir.unwrap_or_else(default_output_dir);
    if let Err(e) = std::fs::create_dir_all(&output_dir) {
        eprintln!("cannot create {}: {e}", output_dir.display());
        std::process::exit(1);
    }
    let start = a.connect.as_deref().map(|c| parse_connection(c, &a.bridge_token));
    let start_kind = start.as_ref().map(|c| match c {
        Connection::Simulator => "Simulator",
        Connection::AutoLocal | Connection::Local(_) => "Local",
        Connection::Bridge { .. } => "Bridge",
    });
    let st = AppState::new(Config { output_dir: output_dir.clone(), token, start_kind });
    if let Some(c) = start {
        let _ = st.send(state::Request::Connect(c));
    }

    let protected = Router::new()
        .route("/state", get(api::state))
        .route("/connect", post(api::connect))
        .route("/disconnect", post(api::disconnect))
        .route("/probe", post(api::probe))
        .route("/devices", get(api::devices))
        .route("/cancel", post(api::cancel))
        .route("/prescan", post(api::prescan))
        .route("/prescan.png", get(api::prescan_png))
        .route("/density", get(api::density))
        .route("/preview", post(api::preview))
        .route("/scan", post(api::scan))
        .route("/files/:id", get(api::download))
        .route("/files/:id", delete(api::delete_file))
        .route("/presets", get(api::presets).post(api::save_preset))
        .route_layer(middleware::from_fn_with_state(st.clone(), require_token));
    let api = Router::new()
        .route("/info", get(api::info))
        .route("/login", post(api::login))
        .route("/meta", get(api::meta))
        .merge(protected);

    let app = Router::new()
        .route("/", get(|| async { page("text/html; charset=utf-8", INDEX.as_bytes()) }))
        .route("/app.js", get(|| async { page("text/javascript; charset=utf-8", APP_JS.as_bytes()) }))
        .route("/style.css", get(|| async { page("text/css; charset=utf-8", STYLE.as_bytes()) }))
        .route("/icon.png", get(|| async { page("image/png", ICON) }))
        .nest("/api", api)
        .with_state(st);

    let listener = match tokio::net::TcpListener::bind(&a.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {}: {e}", a.listen);
            std::process::exit(1);
        }
    };
    log::info!("cezanne-server {} listening on http://{}", env!("CARGO_PKG_VERSION"), a.listen);
    log::info!("finished scans are kept in {}", output_dir.display());
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
