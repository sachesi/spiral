pub mod application;
pub mod bookmarks;
pub mod config;
pub mod window;

pub use adw::{gdk, gio, glib, gtk};
pub use libadwaita as adw;

/// Register resources, set up i18n and initialize libadwaita.
/// Must run before any widget is created, in every binary.
pub fn init() {
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR).ok();
    gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8").ok();
    gettextrs::textdomain(config::GETTEXT_PACKAGE).ok();

    gio::resources_register_include!("spiral.gresource").expect("register resources");
    glib::set_application_name("Spiral");
    adw::init().expect("libadwaita init");

    let css = gtk::CssProvider::new();
    css.load_from_resource(&format!("{}/style.css", config::RESOURCE_PATH));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
pub mod browser_actions;
pub mod browser_view;
pub mod clipboard;
pub mod dbus;
pub mod dialogs;
pub mod enums;
pub mod file_utils;
pub mod folder_model;
pub mod location_entry;
pub mod naming;
pub mod ops;
pub mod path_bar;
pub mod places_sidebar;
pub mod portal;
pub mod prefs;
pub mod progress_indicator;
pub mod starred;
pub mod terminal;
pub mod thumbnails;
