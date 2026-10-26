//! The desktop: the icons of the desktop folder, and the places shown beside them, on a
//! surface under every window.
//!
//! This is what the library offers to programs that show a desktop: the application to run
//! it in and the view of the icons. Putting the view on a surface under the windows is left
//! to them.

pub(crate) mod displays;
pub(crate) mod map;
pub(crate) mod placement;
pub(crate) mod positions;
pub(crate) mod specials;

use crate::adw;
use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::gtk::prelude::*;
use crate::{gdk, gio, gtk};

/// Whether `widget` is on the desktop, rather than in a window of the file manager.
pub(crate) fn on_desktop(widget: &impl IsA<gtk::Widget>) -> bool {
    widget
        .root()
        .is_some_and(|root| root.has_css_class("spiral-desktop-window"))
}

/// The application a desktop runs in, `io.github.sachesi.spiral.Desktop`. Only one runs
/// at a time, it opens no window when activated, and file operations started from the
/// desktop run in it with their progress and questions.
pub fn application() -> adw::Application {
    SpiralApplication::new_desktop().upcast()
}

/// Calls `f` with the displays that show a desktop, now and whenever they change.
pub fn watch_displays(f: impl Fn(&[gdk::Monitor]) + 'static) {
    let map = map::DisplayMap::get();
    f(&map.shown());
    map.connect_changed(move |map| f(&map.shown()));
}

/// The files of a folder as desktop icons, with Home, the trash and the drives beside them
/// as the settings ask, and the menus, keys and drag and drop of Spiral's views.
#[derive(Debug, Clone)]
pub struct DesktopView {
    view: BrowserView,
}

impl DesktopView {
    /// The icons of `folder` on `monitor`: with more than one display, each file is on one
    /// of them.
    pub fn new(folder: &gio::File, monitor: &gdk::Monitor) -> Self {
        let view = BrowserView::new_desktop(folder);
        map::DisplayMap::get().set_folder(folder);
        if let Some(canvas) = view.desktop_canvas() {
            canvas.set_monitor(monitor);
        }
        Self { view }
    }

    /// The widget to put on the desktop's surface, filling it.
    pub fn widget(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }
}
