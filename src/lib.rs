pub mod application;
pub mod bookmarks;
pub mod config;
pub mod window;

pub use adw::{gdk, gio, glib, gtk};
pub use gstreamer as gst;
pub use libadwaita as adw;

/// Translations, resources, the application name: everything that does not touch GTK.
///
/// Keep this the only thing an application does before it registers on the bus. GTK 4.22
/// reads its settings from xdg-desktop-portal, and that portal may well be the one waiting
/// on us to answer a D-Bus call, so initialising GTK first deadlocks the pair.
///
/// # Safety
///
/// Call it first in `main`, before any thread is started: it sets the locale, which reads
/// the environment and changes state other threads may be reading.
pub unsafe fn init_early() {
    // SAFETY: the caller has started no thread yet.
    unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR).ok();
    gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8").ok();
    gettextrs::textdomain(config::GETTEXT_PACKAGE).ok();

    gio::resources_register_include!("spiral.gresource").expect("register resources");
    glib::set_application_name("Spiral");
}

/// The stylesheet and the bundled icons. Needs GTK started, so `AdwApplication` calls it
/// from `startup`.
pub fn init_style() {
    let css = gtk::CssProvider::new();
    css.load_from_resource(&format!("{}/style.css", config::RESOURCE_PATH));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // `GtkApplication` does this for the file manager; the portal backend has no
        // application object, and without it the icons we ship draw as broken images.
        gtk::IconTheme::for_display(&display)
            .add_resource_path(&format!("{}/icons", config::RESOURCE_PATH));
    }
}

/// GTK and the stylesheet, for binaries without an `AdwApplication` of their own, once
/// [`init_early`] has run.
pub fn init() {
    adw::init().expect("libadwaita init");
    init_style();
}
pub mod browser_actions;
pub mod browser_view;
pub mod clipboard;
pub mod dbus;
pub mod details_panel;
pub mod dialogs;
pub mod disks;
pub mod enums;
pub mod file_utils;
pub mod folder_model;
pub mod lines;
pub mod location_entry;
pub mod metadata;
pub mod miller;
pub mod naming;
pub mod network;
pub mod object_data;
pub mod ops;
pub mod path_bar;
pub mod places_sidebar;
pub mod player;
pub mod portal;
pub mod prefs;
pub mod progress_indicator;
pub mod search;
pub mod starred;
pub mod tags;
pub mod templates;
pub mod terminal;
pub mod thumbnails;
