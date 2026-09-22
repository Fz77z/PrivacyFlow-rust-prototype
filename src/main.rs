mod app;
mod audio;
mod insertion;
mod instance;
mod latency_trace;
mod platform;
mod router;
mod settings;
mod state;
mod ui;
mod window_position;

fn main() -> eframe::Result<()> {
    let Some(data_root) = dirs::data_local_dir() else {
        eprintln!("PrivacyFlow could not determine the macOS local data directory.");
        return Ok(());
    };
    let data_dir = data_root.join("PrivacyFlow");
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("PrivacyFlow could not create its data directory: {error}");
        return Ok(());
    }
    let _instance_lock = match instance::InstanceLock::acquire(&data_dir.join("privacyflow.lock")) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("PrivacyFlow will not start: {error:#}");
            return Ok(());
        }
    };

    // Restored before the window exists, because moving it afterwards would
    // show the capsule in one place and then jump it to another.
    // The size is passed in rather than assumed, because the check is about
    // how much of this capsule lands on a display.
    let size = (ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y);
    let remembered = window_position::load(&data_dir);
    let settings = settings::load(&data_dir);
    // The window changes size only when the minimal mode setting is toggled,
    // never while the capsule is animating. With minimal mode on it is the
    // catchment: the area macOS delivers mouse events for, and therefore the
    // area within which the capsule can notice someone approaching. The three
    // capsule sizes are painted inside it rather than being window sizes.
    let catchment = ui::theme::window_size(settings.settings.minimal_mode);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([catchment.x, catchment.y])
        .with_min_inner_size([ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y])
        .with_max_inner_size([ui::theme::CATCHMENT_SIZE.x, ui::theme::CATCHMENT_SIZE.y])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_title("PrivacyFlow");
    if let Some(centre) = remembered {
        // Clamped so the visible capsule lands on screen, not so the whole
        // catchment does. The catchment's outer ring is never painted, and
        // keeping it on screen would push the capsule further from the edge
        // than the user put it, for the sake of space nobody can see.
        let (x, y) = window_position::place(centre, size, &platform::work_areas());
        viewport = viewport.with_position([
            x - (catchment.x - size.0) / 2.0,
            y - (catchment.y - size.1) / 2.0,
        ]);
    }
    let options = eframe::NativeOptions {
        viewport,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            // Stated here rather than left to the bundle's LSUIElement. winit
            // honours that key only when it can see a bundle identifier, and
            // forces the Regular policy when it cannot, so a binary run
            // straight out of target/ would take a Dock icon and a slot in
            // the application switcher while the installed app does not.
            //
            // Accessory, not Prohibited: the console is an ordinary window
            // that has to be able to take focus and keystrokes, which a
            // prohibited application cannot do.
            builder.with_activation_policy(ActivationPolicy::Accessory);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "PrivacyFlow",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::PrivacyFlowApp::new(cc, data_dir.clone(), settings.clone())))
        }),
    )
}
