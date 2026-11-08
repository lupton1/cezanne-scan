//! HTTP handlers. Everything under `/api` is JSON except the images and downloads.

use crate::state::{frame_png, AppState, FrameJob, Request};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cezanne_core::connect::Connection;
use cezanne_core::image::tiff::BitDepth;
use cezanne_core::image::{Adjustments, FilmType};
use cezanne_core::preset::Preset;
use cezanne_core::protocol::{AreaUnit, ColorMode, ScanArea, WindowSettings};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub type St = State<Arc<AppState>>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, m.into())
}

fn busy(m: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, m.into())
}

type ApiResult<T> = Result<T, ApiError>;

// ------------------------------------------------------------------ access token

pub const COOKIE: &str = "cezanne_token";

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Percent encodes a token for the cookie, so `;`, spaces and non ASCII characters survive.
fn cookie_encode(t: &str) -> String {
    t.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn cookie_decode(v: &str) -> Vec<u8> {
    let b = v.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// True when the request carries the server's token (cookie or `X-Cezanne-Token` header),
/// or when the server has no token.
pub fn authorised(st: &AppState, headers: &HeaderMap) -> bool {
    let Some(token) = st.cfg.token.as_deref() else { return true };
    if let Some(h) = headers.get("x-cezanne-token") {
        if same(h.as_bytes(), token.as_bytes()) {
            return true;
        }
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .any(|(k, v)| k == COOKIE && same(&cookie_decode(v), token.as_bytes()))
}

#[derive(Deserialize)]
pub struct Login {
    token: String,
}

pub async fn login(State(st): St, Json(l): Json<Login>) -> Response {
    match st.cfg.token.as_deref() {
        Some(t) if !same(l.token.trim().as_bytes(), t.as_bytes()) => {
            ApiError(StatusCode::UNAUTHORIZED, "Wrong access token".into()).into_response()
        }
        _ => {
            let cookie = format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", cookie_encode(l.token.trim()));
            let mut r = Json(json!({ "ok": true })).into_response();
            if let Ok(v) = HeaderValue::from_str(&cookie) {
                r.headers_mut().insert(header::SET_COOKIE, v);
            }
            r
        }
    }
}

// ------------------------------------------------------------------ state

#[derive(Deserialize)]
pub struct StateQuery {
    #[serde(default)]
    log_after: u64,
}

pub async fn info(State(st): St, headers: HeaderMap) -> Json<serde_json::Value> {
    Json(json!({
        "name": "Cezanne Scan server",
        "version": env!("CARGO_PKG_VERSION"),
        "needs_token": st.cfg.token.is_some(),
        "authorised": authorised(&st, &headers),
    }))
}

pub async fn state(State(st): St, Query(q): Query<StateQuery>) -> Json<serde_json::Value> {
    let sh = st.shared();
    let log: Vec<_> = sh.log.iter().filter(|l| l.seq > q.log_after).cloned().collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "host": host_name(),
        "os": std::env::consts::OS,
        "connected": sh.connected,
        "busy": sh.busy,
        "task": sh.task,
        "progress": sh.progress,
        "prescan_seq": sh.prescan_seq,
        "prescan_bed_mm": sh.prescan.as_ref().map(|p| [p.bed_mm.0, p.bed_mm.1]),
        "bed_mm": { "TenthMm": st.bed_mm(AreaUnit::TenthMm), "Inch1200": st.bed_mm(AreaUnit::Inch1200) },
        "max_pixels_per_line": st.limits.max_pixels_per_line,
        "output_dir": st.cfg.output_dir.to_string_lossy(),
        "start_kind": st.cfg.start_kind,
        "files": sh.files,
        "log": log,
        "log_seq": sh.log_seq,
    }))
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_default()
}

/// Film types with their starting adjustments, and the colour modes, so the page uses the
/// same defaults as the desktop app.
pub async fn meta() -> Json<serde_json::Value> {
    let films: Vec<_> = FilmType::ALL
        .iter()
        .map(|f| {
            json!({
                "id": f,
                "label": f.label(),
                "negative": f.is_negative(),
                "original": f.original().label(),
                "reflective": f.original().is_reflective(),
                "adjustments": Adjustments::for_film(*f),
            })
        })
        .collect();
    let modes: Vec<_> = ColorMode::ALL
        .iter()
        .map(|m| json!({ "id": m, "label": m.label(), "channels": m.channels(), "bytes": m.bytes_per_sample() }))
        .collect();
    Json(json!({ "films": films, "modes": modes }))
}

// ------------------------------------------------------------------ connection

#[derive(Deserialize)]
#[serde(tag = "kind")]
pub enum ConnectBody {
    Simulator,
    Local {
        #[serde(default)]
        device: String,
    },
    Bridge {
        host: String,
        #[serde(default)]
        token: String,
    },
}

pub async fn connect(State(st): St, Json(b): Json<ConnectBody>) -> ApiResult<Json<serde_json::Value>> {
    if st.shared().busy {
        return Err(busy("Wait for the current scan to finish"));
    }
    let c = match b {
        ConnectBody::Simulator => Connection::Simulator,
        ConnectBody::Local { device } if device.trim().is_empty() => Connection::AutoLocal,
        ConnectBody::Local { device } => Connection::Local(device.trim().to_string()),
        ConnectBody::Bridge { host, token } => Connection::Bridge { host, token },
    };
    st.send(Request::Connect(c)).map_err(busy)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn disconnect(State(st): St) -> ApiResult<Json<serde_json::Value>> {
    if st.shared().busy {
        return Err(busy("Wait for the current scan to finish"));
    }
    st.send(Request::Disconnect).map_err(busy)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn probe(State(st): St) -> ApiResult<Json<serde_json::Value>> {
    if st.shared().busy {
        return Err(busy("Wait for the current scan to finish"));
    }
    st.send(Request::Probe).map_err(busy)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn devices() -> Json<serde_json::Value> {
    let r = tokio::task::spawn_blocking(cezanne_core::scsi::discover).await;
    match r {
        Ok(Ok(found)) => Json(json!({
            "devices": found.iter().map(|d| json!({
                "address": d.address, "vendor": d.vendor, "product": d.product, "revision": d.revision
            })).collect::<Vec<_>>()
        })),
        Ok(Err(e)) => Json(json!({ "devices": [], "error": e.to_string() })),
        Err(e) => Json(json!({ "devices": [], "error": e.to_string() })),
    }
}

pub async fn cancel(State(st): St) -> Json<serde_json::Value> {
    st.cancel.store(true, Ordering::Relaxed);
    st.log("Cancel requested", false);
    Json(json!({ "ok": true }))
}

// ------------------------------------------------------------------ prescan

/// Options from the Advanced section, which match SCREEN's driver by default.
#[derive(Deserialize, Clone, Copy)]
#[serde(default)]
pub struct Advanced {
    area_unit: AreaUnit,
    res_scale: u32,
}

impl Default for Advanced {
    fn default() -> Self {
        Advanced { area_unit: AreaUnit::TenthMm, res_scale: 10 }
    }
}

#[derive(Deserialize)]
pub struct PrescanBody {
    dpi: f64,
    film: FilmType,
    #[serde(default)]
    advanced: Advanced,
}

fn window(area: ScanArea, dpi: f64, mode: ColorMode, film: FilmType, adv: Advanced) -> WindowSettings {
    let mut ws = WindowSettings::new(area, dpi, mode);
    ws.original = film.original();
    ws.area_unit = adv.area_unit;
    ws.resolution_scale = adv.res_scale.clamp(1, 100);
    ws
}

pub async fn prescan(State(st): St, Json(b): Json<PrescanBody>) -> ApiResult<Json<serde_json::Value>> {
    if !(20.0..=300.0).contains(&b.dpi) {
        return Err(bad("Prescan resolution must be 20 to 300 dpi"));
    }
    let (bw, bh) = st.bed_mm(b.advanced.area_unit);
    let ws = window(ScanArea::new(0.0, 0.0, bw, bh), b.dpi, ColorMode::Rgb48, b.film, b.advanced);
    st.send(Request::Prescan(ws)).map_err(busy)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn prescan_png(State(st): St) -> Response {
    let p = st.shared().prescan.clone();
    match p {
        Some(p) => ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], p.png.clone()).into_response(),
        None => ApiError(StatusCode::NOT_FOUND, "No prescan yet".into()).into_response(),
    }
}

#[derive(Deserialize)]
pub struct DensityQuery {
    x: f64,
    y: f64,
}

pub async fn density(State(st): St, Query(q): Query<DensityQuery>) -> Json<serde_json::Value> {
    let p = st.shared().prescan.clone();
    Json(json!({ "density": p.and_then(|p| p.density_mm(q.x, q.y)) }))
}

#[derive(Deserialize)]
pub struct PreviewBody {
    area: ScanArea,
    adjustments: Adjustments,
    #[serde(default)]
    auto_levels: bool,
}

/// The processed preview of a frame. With `auto_levels` the HD and SD found are returned
/// in the `X-Adjustments` header so the page can show them.
pub async fn preview(State(st): St, Json(mut b): Json<PreviewBody>) -> ApiResult<Response> {
    let Some(p) = st.shared().prescan.clone() else {
        return Err(ApiError(StatusCode::NOT_FOUND, "No prescan yet".into()));
    };
    if b.area.width_mm <= 0.0 || b.area.height_mm <= 0.0 {
        return Err(bad("The frame is empty"));
    }
    let (png, adj) = tokio::task::spawn_blocking(move || {
        let png = frame_png(&p, &b.area, &mut b.adjustments, b.auto_levels);
        (png, b.adjustments)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut r = ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], png).into_response();
    if let Ok(v) = HeaderValue::from_str(&serde_json::to_string(&adj).unwrap_or_default()) {
        r.headers_mut().insert("x-adjustments", v);
    }
    Ok(r)
}

// ------------------------------------------------------------------ scanning

#[derive(Deserialize)]
pub struct FrameBody {
    name: String,
    area: ScanArea,
    adjustments: Adjustments,
    #[serde(default)]
    auto_levels: bool,
}

#[derive(Deserialize)]
pub struct ScanBody {
    frames: Vec<FrameBody>,
    mode: ColorMode,
    dpi: f64,
    #[serde(default = "yes")]
    sixteen_bit: bool,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    advanced: Advanced,
}

fn yes() -> bool {
    true
}

/// Keep file name prefixes to plain characters so a browser cannot write outside the folder.
fn clean_prefix(p: &str) -> String {
    let s: String = p.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | '.')).take(40).collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() {
        "scan_".into()
    } else {
        s
    }
}

pub async fn scan(State(st): St, Json(b): Json<ScanBody>) -> ApiResult<Json<serde_json::Value>> {
    if b.frames.is_empty() {
        return Err(bad("Draw a frame on the prescan first"));
    }
    if !(20.0..=12000.0).contains(&b.dpi) {
        return Err(bad("Resolution must be 20 to 12000 dpi"));
    }
    let prefix = clean_prefix(&b.prefix);
    let mut used = Vec::new();
    let mut jobs = Vec::new();
    for f in b.frames {
        let ws = window(f.area, b.dpi, b.mode, f.adjustments.film, b.advanced);
        let g = ws.geometry();
        if g.pixels_per_line > st.limits.max_pixels_per_line as u32 || g.lines > u16::MAX as u32 {
            return Err(bad(format!(
                "{} is too large at {} dpi ({} x {} pixels). Lower the resolution.",
                f.name, b.dpi, g.pixels_per_line, g.lines
            )));
        }
        jobs.push(FrameJob {
            name: f.name,
            window: ws,
            adjustments: f.adjustments,
            auto_levels: f.auto_levels,
            path: st.next_path(&prefix, &mut used),
            depth: if b.sixteen_bit { BitDepth::Sixteen } else { BitDepth::Eight },
        });
    }
    let n = jobs.len();
    st.send(Request::Scan(jobs)).map_err(busy)?;
    Ok(Json(json!({ "ok": true, "frames": n })))
}

// ------------------------------------------------------------------ files

pub async fn download(State(st): St, Path(id): Path<u64>) -> ApiResult<Response> {
    let f = st.shared().files.iter().find(|f| f.id == id).cloned();
    let Some(f) = f else { return Err(ApiError(StatusCode::NOT_FOUND, "No such file".into())) };
    let file = tokio::fs::File::open(&f.path)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", f.file_name)))?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024));
    let disposition = format!("attachment; filename=\"{}\"", f.file_name.replace('"', ""));
    Ok((
        [
            (header::CONTENT_TYPE, "image/tiff".to_string()),
            (header::CONTENT_LENGTH, f.bytes.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

pub async fn delete_file(State(st): St, Path(id): Path<u64>) -> ApiResult<Json<serde_json::Value>> {
    let f = {
        let mut sh = st.shared();
        let Some(i) = sh.files.iter().position(|f| f.id == id) else {
            return Err(ApiError(StatusCode::NOT_FOUND, "No such file".into()));
        };
        sh.files.remove(i)
    };
    match tokio::fs::remove_file(&f.path).await {
        Ok(()) => st.log(format!("Removed {} from the server", f.file_name), false),
        Err(e) => st.log(format!("Could not remove {}: {e}", f.file_name), true),
    }
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ presets

#[derive(Serialize)]
struct PresetOut {
    builtin: bool,
    #[serde(flatten)]
    preset: Preset,
}

pub async fn presets() -> Json<serde_json::Value> {
    let builtin = Preset::builtin();
    let all: Vec<PresetOut> =
        Preset::load_all().into_iter().map(|p| PresetOut { builtin: builtin.contains(&p), preset: p }).collect();
    Json(json!({ "presets": all }))
}

pub async fn save_preset(State(st): St, Json(p): Json<Preset>) -> ApiResult<Json<serde_json::Value>> {
    if p.name.trim().is_empty() {
        return Err(bad("Give the preset a name"));
    }
    let path = p.save().map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    st.log(format!("Saved preset {} to {}", p.name, path.display()), false);
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Config;

    fn server(token: Option<&str>) -> Arc<AppState> {
        AppState::new(Config { output_dir: std::env::temp_dir(), token: token.map(String::from), start_kind: None })
    }

    #[test]
    fn prefixes_stay_inside_the_folder() {
        assert_eq!(clean_prefix("../../evil"), "evil");
        assert_eq!(clean_prefix(r"..\x/y"), "xy");
        assert_eq!(clean_prefix("roll 12_"), "roll 12_");
        assert_eq!(clean_prefix("..."), "scan_");
        assert_eq!(clean_prefix(""), "scan_");
    }

    #[test]
    fn token_from_cookie_or_header() {
        let st = server(Some("sekrit"));
        let mut h = HeaderMap::new();
        assert!(!authorised(&st, &h));
        h.insert(header::COOKIE, HeaderValue::from_static("a=b; cezanne_token=wrong"));
        assert!(!authorised(&st, &h));
        h.insert(header::COOKIE, HeaderValue::from_static("a=b; cezanne_token=sekrit"));
        assert!(authorised(&st, &h));
        let mut h = HeaderMap::new();
        h.insert("x-cezanne-token", HeaderValue::from_static("sekrit"));
        assert!(authorised(&st, &h));
        assert!(authorised(&server(None), &HeaderMap::new()));
    }

    #[test]
    fn awkward_tokens_survive_the_cookie() {
        let token = "a;b c%20=\u{e9}";
        let st = server(Some(token));
        let mut h = HeaderMap::new();
        let cookie = format!("a=b; {COOKIE}={}", cookie_encode(token));
        h.insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        assert!(authorised(&st, &h));
        assert_eq!(cookie_encode("plain-Token_0.~"), "plain-Token_0.~");
    }
}
