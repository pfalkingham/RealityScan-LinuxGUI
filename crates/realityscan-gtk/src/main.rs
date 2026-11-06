//! realityscan-gtk: a native front-end for RealityScan running headless under Wine.

mod align_panel;
mod backend;
mod export_dialog;
mod image_view;
mod mask;
mod recent;
mod region_panel;
mod settings;
mod thumbs;
mod viewport;
mod window;

use gtk4 as gtk;
use libadwaita as adw;

use adw::prelude::*;

const APP_ID: &str = "org.realityscan_gtk.RealityScanGtk";

fn main() -> gtk::glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        // CAN_OVERRIDE_APP_ID lets `--gapplication-app-id=…` run a second copy (handy with
        // REALITYSCAN_GTK_ATTACH while another copy owns the RealityScan instance).
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN | gtk::gio::ApplicationFlags::CAN_OVERRIDE_APP_ID)
        .build();
    // GTK is initialised by `run`, so the icon name has to wait for start-up.
    app.connect_startup(|_| gtk::Window::set_default_icon_name(APP_ID));
    app.connect_activate(|app| {
        window::MainWindow::new(app, backend::start(), None).present();
    });
    // `realityscan-gtk project.rsproj` opens that project once RealityScan is up.
    app.connect_open(|app, files, _hint| {
        let project = files.first().and_then(|f| f.path());
        window::MainWindow::new(app, backend::start(), project).present();
    });
    app.run()
}
