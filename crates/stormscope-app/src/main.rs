//! stormscope binary entry point.

use stormscope_app::StormApp;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("stormscope"),
        ..Default::default()
    };

    eframe::run_native(
        "stormscope",
        options,
        Box::new(|cc| Ok(Box::new(StormApp::new(cc)))),
    )
}
