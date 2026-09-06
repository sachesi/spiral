//! Helpers over `gio::FileInfo` as produced by `gtk::DirectoryList`.

use std::cmp::Ordering;

use gettextrs::gettext;

use crate::enums::SortKey;
use crate::gio::prelude::*;
use crate::{gio, glib};

/// Attributes requested from `gtk::DirectoryList` for every view.
pub const ATTRIBUTES: &str = "standard::*,time::modified,thumbnail::path,thumbnail::is-valid,\
thumbnail::failed,access::can-write,access::can-delete,access::can-rename,unix::mode,owner::user,\
owner::group,metadata::custom-icon,metadata::custom-icon-name";

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
        d if d < 7 => gettext("%d days ago").replace("%d", &n(d)),
        d if d < 14 => gettext("Last week"),
        d if d < 31 => gettext("%d weeks ago").replace("%d", &n(d / 7)),
        d if d < 61 => gettext("Last month"),
        d if d < 365 => gettext("%d months ago").replace("%d", &n((d as f64 / 30.4) as i64)),
        d if d < 730 => gettext("Last year"),
        d => gettext("%d years ago").replace("%d", &n((d as f64 / 365.25) as i64)),
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
        1 => gettext("1 item"),
        n => gettext("%d items").replace("%d", &n.to_string()),
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
