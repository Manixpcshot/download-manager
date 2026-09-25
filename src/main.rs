#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod apps;
mod database;
mod downloader;
mod models;
mod notifications;
mod queue;
mod settings;
mod system;
mod tray;
mod ui;
mod utils;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pulse Download Manager")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([980.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Pulse Download Manager",
        options,
        Box::new(|creation_context| {
            app::DownloadManagerApp::new(creation_context)
                .map(|application| Box::new(application) as Box<dyn eframe::App>)
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(std::io::Error::other(error.to_string()))
                })
        }),
    )
}
