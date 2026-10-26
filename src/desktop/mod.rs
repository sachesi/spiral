//! The desktop: the icons of the desktop folder, and the places shown beside them, on a
//! surface under every window.
//!
//! This is what the library offers to programs that show a desktop: the application to run
//! it in and the view of the icons. Putting the view on a surface under the windows is left
//! to them.

pub(crate) mod placement;
pub(crate) mod positions;
pub(crate) mod specials;

use crate::adw;
use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::gtk::prelude::*;
use crate::{gio, gtk};

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

/// The files of a folder as desktop icons, with Home, the trash and the drives beside them
/// as the settings ask, and the menus, keys and drag and drop of Spiral's views.
#[derive(Debug, Clone)]
pub struct DesktopView {
    view: BrowserView,
}

impl DesktopView {
    /// The icons of `folder`.
    pub fn new(folder: &gio::File) -> Self {
        Self {
            view: BrowserView::new_desktop(folder),
        }
    }

    /// The widget to put on the desktop's surface, filling it.
    pub fn widget(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }
}
