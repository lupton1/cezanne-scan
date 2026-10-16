use crate::curve_editor::curve_editor;
use crate::worker::{Event, FrameJob, Request, Worker};
use cezanne_core::connect::Connection;
use cezanne_core::image::pipeline::GrayMode;
use cezanne_core::image::tiff::BitDepth;
use cezanne_core::image::{Adjustments, Curve, FilmType, RawImage};
use cezanne_core::preset::Preset;
use cezanne_core::protocol::window::ModelLimits;
use cezanne_core::protocol::{AreaUnit, ColorMode, LightSource, ScanArea, WindowSettings};
use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions, Vec2};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ConnKind {
    Simulator,
    Local,
    Bridge,
}

/// Settings remembered between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    conn_kind: ConnKind,
    local_device: String,
    bridge_host: String,
    bridge_token: String,
    mode: ColorMode,
    dpi: f64,
    prescan_dpi: f64,
    light: LightSource,
    film: FilmType,
    output_dir: String,
    prefix: String,
    sixteen_bit: bool,
    area_unit: AreaUnit,
    res_scale: u32,
}

impl Default for Settings {
    fn default() -> Self {
        let pictures = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Pictures")
            .join("Cezanne Scans");
        Settings {
            conn_kind: if cfg!(target_os = "macos") { ConnKind::Bridge } else { ConnKind::Simulator },
            local_device: String::new(),
            bridge_host: String::new(),
            bridge_token: String::new(),
            mode: ColorMode::Rgb48,
            dpi: 2000.0,
            prescan_dpi: 75.0,
            light: LightSource::Transmissive,
            film: FilmType::ColorNegative,
            output_dir: pictures.to_string_lossy().to_string(),
            prefix: "scan_".into(),
            sixteen_bit: true,
            area_unit: AreaUnit::TenthMm,
            res_scale: 10,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Highlight,
    Shadow,
}

struct Frame {
    name: String,
    area: ScanArea,
    adj: Adjustments,
    auto_levels: bool,
    texture: Option<TextureHandle>,
    dirty: bool,
}

pub struct App {
    worker: Worker,
    s: Settings,
    presets: Vec<Preset>,
    connected: Option<String>,
    busy: bool,
    progress: f32,
    progress_label: String,
    status: String,
    log: Vec<String>,
    show_log: bool,
    prescan: Option<RawImage>,
    prescan_tex: Option<TextureHandle>,
    frames: Vec<Frame>,
    selected: Option<usize>,
    drag_from: Option<Pos2>,
    picker: Option<Pick>,
    hover: Option<(f64, f64, Vec<f64>)>,
    show_advanced: bool,
    demo: Option<Demo>,
}

/// Scripted run used for screenshots and smoke tests: `CEZANNE_DEMO=shot.png`.
struct Demo {
    shot: PathBuf,
    out_dir: Option<String>,
    step: DemoStep,
    frames_waited: u32,
}

#[derive(PartialEq)]
enum DemoStep {
    Connecting,
    Prescanning,
    Rendering,
    Shooting,
    Scanning,
    Finished,
}

const SETTINGS_KEY: &str = "cezanne-scan-settings";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut s: Settings = cc.storage.and_then(|st| eframe::get_value(st, SETTINGS_KEY)).unwrap_or_default();
        let demo = std::env::var_os("CEZANNE_DEMO").map(|shot| Demo {
            shot: PathBuf::from(shot),
            out_dir: std::env::var("CEZANNE_DEMO_OUT").ok(),
            step: DemoStep::Connecting,
            frames_waited: 0,
        });
        if demo.is_some() {
            s = Settings::default();
            s.conn_kind = ConnKind::Simulator;
            s.dpi = 600.0;
        }
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        cc.egui_ctx.set_style(style);
        let mut app = App {
            worker: Worker::spawn(cc.egui_ctx.clone()),
            s,
            presets: Preset::load_all(),
            connected: None,
            busy: false,
            progress: 0.0,
            progress_label: String::new(),
            status: "Not connected. Choose a connection and press Connect.".into(),
            log: Vec::new(),
            show_log: false,
            prescan: None,
            prescan_tex: None,
            frames: Vec::new(),
            selected: None,
            drag_from: None,
            picker: None,
            hover: None,
            show_advanced: false,
            demo,
        };
        if app.demo.is_some() {
            if let Some(d) = app.demo.as_ref().and_then(|d| d.out_dir.clone()) {
                app.s.output_dir = d;
            }
            app.send(Request::Connect(Connection::Simulator));
        }
        app
    }

    /// Advance the scripted demo run.
    fn demo_tick(&mut self, ctx: &egui::Context) {
        let Some(step) = self.demo.as_ref().map(|d| &d.step) else { return };
        match step {
            DemoStep::Connecting if self.connected.is_some() => {
                self.start_prescan();
                self.demo.as_mut().unwrap().step = DemoStep::Prescanning;
            }
            DemoStep::Prescanning if self.prescan.is_some() && !self.busy => {
                for (area, film) in [
                    (ScanArea::new(31.5, 31.5, 67.0, 53.0), FilmType::ColorNegative),
                    (ScanArea::new(121.5, 31.5, 81.0, 53.0), FilmType::BwNegative),
                    (ScanArea::new(31.5, 111.5, 53.0, 39.0), FilmType::Positive),
                ] {
                    self.selected = None;
                    self.s.film = film;
                    self.add_frame(area);
                }
                self.frames[0].name = "Colour neg".into();
                self.frames[1].name = "B&W neg".into();
                self.frames[2].name = "Slide".into();
                self.frames[0].adj.curve = Curve::s_curve(1.0);
                self.selected = Some(0);
                self.demo.as_mut().unwrap().step = DemoStep::Rendering;
            }
            DemoStep::Rendering => {
                let d = self.demo.as_mut().unwrap();
                d.frames_waited += 1;
                if d.frames_waited > 20 {
                    d.step = DemoStep::Shooting;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                }
            }
            DemoStep::Scanning if !self.busy => {
                self.demo.as_mut().unwrap().step = DemoStep::Finished;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        // screenshot arrives as an input event
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let d = self.demo.as_mut().unwrap();
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let _ = cezanne_core::image::png::write_rgba(&d.shot, img.size[0] as u32, img.size[1] as u32, &rgba);
            log::info!("demo screenshot saved to {}", d.shot.display());
            if d.out_dir.is_some() {
                d.step = DemoStep::Scanning;
                self.start_scan(false);
            } else {
                d.step = DemoStep::Finished;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        ctx.request_repaint();
    }

    fn limits(&self) -> ModelLimits {
        ModelLimits::FT_S5000
    }

    fn bed_mm(&self) -> (f64, f64) {
        self.limits().bed_mm(self.s.area_unit)
    }

    fn connection(&self) -> Connection {
        match self.s.conn_kind {
            ConnKind::Simulator => Connection::Simulator,
            ConnKind::Local if self.s.local_device.trim().is_empty() => Connection::AutoLocal,
            ConnKind::Local => Connection::Local(self.s.local_device.trim().to_string()),
            ConnKind::Bridge => Connection::Bridge { host: self.s.bridge_host.clone(), token: self.s.bridge_token.clone() },
        }
    }

    fn send(&mut self, r: Request) {
        if !matches!(r, Request::Connect(_) | Request::Disconnect | Request::Probe) {
            self.busy = true;
            self.progress = 0.0;
        }
        let _ = self.worker.tx.send(r);
    }

    fn log(&mut self, m: impl Into<String>) {
        let m = m.into();
        log::info!("{m}");
        self.status = m.lines().next().unwrap_or_default().to_string();
        self.log.push(m);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                Event::Connected(d) => {
                    self.log(format!("Connected: {d}"));
                    self.connected = Some(d);
                }
                Event::Disconnected => {
                    self.connected = None;
                    self.log("Disconnected");
                }
                Event::Progress { fraction, label } => {
                    self.progress = fraction;
                    self.progress_label = label;
                }
                Event::Prescan(img) => {
                    self.prescan_tex = Some(ctx.load_texture("prescan", raw_to_display(&img), TextureOptions::LINEAR));
                    self.prescan = Some(img);
                    for f in &mut self.frames {
                        f.dirty = true;
                    }
                }
                Event::Saved { name, path } => self.log(format!("Saved {name} to {}", path.display())),
                Event::Done(m) => {
                    self.busy = false;
                    self.log(m);
                }
                Event::Error(e) => {
                    self.busy = false;
                    self.log(format!("Error: {e}"));
                    self.show_log = true;
                }
                Event::Log(m) => {
                    self.log.push(m);
                    self.show_log = true;
                }
            }
        }
    }

    /// Raw prescan pixels inside a frame.
    fn frame_raw(&self, area: &ScanArea) -> Option<RawImage> {
        let img = self.prescan.as_ref()?;
        let (bw, bh) = self.bed_mm();
        let sx = img.width as f64 / bw;
        let sy = img.height as f64 / bh;
        let x = (area.x_mm * sx).max(0.0) as u32;
        let y = (area.y_mm * sy).max(0.0) as u32;
        let w = (area.width_mm * sx).max(1.0) as u32;
        let h = (area.height_mm * sy).max(1.0) as u32;
        Some(img.crop(x, y, w, h))
    }

    fn refresh_frame(&mut self, ctx: &egui::Context, i: usize) {
        let area = self.frames[i].area;
        let Some(raw) = self.frame_raw(&area) else { return };
        let f = &mut self.frames[i];
        if f.auto_levels {
            f.adj.auto_levels(&raw, 0.002);
            f.auto_levels = false;
        }
        let out = f.adj.apply(&raw);
        let img = ColorImage::from_rgba_unmultiplied([out.width as usize, out.height as usize], &out.to_rgba8());
        f.texture = Some(ctx.load_texture(format!("frame{i}"), img, TextureOptions::LINEAR));
        f.dirty = false;
    }

    fn current_preset_adjustments(&self) -> Adjustments {
        Adjustments::for_film(self.s.film)
    }

    fn add_frame(&mut self, area: ScanArea) {
        let n = self.frames.len() + 1;
        let mut adj = self.current_preset_adjustments();
        if let Some(sel) = self.selected.and_then(|i| self.frames.get(i)) {
            // new frames on the same roll start from the frame being worked on
            adj = sel.adj.clone();
        }
        self.frames.push(Frame { name: format!("Frame {n}"), area, adj, auto_levels: true, texture: None, dirty: true });
        self.selected = Some(self.frames.len() - 1);
    }

    fn apply_preset(&mut self, p: &Preset) {
        self.s.mode = p.mode;
        self.s.dpi = p.dpi;
        self.s.light = p.light;
        self.s.film = p.adjustments.film;
        self.s.sixteen_bit = p.output_depth == BitDepth::Sixteen;
        if let Some(f) = self.selected.and_then(|i| self.frames.get_mut(i)) {
            f.adj = p.adjustments.clone();
            f.auto_levels = p.auto_levels;
            f.dirty = true;
        }
        self.log(format!("Preset: {}", p.name));
    }

    fn window_for(&self, area: ScanArea, dpi: f64) -> WindowSettings {
        let mut ws = WindowSettings::new(area, dpi, self.s.mode);
        ws.original = self.s.film.original();
        ws.area_unit = self.s.area_unit;
        ws.resolution_scale = self.s.res_scale;
        ws
    }

    fn start_prescan(&mut self) {
        let (bw, bh) = self.bed_mm();
        let mut ws = self.window_for(ScanArea::new(0.0, 0.0, bw, bh), self.s.prescan_dpi);
        ws.mode = ColorMode::Rgb48;
        self.send(Request::Prescan(ws));
    }

    fn next_path(&self, used: &mut Vec<PathBuf>) -> PathBuf {
        let dir = PathBuf::from(&self.s.output_dir);
        for n in 1..100_000 {
            let p = dir.join(format!("{}{n:04}.tif", self.s.prefix));
            if !p.exists() && !used.contains(&p) {
                used.push(p.clone());
                return p;
            }
        }
        dir.join("overflow.tif")
    }

    fn start_scan(&mut self, only_selected: bool) {
        let idx: Vec<usize> = if only_selected { self.selected.into_iter().collect() } else { (0..self.frames.len()).collect() };
        if idx.is_empty() {
            self.log("Draw a frame on the prescan first");
            return;
        }
        let mut used = Vec::new();
        let mut jobs = Vec::new();
        for i in idx {
            let f = &self.frames[i];
            let ws = self.window_for(f.area, self.s.dpi);
            let g = ws.geometry();
            if g.pixels_per_line > self.limits().max_pixels_per_line as u32 || g.lines > u16::MAX as u32 {
                self.log(format!(
                    "{} is too large at {} dpi ({} x {} pixels). Lower the resolution.",
                    f.name, self.s.dpi, g.pixels_per_line, g.lines
                ));
                return;
            }
            jobs.push(FrameJob {
                name: f.name.clone(),
                window: ws,
                adjustments: f.adj.clone(),
                // frames never shown in the preview still need their automatic levels
                auto_levels: f.auto_levels,
                path: self.next_path(&mut used),
                depth: if self.s.sixteen_bit { BitDepth::Sixteen } else { BitDepth::Eight },
            });
        }
        self.send(Request::Scan(jobs));
    }

    // ------------------------------------------------------------------ panels

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Scanner");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.s.conn_kind, ConnKind::Simulator, "Simulator");
            if !cfg!(target_os = "macos") {
                ui.selectable_value(&mut self.s.conn_kind, ConnKind::Local, "This computer");
            }
            ui.selectable_value(&mut self.s.conn_kind, ConnKind::Bridge, "Bridge");
        });
        match self.s.conn_kind {
            ConnKind::Simulator => {
                ui.weak("A virtual FT-S5000 with test film on the bed.");
            }
            ConnKind::Local => {
                ui.horizontal(|ui| {
                    ui.label("Device");
                    ui.add(egui::TextEdit::singleline(&mut self.s.local_device).hint_text("automatic"));
                });
            }
            ConnKind::Bridge => {
                ui.horizontal(|ui| {
                    ui.label("Host");
                    ui.add(egui::TextEdit::singleline(&mut self.s.bridge_host).hint_text("pc-name or 192.168.0.20"));
                });
                ui.horizontal(|ui| {
                    ui.label("Token");
                    ui.add(egui::TextEdit::singleline(&mut self.s.bridge_token).password(true));
                });
            }
        }
        ui.horizontal(|ui| {
            if self.connected.is_none() {
                if ui.button("Connect").clicked() {
                    let c = self.connection();
                    self.log(format!("Connecting to {}", c.label()));
                    self.send(Request::Connect(c));
                }
            } else {
                if ui.button("Disconnect").clicked() {
                    self.send(Request::Disconnect);
                }
                if ui.button("Probe").on_hover_text("Read only report of the scanner's state").clicked() {
                    self.send(Request::Probe);
                }
            }
        });
        if let Some(d) = &self.connected {
            ui.colored_label(Color32::from_rgb(90, 170, 90), d);
        }

        ui.separator();
        ui.heading("Original");
        let presets = self.presets.clone();
        egui::ComboBox::from_label("Preset").selected_text("Choose…").show_ui(ui, |ui| {
            for p in &presets {
                if ui.selectable_label(false, &p.name).clicked() {
                    self.apply_preset(p);
                }
            }
        });
        egui::ComboBox::from_label("Film").selected_text(self.s.film.label()).show_ui(ui, |ui| {
            for f in FilmType::ALL {
                ui.selectable_value(&mut self.s.film, f, f.label());
            }
        });
        ui.weak(format!(
            "Scanned as {} with the {} lamp{}",
            self.s.film.original().label().to_lowercase(),
            if self.s.film.original().light() == LightSource::Reflective { "reflective" } else { "transmission" },
            if self.s.film.is_negative() { ", inverted on the computer" } else { "" }
        ));

        ui.separator();
        ui.heading("Scan");
        egui::ComboBox::from_label("Mode").selected_text(self.s.mode.label()).show_ui(ui, |ui| {
            for m in ColorMode::ALL {
                ui.selectable_value(&mut self.s.mode, m, m.label());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Resolution");
            ui.add(egui::DragValue::new(&mut self.s.dpi).speed(10.0).range(20.0..=12000.0).suffix(" dpi"));
        });
        ui.horizontal(|ui| {
            for d in [600.0, 1200.0, 2000.0, 3000.0, 4000.0] {
                if ui.small_button(format!("{d}")).clicked() {
                    self.s.dpi = d;
                }
            }
        });
        if let Some(f) = self.selected.and_then(|i| self.frames.get(i)) {
            let g = self.window_for(f.area, self.s.dpi).geometry();
            let mp = g.pixels_per_line as f64 * g.lines as f64 / 1e6;
            ui.weak(format!(
                "{} x {} px, {:.1} MP, {:.0} MB",
                g.pixels_per_line,
                g.lines,
                mp,
                g.total_bytes() as f64 / 1e6
            ));
            if g.pixels_per_line > self.limits().max_pixels_per_line as u32 {
                ui.colored_label(Color32::LIGHT_RED, "Too wide for the scanner at this resolution");
            }
        }

        ui.separator();
        ui.heading("Output");
        ui.horizontal(|ui| {
            ui.label("Folder");
            ui.text_edit_singleline(&mut self.s.output_dir);
        });
        ui.horizontal(|ui| {
            ui.label("Name prefix");
            ui.text_edit_singleline(&mut self.s.prefix);
        });
        ui.checkbox(&mut self.s.sixteen_bit, "16 bits per channel");
        if ui.button("Open output folder").clicked() {
            open_folder(&self.s.output_dir);
        }

        ui.separator();
        ui.collapsing("Advanced (diagnostics)", |ui| {
            ui.weak("Diagnostic options. The defaults match SCREEN's own driver, leave them unless asked to change them.");
            ui.horizontal(|ui| {
                ui.label("Area unit");
                ui.selectable_value(&mut self.s.area_unit, AreaUnit::TenthMm, "0.1 mm");
                ui.selectable_value(&mut self.s.area_unit, AreaUnit::Inch1200, "1/1200 in");
            });
            ui.horizontal(|ui| {
                ui.label("Resolution field scale");
                ui.add(egui::DragValue::new(&mut self.s.res_scale).range(1..=100));
            });
            ui.horizontal(|ui| {
                ui.label("Prescan");
                ui.add(egui::DragValue::new(&mut self.s.prescan_dpi).range(20.0..=300.0).suffix(" dpi"));
            });
            self.show_advanced = true;
        });
    }

    fn bed_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (bw, bh) = self.bed_mm();
        let avail = ui.available_size();
        let scale = (avail.x as f64 / bw).min(avail.y as f64 / bh) as f32;
        let size = Vec2::new(bw as f32 * scale, bh as f32 * scale);
        let (resp, painter) = ui.allocate_painter(avail, Sense::click_and_drag());
        // the bed keeps its true proportions, centred in the panel
        let r = Rect::from_center_size(resp.rect.center(), size);
        painter.rect_filled(r, 0.0, Color32::from_gray(28));
        if let Some(t) = &self.prescan_tex {
            painter.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else {
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "Press Prescan to see the bed",
                egui::FontId::proportional(18.0),
                Color32::GRAY,
            );
        }
        let to_mm = |p: Pos2| -> Pos2 { Pos2::new((p.x - r.min.x) / scale, (p.y - r.min.y) / scale) };
        let to_screen = |a: &ScanArea| -> Rect {
            Rect::from_min_size(
                Pos2::new(r.min.x + a.x_mm as f32 * scale, r.min.y + a.y_mm as f32 * scale),
                Vec2::new(a.width_mm as f32 * scale, a.height_mm as f32 * scale),
            )
        };

        // draw or select frames
        if resp.drag_started() {
            self.drag_from = resp.interact_pointer_pos().map(to_mm);
        }
        if let (Some(from), Some(pos)) = (self.drag_from, resp.interact_pointer_pos()) {
            let to = to_mm(pos);
            let rr = Rect::from_two_pos(from, to);
            let screen = Rect::from_two_pos(
                Pos2::new(r.min.x + rr.min.x * scale, r.min.y + rr.min.y * scale),
                Pos2::new(r.min.x + rr.max.x * scale, r.min.y + rr.max.y * scale),
            );
            outline(&painter, screen, Stroke::new(1.5_f32, Color32::YELLOW));
            if resp.drag_stopped() {
                self.drag_from = None;
                let rr = rr.intersect(Rect::from_min_size(Pos2::ZERO, Vec2::new(bw as f32, bh as f32)));
                if rr.width() > 3.0 && rr.height() > 3.0 {
                    self.add_frame(ScanArea::new(rr.min.x as f64, rr.min.y as f64, rr.width() as f64, rr.height() as f64));
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let mm = to_mm(p);
                self.selected = self.frames.iter().rposition(|f| {
                    Rect::from_min_size(Pos2::new(f.area.x_mm as f32, f.area.y_mm as f32), Vec2::new(f.area.width_mm as f32, f.area.height_mm as f32))
                        .contains(mm)
                });
            }
        }
        for (i, f) in self.frames.iter().enumerate() {
            let sr = to_screen(&f.area);
            let col = if Some(i) == self.selected { Color32::from_rgb(80, 200, 255) } else { Color32::from_rgb(255, 170, 60) };
            outline(&painter, sr, Stroke::new(2.0_f32, col));
            painter.text(sr.min + Vec2::new(4.0, 2.0), egui::Align2::LEFT_TOP, &f.name, egui::FontId::proportional(13.0), col);
        }

        // densitometer
        self.hover = None;
        if let (Some(p), Some(img)) = (resp.hover_pos(), self.prescan.as_ref()) {
            let mm = to_mm(p);
            let px = (mm.x as f64 / bw * img.width as f64) as u32;
            let py = (mm.y as f64 / bh * img.height as f64) as u32;
            if px < img.width && py < img.height {
                let d = img.density_at(px.saturating_sub(1), py.saturating_sub(1), 3, 3);
                self.hover = Some((mm.x as f64, mm.y as f64, d));
            }
        }
        let _ = ctx;
    }

    fn right_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(i) = self.selected.filter(|&i| i < self.frames.len()) else {
            ui.heading("Frame");
            ui.label("Drag on the prescan to mark a frame. Each frame keeps its own settings.");
            ui.add_space(8.0);
            ui.label("Workflow from Cezanne owners:");
            ui.label("1. Prescan, then draw a frame around each image.");
            ui.label("2. For negatives use flipped densities: HD high, SD low.");
            ui.label("3. Use the droppers: highlight on the brightest part of the image, shadow on the film rebate.");
            ui.label("4. Keep MIN 0 and MAX 255, sharpening off, add a gentle S curve.");
            ui.label("5. Scan all frames in one go.");
            return;
        };
        if self.frames[i].dirty {
            self.refresh_frame(ctx, i);
        }
        let mut changed = false;
        let mut delete = false;
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.frames[i].name);
            if ui.button("Delete").clicked() {
                delete = true;
            }
        });

        // processed preview with droppers
        if let Some(t) = self.frames[i].texture.clone() {
            let tsize = t.size_vec2();
            let w = ui.available_width().min(420.0);
            let size = Vec2::new(w, w * tsize.y / tsize.x.max(1.0));
            let (resp, painter) = ui.allocate_painter(size, Sense::click());
            painter.image(t.id(), resp.rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            if let (Some(pick), Some(p)) = (self.picker, resp.interact_pointer_pos()) {
                if resp.clicked() {
                    let u = ((p.x - resp.rect.min.x) / resp.rect.width()) as f64;
                    let v = ((p.y - resp.rect.min.y) / resp.rect.height()) as f64;
                    let area = self.frames[i].area;
                    if let Some(raw) = self.frame_raw(&area) {
                        let x = (u * raw.width as f64) as u32;
                        let y = (v * raw.height as f64) as u32;
                        let d = raw.density_at(x.saturating_sub(1), y.saturating_sub(1), 3, 3);
                        let f = &mut self.frames[i];
                        for c in 0..3 {
                            let dv = d[c.min(d.len() - 1)] as f32;
                            match pick {
                                Pick::Highlight => f.adj.hd[c] = dv,
                                Pick::Shadow => f.adj.sd[c] = dv,
                            }
                        }
                        changed = true;
                    }
                    self.picker = None;
                }
            }
        }
        ui.horizontal(|ui| {
            ui.toggle_value(&mut (self.picker == Some(Pick::Highlight)), "Pick highlight")
                .clicked()
                .then(|| self.picker = if self.picker == Some(Pick::Highlight) { None } else { Some(Pick::Highlight) });
            ui.toggle_value(&mut (self.picker == Some(Pick::Shadow)), "Pick shadow")
                .clicked()
                .then(|| self.picker = if self.picker == Some(Pick::Shadow) { None } else { Some(Pick::Shadow) });
            if ui.button("Auto").on_hover_text("Set HD and SD from this frame").clicked() {
                self.frames[i].auto_levels = true;
                changed = true;
            }
            if ui.button("Reset").clicked() {
                let film = self.frames[i].adj.film;
                self.frames[i].adj = Adjustments::for_film(film);
                changed = true;
            }
        });

        let f = &mut self.frames[i];
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("densities").num_columns(4).show(ui, |ui| {
                ui.label("");
                ui.colored_label(Color32::from_rgb(240, 110, 110), "R");
                ui.colored_label(Color32::from_rgb(110, 220, 110), "G");
                ui.colored_label(Color32::from_rgb(120, 150, 255), "B");
                ui.end_row();
                ui.label("HD").on_hover_text("Highlight density");
                for c in 0..3 {
                    changed |= ui.add(egui::DragValue::new(&mut f.adj.hd[c]).speed(0.01).range(0.0..=4.5).fixed_decimals(2)).changed();
                }
                ui.end_row();
                ui.label("SD").on_hover_text("Shadow density");
                for c in 0..3 {
                    changed |= ui.add(egui::DragValue::new(&mut f.adj.sd[c]).speed(0.01).range(0.0..=4.5).fixed_decimals(2)).changed();
                }
                ui.end_row();
            });
            if ui.small_button("Link channels to G").clicked() {
                f.adj.hd = [f.adj.hd[1]; 3];
                f.adj.sd = [f.adj.sd[1]; 3];
                changed = true;
            }
            changed |= ui.add(egui::Slider::new(&mut f.adj.gamma, 0.3..=3.0).text("Mid tone")).changed();
            changed |= ui.add(egui::Slider::new(&mut f.adj.output_min, 0.0..=128.0).text("MIN")).changed();
            changed |= ui.add(egui::Slider::new(&mut f.adj.output_max, 128.0..=255.0).text("MAX")).changed();

            ui.label("Curve (drag points, double click to add, right click to remove)");
            changed |= curve_editor(ui, &mut f.adj.curve, 220.0);
            ui.horizontal(|ui| {
                if ui.small_button("Linear").clicked() {
                    f.adj.curve = Curve::linear();
                    changed = true;
                }
                if ui.small_button("Gentle S").clicked() {
                    f.adj.curve = Curve::s_curve(1.0);
                    changed = true;
                }
            });

            ui.separator();
            changed |= ui.checkbox(&mut f.adj.sharpen.enabled, "Sharpen").changed();
            if f.adj.sharpen.enabled {
                changed |= ui.add(egui::Slider::new(&mut f.adj.sharpen.amount, 0.0..=3.0).text("Amount")).changed();
                changed |= ui.add(egui::Slider::new(&mut f.adj.sharpen.radius, 0.3..=5.0).text("Radius")).changed();
            }
            ui.horizontal(|ui| {
                ui.label("Colour");
                changed |= ui.selectable_value(&mut f.adj.gray, GrayMode::Keep, "Keep").changed();
                changed |= ui.selectable_value(&mut f.adj.gray, GrayMode::Luminance, "Grey").changed();
                for (k, n) in ["R", "G", "B"].iter().enumerate() {
                    changed |= ui.selectable_value(&mut f.adj.gray, GrayMode::Channel(k as u8), *n).on_hover_text("Keep only this channel").changed();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Rotate").clicked() {
                    f.adj.rotate = (f.adj.rotate + 1) % 4;
                    changed = true;
                }
                changed |= ui.checkbox(&mut f.adj.mirror, "Mirror").changed();
            });
        });
        if changed {
            self.frames[i].dirty = true;
        }
        if delete {
            self.frames.remove(i);
            self.selected = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self.demo.is_some() {
            self.demo_tick(ctx);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Delete)) && !ctx.wants_keyboard_input() {
            if let Some(i) = self.selected.take() {
                if i < self.frames.len() {
                    self.frames.remove(i);
                }
            }
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let can = self.connected.is_some() && !self.busy;
                if ui.add_enabled(can, egui::Button::new("Prescan")).clicked() {
                    self.start_prescan();
                }
                if ui.add_enabled(can && self.selected.is_some(), egui::Button::new("Scan frame")).clicked() {
                    self.start_scan(true);
                }
                if ui.add_enabled(can && !self.frames.is_empty(), egui::Button::new(format!("Scan all ({})", self.frames.len()))).clicked() {
                    self.start_scan(false);
                }
                if self.busy && ui.button("Cancel").clicked() {
                    self.worker.cancel.store(true, Ordering::Relaxed);
                }
                if self.busy {
                    ui.add(egui::ProgressBar::new(self.progress).text(format!("{} {:.0}%", self.progress_label, self.progress * 100.0)).desired_width(260.0));
                }
                ui.separator();
                if let Some((x, y, d)) = &self.hover {
                    let ds: Vec<String> = d.iter().map(|v| format!("{v:.2}")).collect();
                    ui.monospace(format!("x {x:5.1} y {y:5.1} mm  D {}", ds.join(" ")));
                    ui.separator();
                }
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.show_log, "Log");
                });
            });
            if self.show_log {
                egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                    for l in &self.log {
                        ui.monospace(l);
                    }
                });
            }
        });

        egui::SidePanel::left("left").resizable(true).default_width(300.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.left_panel(ui));
        });
        egui::SidePanel::right("right").resizable(true).default_width(380.0).show(ctx, |ui| {
            self.right_panel(ui, ctx);
        });
        egui::CentralPanel::default().show(ctx, |ui| self.bed_view(ui, ctx));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.demo.is_none() {
            eframe::set_value(storage, SETTINGS_KEY, &self.s);
        }
    }
}

fn outline(p: &egui::Painter, r: Rect, s: Stroke) {
    p.line_segment([r.left_top(), r.right_top()], s);
    p.line_segment([r.right_top(), r.right_bottom()], s);
    p.line_segment([r.right_bottom(), r.left_bottom()], s);
    p.line_segment([r.left_bottom(), r.left_top()], s);
}

/// Linear scanner data to an sRGB-ish display image.
fn raw_to_display(img: &RawImage) -> ColorImage {
    let lut: Vec<u8> = (0..4096).map(|i| ((i as f64 / 4095.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
    let mut rgba = Vec::with_capacity((img.width * img.height) as usize * 4);
    for px in img.data.chunks_exact(img.channels) {
        let g = |k: usize| lut[(px[k.min(img.channels - 1)] >> 4) as usize];
        rgba.extend_from_slice(&[g(0), g(1), g(2), 255]);
    }
    ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &rgba)
}

fn open_folder(path: &str) {
    let _ = std::fs::create_dir_all(path);
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}
