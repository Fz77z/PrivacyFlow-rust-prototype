mod app;
mod audio;
mod instance;
mod platform;
mod router;
mod state;
mod ui;

fn main() -> eframe::Result<()> {
    let Some(data_root) = dirs::data_local_dir() else {
        eprintln!("LocalFlow could not determine the macOS local data directory.");
        return Ok(());
    };
    let data_dir = data_root.join("LocalFlow");
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("LocalFlow could not create its data directory: {error}");
        return Ok(());
    }
    let _instance_lock = match instance::InstanceLock::acquire(&data_dir.join("localflow.lock")) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("LocalFlow will not start: {error:#}");
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([240.0, 56.0])
            .with_min_inner_size([240.0, 56.0])
            .with_max_inner_size([240.0, 56.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_title("LocalFlow"),
        ..Default::default()
    };
    eframe::run_native(
        "LocalFlow",
        options,
        Box::new(move |cc| Ok(Box::new(app::LocalFlowApp::new(cc, data_dir.clone())))),
    )
}
