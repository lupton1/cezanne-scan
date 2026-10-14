//! Background thread that owns the scanner, so the window never freezes during a scan.

use cezanne_core::connect::Connection;
use cezanne_core::image::{icc, tiff, Adjustments, RawImage};
use cezanne_core::protocol::WindowSettings;
use cezanne_core::scanner::Scanner;
use cezanne_core::scsi::sim::SimConfig;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

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
    Prescan(WindowSettings),
    Scan(Vec<FrameJob>),
    Probe,
}

pub enum Event {
    Connected(String),
    Disconnected,
    Progress { fraction: f32, label: String },
    Prescan(RawImage),
    Saved { name: String, path: PathBuf },
    Done(String),
    Error(String),
    Log(String),
}

pub struct Worker {
    pub tx: Sender<Request>,
    pub rx: Receiver<Event>,
    pub cancel: Arc<AtomicBool>,
}

impl Worker {
    pub fn spawn(ctx: eframe::egui::Context) -> Worker {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::Builder::new()
            .name("scanner".into())
            .spawn(move || run(req_rx, ev_tx, c2, ctx))
            .expect("spawn scanner thread");
        Worker { tx: req_tx, rx: ev_rx, cancel }
    }
}

fn run(rx: Receiver<Request>, tx: Sender<Event>, cancel: Arc<AtomicBool>, ctx: eframe::egui::Context) {
    let send = |e: Event| {
        let _ = tx.send(e);
        ctx.request_repaint();
    };
    let mut scanner: Option<Scanner> = None;
    while let Ok(req) = rx.recv() {
        cancel.store(false, Ordering::Relaxed);
        match req {
            Request::Connect(conn) => {
                scanner = None;
                let sim = SimConfig { line_delay: Duration::from_micros(400), ..SimConfig::default() };
                match conn.open_with(sim).and_then(Scanner::open) {
                    Ok(s) => {
                        send(Event::Connected(s.describe()));
                        scanner = Some(s);
                    }
                    Err(e) => send(Event::Error(format!("Connection failed: {e}"))),
                }
            }
            Request::Disconnect => {
                scanner = None;
                send(Event::Disconnected);
            }
            Request::Probe => match scanner.as_mut() {
                Some(s) => match s.probe() {
                    Ok(r) => send(Event::Log(r)),
                    Err(e) => send(Event::Error(e.to_string())),
                },
                None => send(Event::Error("Not connected".into())),
            },
            Request::Prescan(ws) => {
                let Some(s) = scanner.as_mut() else {
                    send(Event::Error("Not connected".into()));
                    continue;
                };
                let mut progress = |p: f32| send(Event::Progress { fraction: p, label: "Prescan".into() });
                match s.preview(&ws, &mut progress, &cancel) {
                    Ok(img) => {
                        send(Event::Prescan(img));
                        send(Event::Done("Prescan finished".into()));
                    }
                    Err(e) => send(Event::Error(format!("Prescan failed: {e}"))),
                }
            }
            Request::Scan(jobs) => {
                let Some(s) = scanner.as_mut() else {
                    send(Event::Error("Not connected".into()));
                    continue;
                };
                let n = jobs.len();
                let mut ok = 0;
                for (i, job) in jobs.into_iter().enumerate() {
                    let label = format!("Scanning {} ({} of {n})", job.name, i + 1);
                    let mut progress = |p: f32| send(Event::Progress { fraction: p, label: label.clone() });
                    let result = s.scan(&job.window, &mut progress, &cancel).and_then(|raw| {
                        let mut adj = job.adjustments.clone();
                        if job.auto_levels {
                            adj.auto_levels(&raw, 0.002);
                        }
                        let out = adj.apply(&raw);
                        let profile = if out.channels == 3 { icc::rgb_profile(2.2) } else { icc::gray_profile(2.2) };
                        if let Some(dir) = job.path.parent() {
                            std::fs::create_dir_all(dir)?;
                        }
                        tiff::write_tiff(&job.path, &out, job.depth, Some(&profile))?;
                        Ok(())
                    });
                    match result {
                        Ok(()) => {
                            ok += 1;
                            send(Event::Saved { name: job.name, path: job.path });
                        }
                        Err(e) => {
                            send(Event::Error(format!("{}: {e}", job.name)));
                            if cancel.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
                send(Event::Done(format!("Scanned {ok} of {n} frames")));
            }
        }
    }
}
