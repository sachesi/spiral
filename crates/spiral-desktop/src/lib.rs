//! The icons of the desktop folder on a surface under every window, for Wayland compositors
//! with the layer shell: the desktop of the Spiral file manager, with its views, menus and
//! file operations.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gettextrs::gettext;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use spiral_core::desktop::DesktopView;
use spiral_core::gio::prelude::*;
use spiral_core::gtk::gdk;
use spiral_core::gtk::prelude::*;
pub use spiral_core::{gio, glib, gtk};

/// Runs the desktop until it is quit, a surface on each display; a second one started
/// meanwhile leaves the first running. Call [`spiral_core::init_early`] first.
pub fn run() -> glib::ExitCode {
    let app = spiral_core::desktop::application();
    let started = Cell::new(false);
    app.connect_activate(move |app| {
        if started.replace(true) {
            return;
        }
        // With every display gone, as when a laptop is closed, the desktop waits for one.
        std::mem::forget(app.hold());
        let windows: Rc<RefCell<Vec<(gdk::Monitor, gtk::ApplicationWindow)>>> = Rc::default();
        let app = app.clone();
        spiral_core::desktop::watch_displays(move |shown| {
            let mut windows = windows.borrow_mut();
            windows.retain(|(monitor, window)| {
                let keep = shown.contains(monitor);
                if !keep {
                    window.destroy();
                }
                keep
            });
            for monitor in shown {
                if !windows.iter().any(|(m, _)| m == monitor) {
                    let window = desktop_window(&app, monitor);
                    window.present();
                    windows.push((monitor.clone(), window));
                }
            }
        });
    });
    app.run()
}

/// The folder the desktop shows, made if it is not there yet.
fn desktop_folder() -> gio::File {
    let path = glib::user_special_dir(glib::UserDirectory::Desktop)
        .unwrap_or_else(|| glib::home_dir().join("Desktop"));
    if let Err(e) = std::fs::create_dir_all(&path) {
        glib::g_warning!("spiral", "Could not make {}: {e}", path.display());
    }
    gio::File::for_path(path)
}

fn desktop_window(
    app: &spiral_core::adw::Application,
    monitor: &gdk::Monitor,
) -> gtk::ApplicationWindow {
    // Not an Adwaita window: dialogs then open as windows of their own rather than inside
    // a surface that every other window covers.
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(gettext("Desktop"))
        .css_classes(["spiral-desktop-window"])
        .build();
    window.init_layer_shell();
    window.set_namespace(Some("spiral-desktop"));
    window.set_monitor(Some(monitor));
    window.set_layer(Layer::Bottom);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    // Inside the room panels and docks leave, and the keyboard only once clicked.
    window.set_exclusive_zone(0);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    // A display going away closes the surface, and the window goes with the display. Closed
    // with the display still there, it comes back.
    window.set_respect_close(true);
    window.connect_close_request(|window| {
        window.set_visible(false);
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            window,
            move || {
                if window.monitor().is_some_and(|m| m.is_valid()) {
                    window.present();
                }
            }
        ));
        glib::Propagation::Stop
    });
    let desktop = DesktopView::new(&desktop_folder(), monitor);
    window.set_child(Some(&desktop.widget()));
    window
}
