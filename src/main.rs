mod app;
mod audio;
mod instance;
mod platform;
mod router;
mod settings;
mod state;
mod ui;
mod window_position;

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

    // Restored before the window exists, because moving it afterwards would
    // show the capsule in one place and then jump it to another.
    // The size is passed in rather than assumed, because the check is about
    // how much of this capsule lands on a display.
    let size = (ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y);
    let remembered = window_position::load(&data_dir, size);
    let settings = settings::load(&data_dir);
    let mut viewport = egui::ViewportBuilder::default()
            .with_inner_size([ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y])
            .with_min_inner_size([ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y])
            .with_max_inner_size([ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_title("LocalFlow");
    if let Some(centre) = remembered {
        let (x, y) = window_position::place(centre, size, &platform::work_areas());
        viewport = viewport.with_position([x, y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "LocalFlow",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::LocalFlowApp::new(cc, data_dir.clone(), settings.clone())))
        }),
    )
}
