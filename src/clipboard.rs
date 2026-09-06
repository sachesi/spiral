//! File clipboard using Nautilus' `x-special/gnome-copied-files` plus a `gdk::FileList`,
//! which GDK serialises as `text/uri-list` and as plain-text paths for terminals and editors.

use std::cell::RefCell;
use std::collections::HashSet;

use crate::gtk::prelude::*;
use crate::{gdk, gio, glib};

const GNOME_MIME: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";

thread_local! {
    /// URIs of the files the clipboard currently holds as a cut, so views can dim them.
    static CUT: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

pub fn is_cut(file: &gio::File) -> bool {
    CUT.with(|c| c.borrow().contains(file.uri().as_str()))
}

/// Re-read the clipboard into the cut set. Returns whether the set changed.
pub async fn refresh_cut(clipboard: &gdk::Clipboard) -> bool {
    let mut cut = HashSet::new();
    if has_files(clipboard)
        && let Some((files, true)) = read(clipboard).await
    {
        cut = files.iter().map(|f| f.uri().to_string()).collect();
    }
    CUT.with(|c| {
        let changed = *c.borrow() != cut;
        *c.borrow_mut() = cut;
        changed
    })
}

pub fn set(clipboard: &gdk::Clipboard, files: &[gio::File], cut: bool) {
    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
    let provider = gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_bytes(GNOME_MIME, &glib::Bytes::from_owned(gnome.into_bytes())),
        gdk::ContentProvider::for_value(&gdk::FileList::from_array(files).to_value()),
    ]);
    let _ = clipboard.set_content(Some(&provider));
}

pub fn has_files(clipboard: &gdk::Clipboard) -> bool {
    let f = clipboard.formats();
    f.contain_mime_type(GNOME_MIME) || f.contain_mime_type(URI_LIST)
}

/// True when the clipboard holds an image and no files: a screenshot, say. Other
/// applications offer the image as `image/png` and friends, so ask what GDK can turn
/// those into rather than what is literally on offer.
pub fn has_image(clipboard: &gdk::Clipboard) -> bool {
    !has_files(clipboard)
        && clipboard
            .formats()
            .union_deserialize_types()
            .contains_type(gdk::Texture::static_type())
}

/// Returns (files, cut). `None` if the clipboard holds no file list.
pub async fn read(clipboard: &gdk::Clipboard) -> Option<(Vec<gio::File>, bool)> {
    let (stream, mime) = clipboard
        .read_future(&[GNOME_MIME, URI_LIST], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let mut data = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(65536, glib::Priority::DEFAULT)
            .await
            .ok()?;
        if chunk.is_empty() {
            break;
        }
        data.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let mut cut = false;
    if mime == GNOME_MIME {
        match lines.next() {
            Some("cut") => cut = true,
            Some("copy") => {}
            Some(other) => return Some((vec![gio::File::for_uri(other)], false)),
            None => return None,
        }
    }
    let files: Vec<gio::File> = lines.map(gio::File::for_uri).collect();
    (!files.is_empty()).then_some((files, cut))
}
