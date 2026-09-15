//! Starred files: one URI per line under the user data dir, kept in [`Lines`] so views
//! can follow a change.

use std::path::PathBuf;

use crate::gio::prelude::*;
use crate::lines::{Lines, WatchId};
use crate::{gio, glib};

pub const URI: &str = "starred:///";

pub fn is_starred_location(file: &gio::File) -> bool {
    file.uri().starts_with("starred:")
}

fn path() -> PathBuf {
    glib::user_data_dir().join("spiral").join("starred")
}

thread_local! {
    static LIST: Lines = {
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        Lines::new(text.lines().filter(|l| !l.is_empty()).map(String::from).collect())
    };
}

/// Call `f` whenever the starred files change.
pub fn watch(f: impl Fn() + 'static) -> WatchId {
    LIST.with(|l| l.watch(f))
}

pub fn unwatch(id: WatchId) {
    LIST.with(|l| l.unwatch(id));
}

pub fn files() -> Vec<gio::File> {
    LIST.with(|l| l.to_vec())
        .iter()
        .map(|u| gio::File::for_uri(u))
        .collect()
}

pub fn is_starred(file: &gio::File) -> bool {
    LIST.with(|l| l.contains(&file.uri()))
}

pub fn set_starred(file: &gio::File, starred: bool) {
    let uri = file.uri().to_string();
    LIST.with(|list| {
        match (list.position(&uri), starred) {
            (None, true) => list.push(uri),
            (None, false) | (Some(_), true) => return,
            (Some(i), false) => list.remove(i),
        }
        save(list);
    });
}

/// `file` was trashed or deleted, and so was everything below it: none of it stays
/// starred, and a restore does not star it again. One change, so Favorites loads once.
pub fn forget_all(file: &gio::File) {
    let uri = file.uri().to_string();
    let below = format!("{uri}/");
    LIST.with(|list| {
        let kept: Vec<String> = list
            .to_vec()
            .into_iter()
            .filter(|u| *u != uri && !u.starts_with(&below))
            .collect();
        if kept.len() == list.len() {
            return;
        }
        list.replace(kept);
        save(list);
    });
}

/// `from` was moved or renamed to `to`, and so was everything below it: the stars go
/// with them.
pub fn relocate(from: &gio::File, to: &gio::File) {
    let (from, to) = (from.uri(), to.uri());
    let below = format!("{from}/");
    LIST.with(|list| {
        let lines = list.to_vec();
        if !lines.iter().any(|u| *u == from || u.starts_with(&below)) {
            return;
        }
        let moved = lines
            .into_iter()
            .map(|u| match u.strip_prefix(&below) {
                _ if u == from => to.to_string(),
                Some(rest) => format!("{to}/{rest}"),
                None => u,
            })
            .collect();
        list.replace(moved);
        save(list);
    });
}

fn save(list: &Lines) {
    let text: String = list.to_vec().iter().map(|s| format!("{s}\n")).collect();
    let p = path();
    let _ = p.parent().map(std::fs::create_dir_all);
    if let Err(e) = std::fs::write(&p, text) {
        glib::g_warning!("spiral", "cannot save starred list: {e}");
    }
}
