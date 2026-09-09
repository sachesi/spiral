//! Helpers over `gio::FileInfo` as produced by `gtk::DirectoryList`.

use std::cmp::Ordering;

use gettextrs::{gettext, ngettext};

use crate::enums::SortKey;
use crate::gio::prelude::*;
use crate::{gio, glib};

/// Attributes requested from `gtk::DirectoryList` for every view.
/// The `thumbnail::` attributes are deliberately not among them: GIO answers those by
/// hashing the name of every file in the folder and looking for the hash in three
/// directories, listed or not, and Spiral asks the same question itself for the rows it
/// actually shows.
pub const ATTRIBUTES: &str = "standard::*,time::modified,time::access,time::created,\
access::can-read,access::can-write,access::can-delete,access::can-trash,\
access::can-rename,access::can-execute,unix::mode,owner::user,owner::group,trash::orig-path,\
metadata::custom-icon,metadata::custom-icon-name,xattr::xdg.tags";

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
        && content_type_of(info).is_some_and(|ct| gio::content_type_can_be_executable(&ct))
}

pub fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

/// Where an entry that stands for somewhere else points: `network:///` lists a server
/// this way and `computer:///` a place on the machine. The entry itself holds nothing;
/// what it points at is the folder.
pub fn target_of(info: &gio::FileInfo) -> Option<gio::File> {
    if !matches!(
        info.file_type(),
        gio::FileType::Shortcut | gio::FileType::Mountable
    ) {
        return None;
    }
    info.attribute_string("standard::target-uri")
        .map(|uri| gio::File::for_uri(&uri))
}

/// The size of `info`, or nothing where the backend gives none: `network:///`, the online
/// accounts and other gvfs locations answer with an info that carries only what they know,
/// and asking one of those for an attribute it has not got is an error in GLib, not a zero.
pub fn size_of(info: &gio::FileInfo) -> u64 {
    if !info.has_attribute("standard::size") {
        return 0;
    }
    info.size().max(0) as u64
}

/// The content type of `info`, where the backend gives one: the same guarded ask, since
/// a location that knows nothing of its entries fills none in.
pub fn content_type_of(info: &gio::FileInfo) -> Option<glib::GString> {
    if !info.has_attribute("standard::content-type") {
        return None;
    }
    info.content_type()
}

/// Whether `info` is one of the files kept out of sight: hidden, or a backup. Asked the
/// same guarded way, for the same reason.
pub fn is_hidden(info: &gio::FileInfo) -> bool {
    (info.has_attribute("standard::is-hidden") && info.is_hidden())
        || (info.has_attribute("standard::is-backup") && info.is_backup())
}

/// Whether `info` points at a location nothing installed here can open: a server listed
/// on the network is reached by the backend for its protocol, and without that backend
/// the entry is a dead end, so it is not listed at all.
pub fn is_unreachable(info: &gio::FileInfo) -> bool {
    target_of(info)
        .and_then(|target| target.uri_scheme())
        .is_some_and(|scheme| !crate::network::supports(&scheme))
}

/// Whether a size would mean anything for `info`. A folder has none worth showing, and
/// neither has an entry that only points somewhere.
fn sizeless(info: &gio::FileInfo) -> bool {
    matches!(
        info.file_type(),
        gio::FileType::Directory | gio::FileType::Shortcut | gio::FileType::Mountable
    )
}

pub fn display_name(info: &gio::FileInfo) -> glib::GString {
    info.display_name()
}

pub fn size_string(info: &gio::FileInfo) -> String {
    if sizeless(info) {
        return String::new();
    }
    crate::prefs::size(size_of(info))
}

pub fn type_string(info: &gio::FileInfo) -> String {
    if is_dir(info) {
        return gettext("Folder");
    }
    content_type_of(info)
        .and_then(|ct| gio::content_type_get_description(&ct).into())
        .map(|d: glib::GString| d.to_string())
        .unwrap_or_default()
}

/// The Type column wants a word, not a sentence: the extension, lowercased, with the
/// full description kept for names that carry none.
pub fn short_type_string(info: &gio::FileInfo) -> String {
    if is_dir(info) {
        return gettext("Folder");
    }
    let name = info.display_name();
    match extension(&name) {
        Some(ext) => ext,
        None => type_string(info),
    }
}

/// The extension of `name`, if it looks like one: after a dot that is not the first
/// character, short, alphanumeric and not a bare number, so `libfoo.so.6` and `page.1`
/// keep their description. Compound archive suffixes keep their `tar`.
fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty()
        || ext.len() > 8
        || !ext.chars().all(char::is_alphanumeric)
        || !ext.chars().any(char::is_alphabetic)
    {
        return None;
    }
    let ext = ext.to_lowercase();
    match stem.rsplit_once('.') {
        Some((_, "tar")) => Some(format!("tar.{ext}")),
        _ => Some(ext),
    }
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
        "size" if !sizeless(info) => Some(glib::format_size(size_of(info)).to_string()),
        "date_modified" => Some(modified_string(info)).filter(|s| !s.is_empty()),
        "permissions" => permissions_string(info),
        "type" => Some(type_string(info)),
        "mime_type" => content_type_of(info).map(|c| c.to_string()),
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
    if file.uri() == crate::network::NETWORK_URI {
        return gettext("Network");
    }
    if crate::starred::is_starred_location(file) {
        return gettext("Favorites");
    }
    if crate::tags::is_tag_location(file) {
        return crate::tags::tag_of_location(file).unwrap_or_else(|| gettext("Tags"));
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
        // The root of a share has no name of its own; the mount it is the root of has.
        _ => match crate::places_sidebar::mount_of(file) {
            Some(mount) => mount.name().to_string(),
            None => file.uri().to_string(),
        },
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
        SortKey::Size => size_of(a).cmp(&size_of(b)).then_with(by_name),
        SortKey::Type => sort_key(a, TYPE_KEY, short_type_string)
            .cmp(&sort_key(b, TYPE_KEY, short_type_string))
            .then_with(by_name),
        SortKey::Modified => a
            .modification_date_time()
            .cmp(&b.modification_date_time())
            .then_with(by_name),
    };
    if reversed { ord.reverse() } else { ord }
}

/// Attributes Spiral keeps on the infos it is handed, holding what a sort would otherwise
/// work out again for every comparison it makes. They are not asked of the filesystem, so
/// nothing but Spiral ever writes them.
const COLLATE_KEY: &str = "spiral::collate";
const TYPE_KEY: &str = "spiral::type";

/// The value `make` gives for this info, computed the first time it is wanted and kept on
/// the info after that: sorting a folder of a hundred thousand files asks for it well over
/// a million times, and collating a name is not cheap.
fn sort_key(
    info: &gio::FileInfo,
    attribute: &str,
    make: impl Fn(&gio::FileInfo) -> String,
) -> glib::GString {
    if let Some(key) = info.attribute_string(attribute) {
        return key;
    }
    let key = make(info);
    info.set_attribute_string(attribute, &key);
    key.into()
}

/// Drop the cached sort keys, for an info whose attributes are being replaced in place.
pub fn forget_sort_keys(info: &gio::FileInfo) {
    info.remove_attribute(COLLATE_KEY);
    info.remove_attribute(TYPE_KEY);
}

fn name_cmp(a: &gio::FileInfo, b: &gio::FileInfo) -> Ordering {
    sort_key(a, COLLATE_KEY, collate).cmp(&sort_key(b, COLLATE_KEY, collate))
}

/// `g_utf8_collate_key_for_filename` as a plain string: the keys it makes compare byte by
/// byte, which is what `glib::FilenameCollationKey` does behind a type that cannot be kept.
fn collate(info: &gio::FileInfo) -> String {
    let name = info.display_name();
    unsafe {
        let key = glib::ffi::g_utf8_collate_key_for_filename(
            name.as_ptr() as *const libc::c_char,
            name.len() as isize,
        );
        glib::translate::from_glib_full::<_, glib::GString>(key).into()
    }
}

/// Whether `name` matches a shell pattern: `*` stands for any run of characters, `?` for
/// exactly one, and everything else for itself. The whole name has to match.
pub fn matches_pattern(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    // A star is remembered rather than recursed on: when what follows it stops matching,
    // the star takes one more character and the comparison goes on from there.
    let (mut i, mut j) = (0, 0);
    let (mut star, mut taken) = (None, 0);
    while i < name.len() {
        if j < pattern.len() && (pattern[j] == '?' || pattern[j] == name[i]) {
            i += 1;
            j += 1;
        } else if j < pattern.len() && pattern[j] == '*' {
            star = Some(j);
            j += 1;
            taken = i;
        } else if let Some(s) = star {
            taken += 1;
            i = taken;
            j = s + 1;
        } else {
            return false;
        }
    }
    pattern[j..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointing_at(uri: &str) -> gio::FileInfo {
        let info = gio::FileInfo::new();
        info.set_file_type(gio::FileType::Mountable);
        info.set_attribute_string("standard::target-uri", uri);
        info
    }

    /// An entry of `network:///` or `computer:///` is worth listing only where what it
    /// points at can be opened. Local files always can; a protocol with no backend on
    /// this system cannot, and in a test there are no backends at all.
    #[test]
    fn an_entry_pointing_nowhere_openable_is_unreachable() {
        assert!(!is_unreachable(&pointing_at("file:///srv")));
        assert!(is_unreachable(&pointing_at("afp://server/share")));
        // A file of its own points at nothing and is never in question.
        let plain = gio::FileInfo::new();
        plain.set_file_type(gio::FileType::Regular);
        assert!(!is_unreachable(&plain));
    }

    fn named(name: &str) -> gio::FileInfo {
        let info = gio::FileInfo::new();
        info.set_display_name(name);
        info
    }

    /// The cached key has to order names the way `FilenameCollationKey` does, and has to
    /// go on ordering them that way once it has been stored on the info.
    #[test]
    fn collation_keys_match_glib() {
        let names = [
            "b.txt",
            "A.txt",
            "a.txt",
            "10.txt",
            "9.txt",
            "\u{e4}.txt",
            "z",
        ];
        for a in names {
            for b in names {
                let want =
                    glib::FilenameCollationKey::from(a).cmp(&glib::FilenameCollationKey::from(b));
                let (ia, ib) = (named(a), named(b));
                assert_eq!(name_cmp(&ia, &ib), want, "{a} vs {b}");
                // Again, now that both infos carry the key.
                assert_eq!(name_cmp(&ia, &ib), want, "{a} vs {b}, cached");
            }
        }
    }

    #[test]
    fn patterns() {
        for (name, pattern, want) in [
            ("photo.png", "*.png", true),
            ("photo.png", "*.jpg", false),
            ("file01.txt", "file??.txt", true),
            ("file1.txt", "file??.txt", false),
            ("pict.001", "pict*.???", true),
            ("anything", "*", true),
            ("", "*", true),
            ("plain", "plain", true),
            ("plain", "plai", false),
            ("a.tar.gz", "*.gz", true),
            ("\u{e4}pfel.txt", "?pfel.txt", true),
        ] {
            assert_eq!(matches_pattern(name, pattern), want, "{name} vs {pattern}");
        }
    }

    #[test]
    fn extensions() {
        for (name, want) in [
            ("banana.txt", Some("txt")),
            ("Photo.JPG", Some("jpg")),
            ("archive.tar.gz", Some("tar.gz")),
            ("no-extension", None),
            (".bashrc", None),
            ("trailing.", None),
            ("libfoo.so.6", None),
            ("page.1", None),
            ("long.extensionhere", None),
        ] {
            assert_eq!(
                super::extension(name).as_deref(),
                want,
                "extension of {name}"
            );
        }
    }
}
