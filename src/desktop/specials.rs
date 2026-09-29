//! The places the desktop shows beside the files of its folder: the home folder, the trash
//! and the mounted drives, each switched on and off in the settings. They are not files of
//! the folder, so nothing done to the files can reach them: Home cannot be renamed or
//! trashed from the desktop.

use gettextrs::gettext;

use crate::gio;
use crate::gio::prelude::*;
use crate::glib;

pub const TRASH_URI: &str = "trash:///";

#[derive(Debug, Clone)]
pub enum Kind {
    Home,
    Trash,
    Drive(gio::Mount),
}

#[derive(Debug, Clone)]
pub struct Special {
    /// What its place is kept under: `home`, `trash`, or `drive:` and the drive's own name.
    pub key: String,
    pub name: String,
    pub icon: gio::Icon,
    pub file: gio::File,
    pub kind: Kind,
}

impl Special {
    pub fn is_trash(&self) -> bool {
        matches!(self.kind, Kind::Trash)
    }

    pub fn mount(&self) -> Option<&gio::Mount> {
        match &self.kind {
            Kind::Drive(mount) => Some(mount),
            _ => None,
        }
    }
}

/// The places to show, in the order they are first put down: Home, the trash, then the
/// drives as the volume monitor lists them.
pub fn list(settings: &gio::Settings) -> Vec<Special> {
    let mut specials = Vec::new();
    if settings.boolean("desktop-show-home") {
        specials.push(Special {
            key: "home".into(),
            name: gettext("Home"),
            icon: gio::ThemedIcon::new("user-home").upcast(),
            file: gio::File::for_path(glib::home_dir()),
            kind: Kind::Home,
        });
    }
    if settings.boolean("desktop-show-trash") {
        specials.push(Special {
            key: "trash".into(),
            name: gettext("Trash"),
            icon: trash_icon(false),
            file: gio::File::for_uri(TRASH_URI),
            kind: Kind::Trash,
        });
    }
    if settings.boolean("desktop-show-drives") {
        for mount in gio::VolumeMonitor::get().mounts() {
            if mount.is_shadowed() || !(mount.can_unmount() || mount.can_eject()) {
                continue;
            }
            let root = mount.root();
            let id = mount
                .uuid()
                .map(|u| u.to_string())
                .unwrap_or_else(|| root.uri().to_string());
            specials.push(Special {
                key: format!("drive:{id}"),
                name: mount.name().to_string(),
                icon: mount.icon(),
                file: root,
                kind: Kind::Drive(mount),
            });
        }
    }
    specials
}

pub fn trash_icon(full: bool) -> gio::Icon {
    gio::ThemedIcon::new(if full {
        "user-trash-full"
    } else {
        "user-trash"
    })
    .upcast()
}

/// Whether anything is in the trash; empty when it cannot be asked.
pub async fn trash_full() -> bool {
    gio::File::for_uri(TRASH_URI)
        .query_info_future(
            "trash::item-count",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::LOW,
        )
        .await
        .is_ok_and(|info| info.attribute_uint32("trash::item-count") > 0)
}
