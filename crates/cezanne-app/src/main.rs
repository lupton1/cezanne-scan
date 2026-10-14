//! Cezanne Scan desktop app.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod curve_editor;
mod worker;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Cezanne Scan")
            .with_inner_size([1360.0, 880.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("Cezanne Scan", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
