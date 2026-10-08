use std::path::PathBuf;

fn main() -> eframe::Result {
    // A file to open, e.g. from the file manager: a package, an archive or a folder.
    let file = std::env::args_os().nth(1).map(PathBuf::from);
    // One app at a time: a running one takes the file and comes forward.
    let instance = match paleto_modkit::single_instance::claim(file.as_slice()) {
        paleto_modkit::single_instance::Claim::HandedOver => return Ok(()),
        paleto_modkit::single_instance::Claim::First(instance) => Some(instance),
        paleto_modkit::single_instance::Claim::Unavailable => None,
    };
    paleto_modkit::appimage::integrate();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(paleto_modkit::NAME)
            .with_app_id(paleto_modkit::APP_ID)
            .with_icon(paleto_modkit::icon())
            .with_inner_size([1200.0, 760.0])
            // Below this the file list, the tree and a preview no longer fit side by side.
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "paleto-modkit",
        options,
        Box::new(move |cc| {
            let mut app = paleto_modkit::App::new(cc);
            if let Some(instance) = instance {
                app.set_instance(instance, &cc.egui_ctx);
            }
            if let Some(file) = file {
                app.open_path(&file, &cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}
