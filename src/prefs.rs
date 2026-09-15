//! User preferences that shape formatting and loading, read straight from GSettings so every
//! caller sees a change at once.

use gettextrs::gettext;

use crate::{gio, glib};
use gio::prelude::*;

thread_local! {
    static SETTINGS: gio::Settings = new_settings();
}

#[cfg(not(test))]
fn new_settings() -> gio::Settings {
    gio::Settings::new(crate::config::APP_ID)
}

/// The tests read the schema compiled into the build directory and keep what they set in
/// memory, whatever is installed and whatever the user has chosen.
#[cfg(test)]
fn new_settings() -> gio::Settings {
    let source = gio::SettingsSchemaSource::from_directory(
        concat!(env!("OUT_DIR"), "/schemas"),
        None,
        false,
    )
    .expect("schema compiled by build.rs");
    let schema = source
        .lookup(crate::config::APP_ID, false)
        .expect("Spiral's schema");
    gio::Settings::new_full(&schema, Some(&gio::memory_settings_backend_new()), None)
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

/// Asked once per comparison while a folder sorts, so the answer is kept and the signal
/// keeps it honest rather than every comparison going to GSettings.
pub fn folders_first() -> bool {
    thread_local! {
        static VALUE: std::cell::OnceCell<std::rc::Rc<std::cell::Cell<bool>>> =
            const { std::cell::OnceCell::new() };
    }
    VALUE.with(|v| {
        v.get_or_init(|| {
            let cell = std::rc::Rc::new(std::cell::Cell::new(
                SETTINGS.with(|s| s.boolean("folders-first")),
            ));
            SETTINGS.with(|s| {
                s.connect_changed(
                    Some("folders-first"),
                    glib::clone!(
                        #[strong]
                        cell,
                        move |s, key| cell.set(s.boolean(key))
                    ),
                )
            });
            cell
        })
        .get()
    })
}

pub fn tree_view() -> bool {
    SETTINGS.with(|s| s.boolean("use-tree-view"))
}

/// The view and sort order of a folder are kept as `metadata::` attributes, which exist
/// only where gvfs runs its metadata backend. Without it nothing can be stored per folder,
/// so the global default takes over instead of the choice quietly going nowhere.
pub fn per_folder_available() -> bool {
    METADATA.with(|a| *a.get_or_init(metadata_writable))
}

thread_local! {
    static METADATA: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
}

/// The question goes to gvfs over the bus, which may have to be started to answer it, so
/// it is asked off the main loop before anything wants the answer. Every later ask is the
/// cached one; only an ask that beats this home does the work itself, on the main loop.
pub fn warm_per_folder_available() {
    glib::spawn_future_local(async {
        if let Ok(writable) = gio::spawn_blocking(metadata_writable).await {
            METADATA.with(|a| {
                let _ = a.set(writable);
            });
        }
    });
}

fn metadata_writable() -> bool {
    gio::File::for_path(glib::home_dir())
        .query_writable_namespaces(gio::Cancellable::NONE)
        .is_ok_and(|list| list.lookup("metadata").is_some())
}

/// Sidebar places that not everyone wants: the top of the filesystem, and the starred
/// files for those who never star anything.
pub fn show_root() -> bool {
    SETTINGS.with(|s| s.boolean("show-root"))
}

pub fn show_favorites() -> bool {
    SETTINGS.with(|s| s.boolean("show-favorites"))
}

/// Colour tags are off until asked for: they add a row of dots to the context menu, a
/// list to the sidebar and a mark to every tagged file, for those who label their files.
pub fn use_tags() -> bool {
    SETTINGS.with(|s| s.boolean("use-tags"))
}

/// Locations on other machines: connecting to a server, the network in the sidebar and
/// shares opened by address. On for those who have any, off for those who have none.
pub fn use_network() -> bool {
    SETTINGS.with(|s| s.boolean("use-network"))
}

/// The column view is off unless it is asked for: it is a third way of reading a folder,
/// not one everybody wants in the view button.
pub fn column_view() -> bool {
    SETTINGS.with(|s| s.boolean("use-column-view"))
}

pub fn remember_view() -> bool {
    SETTINGS.with(|s| s.boolean("remember-view")) && per_folder_available()
}

/// By location, the attributes favorites and each tag keep their view and order in.
type ListViews = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

fn list_views() -> ListViews {
    SETTINGS.with(|s| s.get("list-views"))
}

fn save_list_views(views: ListViews) {
    if let Err(e) = SETTINGS.with(|s| s.set("list-views", views)) {
        glib::g_warning!("spiral", "cannot save the view of a list: {e}");
    }
}

/// What the list at `uri` remembers: `metadata::` attributes and their values, as a
/// folder would have them.
pub fn list_view(uri: &str) -> std::collections::HashMap<String, String> {
    list_views().remove(uri).unwrap_or_default()
}

pub fn set_list_view(uri: &str, attribute: &str, value: &str) {
    let mut views = list_views();
    views
        .entry(uri.to_string())
        .or_default()
        .insert(attribute.to_string(), value.to_string());
    save_list_views(views);
}

/// What the list at `from` remembers goes to `to`, or is forgotten for `None`: a tag
/// renamed, or removed.
pub fn move_list_view(from: &str, to: Option<&str>) {
    let mut views = list_views();
    let Some(view) = views.remove(from) else {
        return;
    };
    if let Some(to) = to {
        views.insert(to.to_string(), view);
    }
    save_list_views(views);
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

/// Pictures past this many bytes are left with their icon. Reading a very large one costs
/// time and memory out of proportion to the thumbnail it makes.
pub fn thumbnail_limit() -> u64 {
    SETTINGS.with(|s| s.uint64("thumbnail-limit")) * 1000 * 1000
}

pub fn recursive_search_for(file: &gio::File) -> bool {
    scope_allows("recursive-search", file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_view_follows_its_tag() {
        set_list_view("tag:///Red", "metadata::spiral-sort", "size-desc");
        set_list_view("tag:///Red", "metadata::spiral-view", "list");
        move_list_view("tag:///Red", Some("tag:///Work"));
        assert!(list_view("tag:///Red").is_empty());
        let view = list_view("tag:///Work");
        assert_eq!(view["metadata::spiral-sort"], "size-desc");
        assert_eq!(view["metadata::spiral-view"], "list");
        move_list_view("tag:///Work", None);
        assert!(list_view("tag:///Work").is_empty());
    }
}
