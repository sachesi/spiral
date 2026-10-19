pub(crate) mod appimage;
pub(crate) mod application;
pub(crate) mod bookmarks;
pub(crate) mod config;
pub(crate) mod window;

pub use adw::{gdk, gio, glib, gtk};
pub(crate) use gstreamer as gst;
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
pub(crate) fn init_style() {
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

/// GTK and the stylesheet, for programs without an `AdwApplication` of their own, once
/// [`init_early`] has run.
pub(crate) fn init() {
    adw::init().expect("libadwaita init");
    init_style();
}
pub(crate) mod browser_actions;
pub(crate) mod browser_view;
pub(crate) mod clipboard;
pub(crate) mod dbus;
pub mod desktop;
pub(crate) mod details_panel;
pub(crate) mod dialogs;
pub(crate) mod disks;
pub(crate) mod drop_ins;
pub(crate) mod enums;
pub(crate) mod file_utils;
pub(crate) mod folder_model;
pub(crate) mod glycin;
pub(crate) mod lines;
pub(crate) mod location_entry;
pub(crate) mod media;
pub(crate) mod metadata;
pub(crate) mod miller;
pub(crate) mod naming;
pub(crate) mod network;
pub(crate) mod object_data;
pub(crate) mod ops;
pub(crate) mod path_bar;
pub(crate) mod picture;
pub(crate) mod places_sidebar;
pub(crate) mod player;
pub(crate) mod portal;
pub(crate) mod prefs;
#[doc(hidden)]
pub mod programs;
pub(crate) mod progress_indicator;
pub(crate) mod sandbox;
pub(crate) mod search;
pub(crate) mod starred;
pub(crate) mod tags;
pub(crate) mod templates;
pub(crate) mod terminal;
pub(crate) mod thumbnails;
pub(crate) mod touch;
