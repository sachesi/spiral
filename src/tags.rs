//! Colour tags. A file's tags are the names in its `user.xdg.tags` extended attribute,
//! the freedesktop one Dolphin and Baloo read as well, so they stay with the file through
//! a rename, a move or a copy, and other programs see them. Which files carry a tag is
//! kept in an index under the user data dir, one `tag<TAB>uri` line per pair, since asking
//! every file on the disk is not an option; an entry that has stopped being true is
//! dropped when the tag is listed. The tags themselves, name and colour each, are the
//! `tags` setting, with the seven colours standing in while it is empty.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gettextrs::gettext;
use glib::translate::*;

use crate::gtk::prelude::*;
use crate::{gio, glib, gtk};

/// The extended attribute, as GIO names it.
pub const ATTRIBUTE: &str = "xattr::xdg.tags";

/// The colours a tag can have, in the order the pickers show them.
pub const COLORS: [&str; 7] = ["red", "orange", "yellow", "green", "blue", "purple", "gray"];

/// A colour's name, which is also the name of the tag it stands for out of the box.
pub fn color_name(color: &str) -> String {
    match color {
        "red" => gettext("Red"),
        "orange" => gettext("Orange"),
        "yellow" => gettext("Yellow"),
        "green" => gettext("Green"),
        "blue" => gettext("Blue"),
        "purple" => gettext("Purple"),
        "gray" => gettext("Grey"),
        _ => gettext("None"),
    }
}

/// Whether tags are on offer at all: the preference, off until asked for.
pub fn enabled() -> bool {
    crate::prefs::use_tags()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// One of `COLORS`, or empty for a tag with a name and no colour.
    pub color: String,
}

thread_local! {
    /// The `tags` setting, parsed once per change: a cell asks for it on every bind.
    static ALL: RefCell<Option<Rc<Vec<Tag>>>> = const { RefCell::new(None) };
    static WATCHED: Cell<bool> = const { Cell::new(false) };
}

/// The tags on offer: the setting, or the seven colours while it is empty.
pub fn all() -> Rc<Vec<Tag>> {
    if !WATCHED.replace(true) {
        crate::prefs::settings().connect_changed(Some("tags"), |_, _| {
            ALL.with(|a| a.replace(None));
        });
    }
    ALL.with(|a| {
        a.borrow_mut()
            .get_or_insert_with(|| Rc::new(read_setting()))
            .clone()
    })
}

fn read_setting() -> Vec<Tag> {
    let stored: Vec<(String, String)> = crate::prefs::settings()
        .value("tags")
        .get()
        .unwrap_or_default();
    if stored.is_empty() {
        return COLORS
            .iter()
            .map(|c| Tag {
                name: color_name(c),
                color: c.to_string(),
            })
            .collect();
    }
    stored
        .into_iter()
        .map(|(name, color)| Tag { name, color })
        .collect()
}

fn save_all(tags: &[Tag]) {
    let value: Vec<(String, String)> = tags
        .iter()
        .map(|t| (t.name.clone(), t.color.clone()))
        .collect();
    let _ = crate::prefs::settings().set_value("tags", &value.to_variant());
}

pub fn color_of(name: &str) -> Option<String> {
    all()
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.color.clone())
}

pub fn exists(name: &str) -> bool {
    all().iter().any(|t| t.name == name)
}

/// Whether `name` can be a tag: something to read, and no comma, which is what separates
/// the tags in the attribute.
pub fn valid_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && !name.contains(',')
}

pub fn add(tag: Tag) {
    let mut tags = (*all()).clone();
    tags.push(tag);
    save_all(&tags);
}

pub fn set_color(name: &str, color: &str) {
    let mut tags = (*all()).clone();
    if let Some(t) = tags.iter_mut().find(|t| t.name == name) {
        t.color = color.to_string();
        save_all(&tags);
    }
}

/// Give the tag a new name, on every file known to carry it as well as in the setting.
/// The files are written off the main loop: there may be many, and some on a share.
pub fn rename(old: &str, new: &str) {
    let (old, new) = (old.to_string(), new.to_string());
    let files = files_with(Some(&old));
    glib::spawn_future_local(async move {
        let (o, n) = (old.clone(), new.clone());
        let _ = gio::spawn_blocking(move || {
            for file in files {
                let mut names = read(&file);
                if let Some(name) = names.iter_mut().find(|name| **name == o) {
                    *name = n.clone();
                    let _ = write(&file, &names);
                }
            }
        })
        .await;
        // The setting first: a cell bound with an info from before the change would
        // put the old name back in the index while it still counted as a tag.
        let mut tags = (*all()).clone();
        if let Some(t) = tags.iter_mut().find(|t| t.name == old) {
            t.name = new.clone();
            save_all(&tags);
        }
        rewrite_index(|tag, uri| Some((if tag == old { new.clone() } else { tag }, uri)));
    });
}

/// Take the tag off every file known to carry it, then out of the setting.
pub fn remove(name: &str) {
    let name = name.to_string();
    let files = files_with(Some(&name));
    glib::spawn_future_local(async move {
        let n = name.clone();
        let _ = gio::spawn_blocking(move || {
            for file in files {
                let mut names = read(&file);
                if names.contains(&n) {
                    names.retain(|name| *name != n);
                    let _ = write(&file, &names);
                }
            }
        })
        .await;
        let mut tags = (*all()).clone();
        tags.retain(|t| t.name != name);
        save_all(&tags);
        rewrite_index(|tag, uri| (tag != name).then_some((tag, uri)));
    });
}

// The `tag:///` locations: the tag's files, and every tagged file at the root.

pub fn is_tag_location(file: &gio::File) -> bool {
    file.uri().starts_with("tag:")
}

/// `tag:///Name`, with the name escaped the way a URI wants it. Dummy files compare as
/// strings, so every tag location is made here.
pub fn location(name: &str) -> gio::File {
    gio::File::for_uri(&format!(
        "tag:///{}",
        glib::Uri::escape_string(name, None, true)
    ))
}

/// The tag a `tag:///` location lists; `None` for the root, which lists them all.
pub fn tag_of_location(file: &gio::File) -> Option<String> {
    let uri = file.uri();
    let rest = uri.strip_prefix("tag:///")?;
    if rest.is_empty() {
        return None;
    }
    glib::Uri::unescape_string(rest, None)
        .map(|s| s.to_string())
        .or_else(|| Some(rest.to_string()))
}

// The attribute on the file.

/// The tags in an info that was asked for `ATTRIBUTE`.
pub fn of_info(info: &gio::FileInfo) -> Vec<String> {
    info.attribute_string(ATTRIBUTE)
        .map(|raw| parse(&unescape(&raw)))
        .unwrap_or_default()
}

fn parse(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// GIO hands an attribute out with every byte outside printable ASCII, and the backslash,
/// as `\xNN`, and takes `\xNN` back the same way when the attribute is set.
fn unescape(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1] == b'x'
            && let Some(h) = (bytes[i + 2] as char).to_digit(16)
            && let Some(l) = (bytes[i + 3] as char).to_digit(16)
        {
            out.push((h * 16 + l) as u8);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if (32..=126).contains(&b) && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// The tags on `file` now, asked of the file: a listing's info may be a moment old.
pub fn read(file: &gio::File) -> Vec<String> {
    file.query_info(
        ATTRIBUTE,
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )
    .map(|info| of_info(&info))
    .unwrap_or_default()
}

/// Write the whole attribute, or take it off the file for an empty list.
pub fn write(file: &gio::File, names: &[String]) -> Result<(), glib::Error> {
    if names.is_empty() {
        return remove_attribute(file);
    }
    file.set_attribute_string(
        ATTRIBUTE,
        &escape(&names.join(",")),
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )
}

/// Setting an attribute to no type at all is how GIO removes an extended attribute; the
/// binding leaves that call out.
fn remove_attribute(file: &gio::File) -> Result<(), glib::Error> {
    unsafe {
        let mut error = std::ptr::null_mut();
        gio::ffi::g_file_set_attribute(
            file.to_glib_none().0,
            ATTRIBUTE.to_glib_none().0,
            gio::ffi::G_FILE_ATTRIBUTE_TYPE_INVALID,
            std::ptr::null_mut(),
            gio::ffi::G_FILE_QUERY_INFO_NONE,
            std::ptr::null_mut(),
            &mut error,
        );
        if error.is_null() {
            Ok(())
        } else {
            Err(from_glib_full(error))
        }
    }
}

/// Put `name` on `file` or take it off, on the file and in the index. A file that already
/// stands as asked is left alone, but for the index, which may not have heard of it.
pub fn set(file: &gio::File, name: &str, on: bool) -> Result<(), glib::Error> {
    let mut names = read(file);
    if names.iter().any(|n| n == name) != on {
        if on {
            names.push(name.to_string());
        } else {
            names.retain(|n| n != name);
        }
        write(file, &names)?;
    }
    if on {
        note(file, std::slice::from_ref(&name.to_string()));
    } else {
        forget(name, file);
    }
    Ok(())
}

/// The value the attribute has after `set`, for putting into an info by hand.
pub fn attribute_value(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| escape(&names.join(",")))
}

// The index: `tag<TAB>uri` lines, shared as a `gtk::StringList` so a view listing a tag
// can follow `items-changed`, the way Favorites follow theirs.

fn index_path() -> PathBuf {
    glib::user_data_dir().join("spiral").join("tags")
}

thread_local! {
    static INDEX: gtk::StringList = {
        let text = std::fs::read_to_string(index_path()).unwrap_or_default();
        gtk::StringList::new(
            &text
                .lines()
                .filter(|l| l.contains('\t'))
                .collect::<Vec<_>>(),
        )
    };
    static SAVE_QUEUED: Cell<bool> = const { Cell::new(false) };
}

pub fn index() -> gtk::StringList {
    INDEX.with(|l| l.clone())
}

fn line(tag: &str, uri: &str) -> String {
    format!("{tag}\t{uri}")
}

fn entries() -> Vec<(String, String)> {
    let list = index();
    (0..list.n_items())
        .filter_map(|i| list.string(i))
        .filter_map(|l| {
            l.split_once('\t')
                .map(|(t, u)| (t.to_string(), u.to_string()))
        })
        .collect()
}

/// The files known to carry `tag`, or, for `None`, to carry any tag, each once.
pub fn files_with(tag: Option<&str>) -> Vec<gio::File> {
    let mut seen = std::collections::HashSet::new();
    entries()
        .into_iter()
        .filter(|(t, _)| tag.is_none_or(|tag| t == tag))
        .filter(|(_, uri)| seen.insert(uri.clone()))
        .map(|(_, uri)| gio::File::for_uri(&uri))
        .collect()
}

/// The tags the index has `file` down for.
pub fn indexed(file: &gio::File) -> Vec<String> {
    let uri = file.uri();
    entries()
        .into_iter()
        .filter(|(_, u)| *u == uri)
        .map(|(t, _)| t)
        .collect()
}

/// Make sure the index knows `file` carries `names`: a listing has just seen it. That is
/// how a file tagged elsewhere, or moved by something other than Spiral, finds its way in.
/// Only tags on offer are kept: a name another program made up is the file's to show, and
/// one just renamed or removed here must not come back through a cell bound a moment
/// before.
pub fn note(file: &gio::File, names: &[String]) {
    let list = index();
    let uri = file.uri();
    let mut added = false;
    for name in names.iter().filter(|n| exists(n)) {
        let l = line(name, &uri);
        if list.find(&l) == gtk::INVALID_LIST_POSITION {
            list.append(&l);
            added = true;
        }
    }
    if added {
        schedule_save();
    }
}

pub fn forget(name: &str, file: &gio::File) {
    let list = index();
    let pos = list.find(&line(name, &file.uri()));
    if pos != gtk::INVALID_LIST_POSITION {
        list.remove(pos);
        schedule_save();
    }
}

/// `from` was moved or renamed to `to`, and so was everything below it.
pub fn relocate(from: &gio::File, to: &gio::File) {
    let (from, to) = (from.uri(), to.uri());
    let below = format!("{from}/");
    if !entries()
        .iter()
        .any(|(_, u)| *u == from || u.starts_with(&below))
    {
        return;
    }
    rewrite_index(|tag, uri| {
        let uri = if uri == from {
            to.to_string()
        } else if let Some(rest) = uri.strip_prefix(&below) {
            format!("{to}/{rest}")
        } else {
            uri
        };
        Some((tag, uri))
    });
}

/// Replace the index with what `f` makes of each entry, in one change.
fn rewrite_index(f: impl Fn(String, String) -> Option<(String, String)>) {
    let lines: Vec<String> = entries()
        .into_iter()
        .filter_map(|(t, u)| f(t, u))
        .map(|(t, u)| line(&t, &u))
        .collect();
    let list = index();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    list.splice(0, list.n_items(), &refs);
    schedule_save();
}

/// A listing notes its tagged files one by one; the file is written once they are in.
fn schedule_save() {
    if SAVE_QUEUED.replace(true) {
        return;
    }
    glib::idle_add_local_once(|| {
        SAVE_QUEUED.set(false);
        let list = index();
        let text: String = (0..list.n_items())
            .filter_map(|i| list.string(i))
            .map(|s| format!("{s}\n"))
            .collect();
        let p = index_path();
        let _ = p.parent().map(std::fs::create_dir_all);
        if let Err(e) = std::fs::write(&p, text) {
            glib::g_warning!("spiral", "cannot save tag index: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_escaping_round_trips() {
        let names = vec!["Grün".to_string(), "back\\slash".to_string()];
        let raw = escape(&names.join(","));
        assert_eq!(raw, "Gr\\xc3\\xbcn,back\\x5cslash");
        assert_eq!(parse(&unescape(&raw)), names);
    }

    #[test]
    fn parse_skips_empty_fields() {
        assert_eq!(parse(" Red, ,Blue,"), vec!["Red", "Blue"]);
    }
}
