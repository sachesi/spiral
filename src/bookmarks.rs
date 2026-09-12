//! The GTK bookmarks file (~/.config/gtk-3.0/bookmarks): "uri [label]" per line, shared with
//! the GTK file chooser and other file managers.

use std::path::PathBuf;

use crate::gio::prelude::*;
use crate::{gio, glib};

pub type Entry = (gio::File, Option<String>);

pub fn path() -> PathBuf {
    glib::user_config_dir().join("gtk-3.0").join("bookmarks")
}

thread_local! {
    /// The file as it was last read. Every change of the selection asks whether the folder
    /// is bookmarked, and the answer is not worth a read of the disk each time; the sidebar
    /// watches the file and forgets this when it changes underneath.
    static CACHE: std::cell::RefCell<Option<Vec<Entry>>> = const { std::cell::RefCell::new(None) };
}

/// Read the file again the next time it is asked for.
pub fn forget() {
    CACHE.with(|c| c.replace(None));
}

pub fn load() -> Vec<Entry> {
    if let Some(entries) = CACHE.with(|c| c.borrow().clone()) {
        return entries;
    }
    let entries = read();
    CACHE.with(|c| c.replace(Some(entries.clone())));
    entries
}

fn read() -> Vec<Entry> {
    std::fs::read_to_string(path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            let (uri, label) = match l.split_once(' ') {
                Some((u, lab)) => (u, Some(lab.trim().to_string())),
                None => (l, None),
            };
            Some((gio::File::for_uri(uri), label))
        })
        .collect()
}

fn save(entries: &[Entry]) {
    let text: String = entries
        .iter()
        .map(|(f, label)| match label {
            Some(l) => format!("{} {l}\n", f.uri()),
            None => format!("{}\n", f.uri()),
        })
        .collect();
    let p = path();
    let _ = p.parent().map(std::fs::create_dir_all);
    if let Err(e) = std::fs::write(&p, text) {
        glib::g_warning!("spiral", "cannot save bookmarks: {e}");
    }
    CACHE.with(|c| c.replace(Some(entries.to_vec())));
}

pub fn contains(file: &gio::File) -> bool {
    load().iter().any(|(f, _)| f.equal(file))
}

pub fn add(file: &gio::File) {
    let mut entries = load();
    if entries.iter().any(|(f, _)| f.equal(file)) {
        return;
    }
    entries.push((file.clone(), None));
    save(&entries);
}

pub fn remove(file: &gio::File) {
    let mut entries = load();
    entries.retain(|(f, _)| !f.equal(file));
    save(&entries);
}

/// Set or clear (empty) the label shown for `file`.
pub fn rename(file: &gio::File, label: &str) {
    let mut entries = load();
    for (f, l) in &mut entries {
        if f.equal(file) {
            *l = Some(label.trim().to_string()).filter(|s| !s.is_empty());
        }
    }
    save(&entries);
}

/// Move `file` next to `anchor` (before it, or after when `after`), or to the end.
pub fn move_to(file: &gio::File, anchor: Option<(&gio::File, bool)>) {
    // Dropped on itself: it stays where it is.
    if anchor.is_some_and(|(a, _)| a.equal(file)) {
        return;
    }
    let mut entries = load();
    let Some(pos) = entries.iter().position(|(f, _)| f.equal(file)) else {
        return;
    };
    let entry = entries.remove(pos);
    let at = anchor
        .and_then(|(a, after)| {
            entries
                .iter()
                .position(|(f, _)| f.equal(a))
                .map(|i| if after { i + 1 } else { i })
        })
        .unwrap_or(entries.len());
    entries.insert(at, entry);
    save(&entries);
}
