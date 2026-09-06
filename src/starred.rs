//! Starred files: one URI per line under the user data dir, shared as a `gtk::StringList`
//! so views can follow `items-changed`.

use std::path::PathBuf;

use crate::gtk::prelude::*;
use crate::{gio, glib, gtk};

pub const URI: &str = "starred:///";

pub fn is_starred_location(file: &gio::File) -> bool {
    file.uri().starts_with("starred:")
}

fn path() -> PathBuf {
    glib::user_data_dir().join("spiral").join("starred")
}

thread_local! {
    static LIST: gtk::StringList = {
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        gtk::StringList::new(&text.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>())
    };
}

/// The shared list of starred URIs.
pub fn list() -> gtk::StringList {
    LIST.with(|l| l.clone())
}

pub fn files() -> Vec<gio::File> {
    let list = list();
    (0..list.n_items())
        .filter_map(|i| list.string(i))
        .map(|u| gio::File::for_uri(&u))
        .collect()
}

pub fn is_starred(file: &gio::File) -> bool {
    list().find(&file.uri()) != gtk::INVALID_LIST_POSITION
}

pub fn set_starred(file: &gio::File, starred: bool) {
    let list = list();
    let uri = file.uri();
    match (list.find(&uri), starred) {
        (gtk::INVALID_LIST_POSITION, true) => list.append(&uri),
        (gtk::INVALID_LIST_POSITION, false) => return,
        (_, true) => return,
        (i, false) => list.remove(i),
    }
    let text: String = (0..list.n_items())
        .filter_map(|i| list.string(i))
        .map(|s| format!("{s}\n"))
        .collect();
    let p = path();
    let _ = p.parent().map(std::fs::create_dir_all);
    if let Err(e) = std::fs::write(&p, text) {
        glib::g_warning!("spiral", "cannot save starred list: {e}");
    }
}
