//! 3dsrecomp's desktop app, the cli's analyze, build, port and hints in a window
//! pick a game, press a button, watch it go

// no console window behind the app in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod report;
mod theme;
mod toolchain;
mod worker;

/// names where the app keeps its settings, apart from the folder the libs get installed in
const APP_ID: &str = "3dsrecomp-gui";

const ICON: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("3dsrecomp")
        .with_app_id(APP_ID)
        .with_inner_size([1080.0, 860.0])
        .with_min_inner_size([760.0, 560.0])
        .with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(APP_ID, options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
