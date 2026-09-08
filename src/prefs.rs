//! User preferences that shape formatting and loading, read straight from GSettings so every
//! caller sees a change at once.

use gettextrs::gettext;

use crate::{gio, glib};
use gio::prelude::*;

thread_local! {
    static SETTINGS: gio::Settings = gio::Settings::new(crate::config::APP_ID);
}

pub fn settings() -> gio::Settings {
    SETTINGS.with(|s| s.clone())
}

fn choice(key: &str) -> i32 {
    SETTINGS.with(|s| s.enum_(key))
}

/// Keys whose change needs open views to reload.
pub const VIEW_KEYS: [&str; 5] = [
    "size-units",
    "folders-first",
    "date-format",
    "thumbnails",
    "item-counts",
];

/// Human size in the unit system the user chose.
pub fn size(bytes: u64) -> String {
    if choice("size-units") == 1 {
        glib::format_size_full(bytes, glib::FormatSizeFlags::IEC_UNITS).to_string()
    } else {
        glib::format_size(bytes).to_string()
    }
}

/// A date in the user's preferred style.
pub fn date(dt: &glib::DateTime) -> String {
    if choice("date-format") == 1 {
        return dt
            .format("%x, %H:%M")
            .map(|s| s.to_string())
            .unwrap_or_else(|_| gettext("Unknown"));
    }
    crate::file_utils::relative_date(dt)
}

pub fn folders_first() -> bool {
    SETTINGS.with(|s| s.boolean("folders-first"))
}

pub fn tree_view() -> bool {
    SETTINGS.with(|s| s.boolean("use-tree-view"))
}

/// The view and sort order of a folder are kept as `metadata::` attributes, which exist
/// only where gvfs runs its metadata backend. Without it nothing can be stored per folder,
/// so the global default takes over instead of the choice quietly going nowhere.
pub fn per_folder_available() -> bool {
    thread_local! {
        static AVAILABLE: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
    }
    AVAILABLE.with(|a| {
        *a.get_or_init(|| {
            gio::File::for_path(glib::home_dir())
                .query_writable_namespaces(gio::Cancellable::NONE)
                .is_ok_and(|list| list.lookup("metadata").is_some())
        })
    })
}

/// The column view is off unless it is asked for: it is a third way of reading a folder,
/// not one everybody wants in the view button.
pub fn column_view() -> bool {
    SETTINGS.with(|s| s.boolean("use-column-view"))
}

pub fn remember_view() -> bool {
    SETTINGS.with(|s| s.boolean("remember-view")) && per_folder_available()
}

pub fn guess_view() -> bool {
    SETTINGS.with(|s| s.boolean("guess-view"))
}

pub fn single_click() -> bool {
    choice("click-policy") == 1
}

/// Local files only / always / never, as the two scope keys encode it.
fn scope_allows(key: &str, file: &gio::File) -> bool {
    match choice(key) {
        0 => file.is_native(),
        1 => true,
        _ => false,
    }
}

pub fn thumbnails_for(file: &gio::File) -> bool {
    scope_allows("thumbnails", file)
}

pub fn counts_for(file: &gio::File) -> bool {
    scope_allows("item-counts", file)
}

pub fn recursive_search_for(file: &gio::File) -> bool {
    scope_allows("recursive-search", file)
}
