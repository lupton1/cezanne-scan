//! Server state and the thread that owns the scanner.
//!
//! Only one thing talks to the scanner at a time: the scanner thread takes requests from a
//! channel, and the web handlers read progress, the log, the prescan and the finished files
//! from [`Shared`].

use cezanne_core::connect::Connection;
use cezanne_core::image::{icc, png, tiff, Adjustments, RawImage};
use cezanne_core::protocol::window::ModelLimits;
use cezanne_core::protocol::{AreaUnit, WindowSettings};
use cezanne_core::scanner::Scanner;
use cezanne_core::scsi::sim::SimConfig;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LOG_LINES: usize = 500;

pub struct Config {
    /// Where finished scans are written on this computer before the browser downloads them.
    pub output_dir: PathBuf,
    /// Shared secret browsers must give before they can use the scanner.
    pub token: Option<String>,
    /// Connection kind given with `--connect`, offered first to new browsers.
    pub start_kind: Option<&'static str>,
}

pub struct FrameJob {
    pub name: String,
    pub window: WindowSettings,
    pub adjustments: Adjustments,
    pub auto_levels: bool,
    pub path: PathBuf,
    pub depth: tiff::BitDepth,
}

pub enum Request {
    Connect(Connection),
    Disconnect,
    Probe,
    Prescan(WindowSettings),
    Scan(Vec<FrameJob>),
}

/// The last prescan of the whole bed.
pub struct Prescan {
    pub raw: RawImage,
    /// Display version for the browser, gamma 2.2.
    pub png: Vec<u8>,
    pub bed_mm: (f64, f64),
}

impl Prescan {
    /// Raw prescan pixels inside an area given in millimetres.
    pub fn crop_mm(&self, x: f64, y: f64, w: f64, h: f64) -> RawImage {
        let (bw, bh) = self.bed_mm;
        let sx = self.raw.width as f64 / bw;
        let sy = self.raw.height as f64 / bh;
        let px = ((x * sx).max(0.0) as u32).min(self.raw.width - 1);
        let py = ((y * sy).max(0.0) as u32).min(self.raw.height - 1);
        let pw = ((w * sx).max(1.0) as u32).min(self.raw.width - px);
        let ph = ((h * sy).max(1.0) as u32).min(self.raw.height - py);
        self.raw.crop(px, py, pw, ph)
    }

    /// Density per channel around a point on the bed, 3 by 3 prescan pixels.
    pub fn density_mm(&self, x: f64, y: f64) -> Option<Vec<f64>> {
        let (bw, bh) = self.bed_mm;
        let px = (x / bw * self.raw.width as f64).floor();
        let py = (y / bh * self.raw.height as f64).floor();
        if px < 0.0 || py < 0.0 || px >= self.raw.width as f64 || py >= self.raw.height as f64 {
            return None;
        }
        let (px, py) = ((px as u32).saturating_sub(1), (py as u32).saturating_sub(1));
        let w = 3.min(self.raw.width - px);
        let h = 3.min(self.raw.height - py);
        Some(self.raw.density_at(px, py, w, h))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanFile {
    pub id: u64,
    pub file_name: String,
    pub frame: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub channels: usize,
    pub bits: u8,
    /// Seconds since 1970.
    pub created: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub text: String,
    pub error: bool,
}

#[derive(Default)]
pub struct Shared {
    pub connected: Option<String>,
    pub busy: bool,
    pub task: String,
    pub progress: f32,
    pub log: VecDeque<LogLine>,
    pub log_seq: u64,
    pub prescan: Option<Arc<Prescan>>,
    pub prescan_seq: u64,
    pub files: Vec<ScanFile>,
    pub file_seq: u64,
}

impl Shared {
    pub fn log(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            log::warn!("{text}");
        } else {
            log::info!("{text}");
        }
        self.log_seq += 1;
        self.log.push_back(LogLine { seq: self.log_seq, text, error });
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    pub shared: Mutex<Shared>,
    pub tx: Mutex<Sender<Request>>,
    pub cancel: Arc<AtomicBool>,
    pub limits: ModelLimits,
}

impl AppState {
    pub fn new(cfg: Config) -> Arc<AppState> {
        let (tx, rx) = std::sync::mpsc::channel();
        let state = Arc::new(AppState {
            cfg,
            shared: Mutex::new(Shared::default()),
            tx: Mutex::new(tx),
            cancel: Arc::new(AtomicBool::new(false)),
            limits: ModelLimits::FT_S5000,
        });
        let s2 = state.clone();
        std::thread::Builder::new()
            .name("scanner".into())
            .spawn(move || run(rx, s2))
            .expect("spawn scanner thread");
        state
    }

    pub fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn log(&self, text: impl Into<String>, error: bool) {
        self.shared().log(text, error);
    }

    /// Hand a request to the scanner thread. Prescans and scans mark the server busy so a
    /// second browser cannot start another one at the same time.
    pub fn send(&self, r: Request) -> Result<(), String> {
        {
            let mut sh = self.shared();
            let long = matches!(r, Request::Prescan(_) | Request::Scan(_));
            if long {
                if sh.busy {
                    return Err(format!("The scanner is busy ({})", sh.task));
                }
                if sh.connected.is_none() {
                    return Err("Not connected to a scanner".into());
                }
                sh.busy = true;
                sh.progress = 0.0;
                sh.task = if matches!(r, Request::Prescan(_)) { "Prescan".into() } else { "Scan".into() };
            }
        }
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).send(r).map_err(|_| "scanner thread stopped".to_string())
    }

    pub fn bed_mm(&self, unit: AreaUnit) -> (f64, f64) {
        self.limits.bed_mm(unit)
    }

    /// Next free `prefixNNNN.tif` in the output folder.
    pub fn next_path(&self, prefix: &str, used: &mut Vec<PathBuf>) -> PathBuf {
        let dir = &self.cfg.output_dir;
        for n in 1..1_000_000 {
            let p = dir.join(format!("{prefix}{n:04}.tif"));
            if !p.exists() && !used.contains(&p) {
                used.push(p.clone());
                return p;
            }
        }
        dir.join(format!("{prefix}overflow.tif"))
    }
}

fn run(rx: Receiver<Request>, st: Arc<AppState>) {
    let mut scanner: Option<Scanner> = None;
    while let Ok(req) = rx.recv() {
        st.cancel.store(false, Ordering::Relaxed);
        match req {
            Request::Connect(conn) => {
                scanner = None;
                st.shared().connected = None;
                st.log(format!("Connecting to {}", conn.label()), false);
                let sim = SimConfig { line_delay: Duration::from_micros(400), ..SimConfig::default() };
                match conn.open_with(sim).and_then(Scanner::open) {
                    Ok(s) => {
                        let d = s.describe();
                        let mut sh = st.shared();
                        sh.log(format!("Connected: {d}"), false);
                        sh.connected = Some(d);
                        scanner = Some(s);
                    }
                    Err(e) => st.log(format!("Connection failed: {e}"), true),
                }
            }
            Request::Disconnect => {
                scanner = None;
                let mut sh = st.shared();
                sh.connected = None;
                sh.log("Disconnected", false);
            }
            Request::Probe => match scanner.as_mut() {
                Some(s) => match s.probe() {
                    Ok(r) => st.log(r, false),
                    Err(e) => st.log(format!("Probe failed: {e}"), true),
                },
                None => st.log("Not connected", true),
            },
            Request::Prescan(ws) => {
                let result = match scanner.as_mut() {
                    Some(s) => {
                        let mut progress = |p: f32| st.shared().progress = p;
                        s.preview(&ws, &mut progress, &st.cancel)
                    }
                    None => Err(cezanne_core::Error::InvalidParameter("not connected".into())),
                };
                let mut sh = st.shared();
                match result {
                    Ok(raw) => {
                        let png = display_png(&raw);
                        let bed_mm = (ws.area.width_mm, ws.area.height_mm);
                        sh.prescan = Some(Arc::new(Prescan { raw, png, bed_mm }));
                        sh.prescan_seq += 1;
                        sh.log("Prescan finished", false);
                    }
                    Err(e) => sh.log(format!("Prescan failed: {e}"), true),
                }
                sh.busy = false;
            }
            Request::Scan(jobs) => {
                let n = jobs.len();
                let mut ok = 0;
                for (i, job) in jobs.into_iter().enumerate() {
                    let Some(s) = scanner.as_mut() else {
                        st.log("Not connected", true);
                        break;
                    };
                    st.shared().task = format!("Scanning {} ({} of {n})", job.name, i + 1);
                    match scan_one(s, &job, &st) {
                        Ok(file) => {
                            ok += 1;
                            let mut sh = st.shared();
                            sh.log(format!("Saved {} as {}", job.name, file.file_name), false);
                            sh.files.push(file);
                        }
                        Err(e) => {
                            st.log(format!("{}: {e}", job.name), true);
                            if st.cancel.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
                let mut sh = st.shared();
                sh.log(format!("Scanned {ok} of {n} frames"), false);
                sh.busy = false;
            }
        }
    }
}

fn scan_one(s: &mut Scanner, job: &FrameJob, st: &AppState) -> cezanne_core::Result<ScanFile> {
    let mut progress = |p: f32| st.shared().progress = p;
    let raw = s.scan(&job.window, &mut progress, &st.cancel)?;
    let mut adj = job.adjustments.clone();
    if job.auto_levels {
        adj.auto_levels(&raw, 0.002);
    }
    let out = adj.apply(&raw);
    let profile = icc::output_profile(out.channels);
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    tiff::write_tiff(&job.path, &out, job.depth, Some(&profile))?;
    let bytes = std::fs::metadata(&job.path).map(|m| m.len()).unwrap_or(0);
    let mut sh = st.shared();
    sh.file_seq += 1;
    Ok(ScanFile {
        id: sh.file_seq,
        file_name: job.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
        frame: job.name.clone(),
        path: job.path.clone(),
        bytes,
        width: out.width,
        height: out.height,
        channels: out.channels,
        bits: if job.depth == tiff::BitDepth::Sixteen { 16 } else { 8 },
        created: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// Linear scanner data to an sRGB-ish PNG, as in the desktop app's bed view.
fn display_png(img: &RawImage) -> Vec<u8> {
    let lut: Vec<u8> = (0..4096).map(|i| ((i as f64 / 4095.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
    let mut rgba = Vec::with_capacity((img.width * img.height) as usize * 4);
    for px in img.data.chunks_exact(img.channels) {
        let g = |k: usize| lut[(px[k.min(img.channels - 1)] >> 4) as usize];
        rgba.extend_from_slice(&[g(0), g(1), g(2), 255]);
    }
    png::encode_rgba(img.width, img.height, &rgba)
}

/// The processed preview of one frame as a PNG.
pub fn frame_png(prescan: &Prescan, area: &cezanne_core::protocol::ScanArea, adj: &mut Adjustments, auto_levels: bool) -> Vec<u8> {
    let raw = prescan.crop_mm(area.x_mm, area.y_mm, area.width_mm, area.height_mm);
    if auto_levels {
        adj.auto_levels(&raw, 0.002);
    }
    let out = adj.apply(&raw);
    png::encode_rgba(out.width, out.height, &out.to_rgba8())
}
