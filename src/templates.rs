//! The XDG Templates folder, which fills the "New Document" menu: every file in it is a
//! document to start from, every folder in it a submenu.

use std::cell::RefCell;
use std::rc::Rc;

use crate::gio::prelude::*;
use crate::{gio, glib};

/// A template, or a folder holding templates.
pub enum Entry {
    File { name: String, file: gio::File },
    Folder { name: String, children: Vec<Entry> },
}

/// As many as GNOME Files offers from one folder, and as deep as it goes.
const PER_FOLDER: usize = 30;
const MAX_DEPTH: u32 = 5;

thread_local! {
    static ENTRIES: RefCell<Rc<Vec<Entry>>> = RefCell::new(Rc::new(Vec::new()));
    static MONITORS: RefCell<Vec<gio::FileMonitor>> = const { RefCell::new(Vec::new()) };
    /// Which read is the current one: a burst of changes starts several, and the last one
    /// to be asked for is the one whose answer counts.
    static GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The templates folder, if the user has one that is not the home folder itself.
fn folder() -> Option<gio::File> {
    let path = glib::user_special_dir(glib::UserDirectory::Templates)?;
    (path != glib::home_dir() && path.is_dir()).then(|| gio::File::for_path(path))
}

/// Read the folder, and read it again whenever something in it changes.
pub fn init() {
    refresh();
}

/// What the last read found. Empty until then, and where there is no templates folder.
pub fn entries() -> Rc<Vec<Entry>> {
    ENTRIES.with(|e| e.borrow().clone())
}

fn refresh() {
    let Some(folder) = folder() else {
        return;
    };
    let generation = GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    glib::spawn_future_local(async move {
        let (read, monitors) = scan(folder, MAX_DEPTH).await;
        if GENERATION.with(|g| g.get()) != generation {
            return;
        }
        ENTRIES.with(|e| e.replace(Rc::new(read)));
        // The monitors are kept for as long as the entries they watch, and replaced with
        // them: a folder that has gone stops being watched.
        MONITORS.with(|m| m.replace(monitors));
    });
}

type Scan =
    std::pin::Pin<Box<dyn std::future::Future<Output = (Vec<Entry>, Vec<gio::FileMonitor>)>>>;

fn scan(folder: gio::File, depth: u32) -> Scan {
    Box::pin(async move {
        let mut entries = Vec::new();
        let mut monitors = Vec::new();
        if let Ok(monitor) =
            folder.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            monitor.connect_changed(|_, _, _, _| refresh());
            monitors.push(monitor);
        }
        let mut found = crate::ops::children(&folder, "standard::*").await;
        found.sort_by_key(|(_, info)| glib::FilenameCollationKey::from(info.display_name()));
        for (file, info) in found.into_iter().take(PER_FOLDER) {
            let name = info.display_name().to_string();
            if info.file_type() == gio::FileType::Directory {
                // Hidden folders are skipped whatever the setting: a `.git` in there is
                // not a menu of documents.
                if depth == 0 || info.is_hidden() {
                    continue;
                }
                let (children, mut below) = scan(file, depth - 1).await;
                if children.is_empty() {
                    continue;
                }
                monitors.append(&mut below);
                entries.push(Entry::Folder { name, children });
            } else if !info.is_backup() {
                entries.push(Entry::File { name, file });
            }
        }
        (entries, monitors)
    })
}
