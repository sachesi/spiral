//! Entries other programs add to Spiral's menus: desktop entry files in
//! `spiral/desktop-menu/` for the menu of the empty desktop and in `spiral/item-menu/` for
//! the menu of files, under the data directories, the user's first. A file there hides any
//! of the same name further down the list, as the XDG base directories have it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::gio::prelude::*;
use crate::{gio, glib};

/// The menu of the empty desktop.
pub const DESKTOP_MENU: &str = "desktop-menu";
/// The menu of files, in a folder or on the desktop.
pub const ITEM_MENU: &str = "item-menu";

/// Where the entries of `menu` are looked for, in the order they win.
pub fn dirs(menu: &str) -> Vec<PathBuf> {
    std::iter::once(glib::user_data_dir())
        .chain(glib::system_data_dirs())
        .map(|dir| dir.join("spiral").join(menu))
        .collect()
}

/// Whether an entry of the menu of files is for files of these content types: each of a
/// type its `MimeType` names, any when it names none, and only one unless its `Exec` takes
/// a list.
pub fn takes(app: &gio_unix::DesktopAppInfo, types: &[String]) -> bool {
    let many = app.commandline().is_some_and(|line| {
        let line = line.to_string_lossy();
        line.contains("%F") || line.contains("%U")
    });
    if types.is_empty() || (types.len() > 1 && !many) {
        return false;
    }
    let wanted = app.supported_types();
    wanted.is_empty()
        || types.iter().all(|kind| {
            wanted.iter().any(|want| {
                gio::content_type_is_a(kind, want) || gio::content_type_is_mime_type(kind, want)
            })
        })
}

/// The entries in `dirs` to offer, ordered by file name. One is left out when its `TryExec`
/// names a program that is not there, when it is `Hidden` or `NoDisplay`, or when its
/// `OnlyShowIn`/`NotShowIn` keep it from `desktop`; no `desktop` means those named in
/// `XDG_CURRENT_DESKTOP`.
pub fn load(dirs: &[PathBuf], desktop: Option<&str>) -> Vec<gio_unix::DesktopAppInfo> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for dir in dirs {
        for path in entries(dir) {
            let Some(name) = path.file_name().map(|n| n.to_owned()) else {
                continue;
            };
            if !seen.insert(name.clone()) {
                continue;
            }
            let Some(app) = gio_unix::DesktopAppInfo::from_filename(&path) else {
                continue;
            };
            if app.is_hidden() || app.is_nodisplay() || !app.shows_in(desktop) {
                continue;
            }
            found.push((name, app));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.into_iter().map(|(_, app)| app).collect()
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "desktop") && p.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiral-drop-ins-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(dir: &Path, file: &str, name: &str, extra: &str) {
        std::fs::write(
            dir.join(file),
            format!("[Desktop Entry]\nType=Application\nName={name}\nExec=true\n{extra}"),
        )
        .unwrap();
    }

    fn names(apps: &[gio_unix::DesktopAppInfo]) -> Vec<String> {
        apps.iter().map(|a| a.name().to_string()).collect()
    }

    #[test]
    fn entries_come_in_the_order_of_their_file_names() {
        let dir = scratch("order");
        entry(&dir, "20-b.desktop", "Second", "");
        entry(&dir, "10-a.desktop", "First", "");
        std::fs::write(dir.join("notes.txt"), "not an entry").unwrap();
        assert_eq!(names(&load(&[dir], Some("any"))), ["First", "Second"]);
    }

    #[test]
    fn the_users_entry_hides_one_of_the_same_name_below() {
        let user = scratch("user");
        let system = scratch("system");
        entry(&user, "background.desktop", "Mine", "");
        entry(&system, "background.desktop", "Theirs", "");
        entry(&system, "display.desktop", "Display", "");
        assert_eq!(
            names(&load(&[user, system], Some("any"))),
            ["Mine", "Display"]
        );
    }

    #[test]
    fn an_entry_for_another_desktop_is_left_out() {
        let dir = scratch("desktops");
        entry(&dir, "a.desktop", "Only here", "OnlyShowIn=here;\n");
        entry(&dir, "b.desktop", "Not here", "NotShowIn=here;\n");
        assert_eq!(
            names(&load(std::slice::from_ref(&dir), Some("here"))),
            ["Only here"]
        );
        assert_eq!(names(&load(&[dir], Some("elsewhere"))), ["Not here"]);
    }

    #[test]
    fn an_entry_of_the_file_menu_takes_the_types_it_names_one_at_a_time_unless_a_list() {
        let dir = scratch("types");
        let at = |file: &str, extra: &str| {
            std::fs::write(
                dir.join(file),
                format!("[Desktop Entry]\nType=Application\nName={file}\n{extra}"),
            )
            .unwrap();
            gio_unix::DesktopAppInfo::from_filename(dir.join(file)).unwrap()
        };
        let pictures = at("pictures.desktop", "Exec=true %f\nMimeType=image/*;\n");
        let pngs = at("pngs.desktop", "Exec=true %F\nMimeType=image/png;\n");
        let any = at("any.desktop", "Exec=true %U\n");
        let kinds = |kinds: &[&str]| kinds.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>();
        assert!(takes(&pictures, &kinds(&["image/png"])));
        assert!(!takes(&pictures, &kinds(&["image/png", "image/jpeg"])));
        assert!(!takes(&pictures, &kinds(&["text/plain"])));
        assert!(takes(&pngs, &kinds(&["image/png", "image/png"])));
        assert!(!takes(&pngs, &kinds(&["image/png", "image/jpeg"])));
        assert!(takes(&any, &kinds(&["text/plain", "image/png"])));
        assert!(!takes(&any, &[]));
    }

    #[test]
    fn entries_whose_program_is_missing_or_that_are_hidden_are_left_out() {
        let dir = scratch("missing");
        entry(
            &dir,
            "a.desktop",
            "Missing",
            "TryExec=/nonexistent/spiral-test\n",
        );
        entry(&dir, "b.desktop", "Hidden", "Hidden=true\n");
        entry(&dir, "c.desktop", "No display", "NoDisplay=true\n");
        std::fs::write(dir.join("d.desktop"), "not a desktop entry").unwrap();
        entry(&dir, "e.desktop", "Kept", "TryExec=true\n");
        assert_eq!(names(&load(&[dir], Some("any"))), ["Kept"]);
    }
}
