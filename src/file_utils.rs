//! Helpers over `gio::FileInfo` as produced by `gtk::DirectoryList`.

use std::cmp::Ordering;

use gettextrs::{gettext, ngettext};

use crate::enums::SortKey;
use crate::gio::prelude::*;
use crate::{gio, glib};

/// Attributes requested from `gtk::DirectoryList` for every view.
pub const ATTRIBUTES: &str = "standard::*,time::modified,time::access,time::created,\
thumbnail::path,thumbnail::is-valid,\
thumbnail::failed,access::can-read,access::can-write,access::can-delete,access::can-trash,\
access::can-rename,access::can-execute,unix::mode,owner::user,owner::group,trash::orig-path,\
metadata::custom-icon,metadata::custom-icon-name";

/// Icon to draw for `info`, honouring the Nautilus-compatible custom icon metadata.
pub fn icon_of(info: &gio::FileInfo) -> gio::Icon {
    if let Some(custom) = info.attribute_string("metadata::custom-icon") {
        let file = if custom.starts_with('/') {
            gio::File::for_path(custom.as_str())
        } else {
            gio::File::for_uri(&custom)
        };
        return gio::FileIcon::new(&file).upcast();
    }
    if let Some(name) = info.attribute_string("metadata::custom-icon-name") {
        return gio::ThemedIcon::new(&name).upcast();
    }
    info.icon()
        .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast())
}

/// The `gio::File` a `DirectoryList` attaches to each info.
pub fn file_of(info: &gio::FileInfo) -> gio::File {
    info.attribute_object("standard::file")
        .and_downcast::<gio::File>()
        .expect("FileInfo without standard::file")
}

/// An `access::` permission; missing (backends that do not report it) counts as allowed.
pub fn allows(info: &gio::FileInfo, attribute: &str) -> bool {
    !info.has_attribute(attribute) || info.boolean(attribute)
}

/// Lock emblem rule from Nautilus: unreadable files always; read-only files only when
/// the folder around them is writable (so a read-only tree is not a wall of locks) and
/// never in the trash, where everything is read-only.
pub fn is_locked(info: &gio::FileInfo, folder_writable: bool) -> bool {
    if !allows(info, "access::can-read") {
        return true;
    }
    folder_writable
        && !allows(info, "access::can-write")
        && !file_of(info).uri().starts_with("trash:")
}

/// A file "Run as a Program" can start: executable bit set and a type that is a program.
pub fn is_program(info: &gio::FileInfo) -> bool {
    !is_dir(info)
        && info.has_attribute("access::can-execute")
        && info.boolean("access::can-execute")
        && info
            .content_type()
            .is_some_and(|ct| gio::content_type_can_be_executable(&ct))
}

pub fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

pub fn display_name(info: &gio::FileInfo) -> glib::GString {
    info.display_name()
}

pub fn size_string(info: &gio::FileInfo) -> String {
    if is_dir(info) {
        return String::new();
    }
    crate::prefs::size(info.size() as u64)
}

pub fn type_string(info: &gio::FileInfo) -> String {
    if is_dir(info) {
        return gettext("Folder");
    }
    info.content_type()
        .and_then(|ct| gio::content_type_get_description(&ct).into())
        .map(|d: glib::GString| d.to_string())
        .unwrap_or_default()
}

pub fn modified_string(info: &gio::FileInfo) -> String {
    info.modification_date_time()
        .and_then(|d| d.to_local().ok())
        .map(|d| crate::prefs::date(&d))
        .unwrap_or_default()
}

pub fn accessed_string(info: &gio::FileInfo) -> String {
    info.access_date_time()
        .and_then(|d| d.to_local().ok())
        .map(|d| crate::prefs::date(&d))
        .unwrap_or_default()
}

pub fn created_string(info: &gio::FileInfo) -> String {
    info.creation_date_time()
        .and_then(|d| d.to_local().ok())
        .map(|d| crate::prefs::date(&d))
        .unwrap_or_default()
}

/// Columns the list view can show besides the name, in display order: key and title.
pub fn optional_columns() -> [(&'static str, String); 8] {
    [
        ("size", gettext("Size")),
        ("type", gettext("Type")),
        ("modified", gettext("Modified")),
        ("accessed", gettext("Accessed")),
        ("created", gettext("Created")),
        ("owner", gettext("Owner")),
        ("group", gettext("Group")),
        ("permissions", gettext("Permissions")),
    ]
}

/// Caption kinds for the grid view: key and title.
pub fn caption_kinds() -> [(&'static str, String); 8] {
    [
        ("none", gettext("None")),
        ("size", gettext("Size")),
        ("date_modified", gettext("Date Modified")),
        ("type", gettext("Type")),
        ("mime_type", gettext("MIME Type")),
        ("permissions", gettext("Permissions")),
        ("owner", gettext("Owner")),
        ("group", gettext("Group")),
    ]
}

/// Nautilus-style relative date, for example "Today, 17:41", "Yesterday, 09:00", "3 days ago",
/// "Last month", "2 years ago".
pub fn relative_date(dt: &glib::DateTime) -> String {
    let Ok(now) = glib::DateTime::now_local() else {
        return String::new();
    };
    let midnight = |d: &glib::DateTime| {
        glib::DateTime::from_local(d.year(), d.month(), d.day_of_month(), 0, 0, 0.0).ok()
    };
    let (Some(today), Some(day)) = (midnight(&now), midnight(dt)) else {
        return String::new();
    };
    let days = today.difference(&day).as_days();
    let time = dt
        .format("%H:%M")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let n = |v: i64| v.to_string();
    match days {
        d if d < 0 => dt.format("%x").map(|s| s.to_string()).unwrap_or_default(),
        0 => gettext("Today, %s").replace("%s", &time),
        1 => gettext("Yesterday, %s").replace("%s", &time),
        d if d < 7 => ngettext("%d day ago", "%d days ago", d as u32).replace("%d", &n(d)),
        d if d < 14 => gettext("Last week"),
        d if d < 31 => {
            ngettext("%d week ago", "%d weeks ago", (d / 7) as u32).replace("%d", &n(d / 7))
        }
        d if d < 61 => gettext("Last month"),
        d if d < 365 => {
            let m = (d as f64 / 30.4) as i64;
            ngettext("%d month ago", "%d months ago", m as u32).replace("%d", &n(m))
        }
        d if d < 730 => gettext("Last year"),
        d => {
            let y = (d as f64 / 365.25) as i64;
            ngettext("%d year ago", "%d years ago", y as u32).replace("%d", &n(y))
        }
    }
}

/// "drwxr-xr-x" style permissions from `unix::mode`.
pub fn permissions_string(info: &gio::FileInfo) -> Option<String> {
    if !info.has_attribute("unix::mode") {
        return None;
    }
    let mode = info.attribute_uint32("unix::mode");
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut out = String::with_capacity(10);
    out.push(kind);
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (i, (bit, ch)) in bits.iter().enumerate() {
        let set = mode & bit != 0;
        let special = match i {
            2 => mode & 0o4000 != 0,
            5 => mode & 0o2000 != 0,
            8 => mode & 0o1000 != 0,
            _ => false,
        };
        out.push(match (set, special, i) {
            (true, true, 8) => 't',
            (false, true, 8) => 'T',
            (true, true, _) => 's',
            (false, true, _) => 'S',
            (true, false, _) => *ch,
            (false, false, _) => '-',
        });
    }
    Some(out)
}

/// Text for a grid caption. Folder item counts are produced asynchronously by the caller.
pub fn caption(info: &gio::FileInfo, kind: &str) -> Option<String> {
    match kind {
        "size" if !is_dir(info) => Some(glib::format_size(info.size() as u64).to_string()),
        "date_modified" => Some(modified_string(info)).filter(|s| !s.is_empty()),
        "permissions" => permissions_string(info),
        "type" => Some(type_string(info)),
        "mime_type" => info.content_type().map(|c| c.to_string()),
        "owner" => info.attribute_string("owner::user").map(|s| s.to_string()),
        "group" => info.attribute_string("owner::group").map(|s| s.to_string()),
        _ => None,
    }
}

pub fn items_string(n: u64) -> String {
    match n {
        0 => gettext("Empty"),
        n => ngettext("%d item", "%d items", n as u32).replace("%d", &n.to_string()),
    }
}

/// Folder holding `info`, shortened with "~" under home, for search results.
pub fn location_of(info: &gio::FileInfo) -> String {
    let Some(parent) = file_of(info).parent() else {
        return String::new();
    };
    let home = gio::File::for_path(glib::home_dir());
    if parent.equal(&home) {
        return "~".into();
    }
    if let Some(rel) = home.relative_path(&parent) {
        return format!("~/{}", rel.to_string_lossy());
    }
    match parent.path() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => parent.uri().to_string(),
    }
}

/// Human-friendly name for a location, used for tabs, crumbs and titles.
pub fn location_name(file: &gio::File) -> String {
    if let Some(home) = glib::home_dir().to_str().map(gio::File::for_path)
        && file.equal(&home)
    {
        return gettext("Home");
    }
    if file.uri() == "trash:///" {
        return gettext("Trash");
    }
    if crate::starred::is_starred_location(file) {
        return gettext("Favorites");
    }
    if let Some(path) = file.path()
        && path.as_os_str() == "/"
    {
        return gettext("System");
    }
    match file.basename() {
        Some(b) if !b.as_os_str().is_empty() && b.as_os_str() != "/" => {
            b.to_string_lossy().into_owned()
        }
        _ => file.uri().to_string(),
    }
}

/// Directories first (unless turned off), then by key, then by name as a tiebreaker.
pub fn compare(a: &gio::FileInfo, b: &gio::FileInfo, key: SortKey, reversed: bool) -> Ordering {
    if crate::prefs::folders_first() {
        match (is_dir(a), is_dir(b)) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }
    let by_name = || name_cmp(a, b);
    let ord = match key {
        SortKey::Name => by_name(),
        SortKey::Size => a.size().cmp(&b.size()).then_with(by_name),
        SortKey::Type => type_string(a).cmp(&type_string(b)).then_with(by_name),
        SortKey::Modified => a
            .modification_date_time()
            .cmp(&b.modification_date_time())
            .then_with(by_name),
    };
    if reversed { ord.reverse() } else { ord }
}

fn name_cmp(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    let ka = glib::FilenameCollationKey::from(a.display_name());
    let kb = glib::FilenameCollationKey::from(b.display_name());
    ka.cmp(&kb)
}
