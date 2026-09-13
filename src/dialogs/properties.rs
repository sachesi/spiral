//! Properties dialog for one or more files: general page, and for a single local file a
//! permissions page that edits `unix::mode` in place.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::{adw, file_utils, gio, glib, gtk, prefs};

mod disk;
mod general;
mod permissions;

use disk::*;
use permissions::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PropertiesDialog {
        pub cancellable: gio::Cancellable,
        pub changed: RefCell<Option<Box<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PropertiesDialog {
        const NAME: &'static str = "SpiralPropertiesDialog";
        type Type = super::PropertiesDialog;
        type ParentType = adw::PreferencesDialog;
    }

    impl ObjectImpl for PropertiesDialog {}
    impl WidgetImpl for PropertiesDialog {}
    impl AdwDialogImpl for PropertiesDialog {
        fn closed(&self) {
            self.cancellable.cancel();
            self.parent_closed();
        }
    }
    impl PreferencesDialogImpl for PropertiesDialog {}
}

glib::wrapper! {
    pub struct PropertiesDialog(ObjectSubclass<imp::PropertiesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

const ATTRS: &str = "standard::*,time::modified,time::access,time::created,owner::user,\
owner::group,unix::mode,unix::uid,access::*,selinux::context,metadata::custom-icon,\
metadata::custom-icon-name,trash::orig-path,trash::deletion-date";

/// What the disk page is told by the filesystem.
const FS_ATTRS: &str = "filesystem::size,filesystem::free,filesystem::used,filesystem::type";

/// Show a folder in the window the dialog came from, with some of what is in it selected.
type Reveal = Rc<dyn Fn(&gio::File, Vec<gio::File>)>;

impl PropertiesDialog {
    /// Query the files in the background, then build and present the dialog. Nothing is
    /// shown for files that cannot be read.
    pub fn open(
        files: Vec<gio::File>,
        parent: &impl IsA<gtk::Widget>,
        on_changed: impl Fn() + 'static,
    ) {
        let parent = parent.clone();
        glib::spawn_future_local(async move {
            let mut infos = Vec::new();
            for f in files {
                if let Ok(i) = f
                    .query_info_future(
                        ATTRS,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    infos.push((f, i));
                }
            }
            // The folders the dialog names can be opened where there is a window of folders
            // to open them in; a file chooser has none.
            let reveal = parent
                .root()
                .and_downcast::<crate::window::SpiralWindow>()
                .map(|win| {
                    let win = win.downgrade();
                    Rc::new(move |folder: &gio::File, select: Vec<gio::File>| {
                        if let Some(win) = win.upgrade() {
                            win.reveal(folder, select);
                        }
                    }) as Reveal
                });
            // A folder has a page about the disk it is on, where the filesystem says how big
            // that is.
            // A share that is slow to answer does not hold the dialog up; it goes without.
            let disk = match infos.as_slice() {
                [(file, info)] if file_utils::is_dir(info) => glib::future_with_timeout(
                    std::time::Duration::from_secs(1),
                    file.query_filesystem_info_future(FS_ATTRS, glib::Priority::DEFAULT),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .filter(|fs| fs.attribute_uint64("filesystem::size") > 0),
                _ => None,
            };
            if !infos.is_empty() && parent.root().is_some() {
                let dialog = Self::new(&infos, reveal, disk);
                dialog.connect_changed(on_changed);
                dialog.present(Some(&parent));
            }
        });
    }

    fn new(
        infos: &[(gio::File, gio::FileInfo)],
        reveal: Option<Reveal>,
        disk: Option<gio::FileInfo>,
    ) -> Self {
        // Laid out as the preferences are, a page per tab.
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Properties"))
            .property("search-enabled", false)
            // A little narrower than the preferences, which have more to say per line.
            .property("content-width", 616)
            .build();
        let general = dialog.general_page(infos, reveal);
        general.set_title(&gettext("General"));
        general.set_icon_name(Some("document-properties-symbolic"));
        dialog.add(&general);
        if let [(file, info)] = infos {
            if let Some(fs) = &disk {
                let page = disk_page(file, fs);
                page.set_title(&gettext("Disk"));
                page.set_icon_name(Some("drive-harddisk-symbolic"));
                dialog.add(&page);
            }
            if info.has_attribute("unix::mode") {
                let page = permissions_page(file, info);
                page.set_title(&gettext("Permissions"));
                page.set_icon_name(Some("system-lock-screen-symbolic"));
                dialog.add(&page);
            }
        }
        dialog
    }

    /// Called after something about the files changed (icon, permissions).
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.imp().changed.replace(Some(Box::new(f)));
    }

    fn emit_changed(&self) {
        if let Some(f) = self.imp().changed.borrow().as_ref() {
            f();
        }
    }

    fn show_error(&self, heading: &str, error: &glib::Error) {
        let alert = adw::AlertDialog::builder()
            .heading(heading)
            .body(error.message())
            .build();
        alert.add_response("ok", &gettext("_OK"));
        alert.present(Some(self));
    }
}

/// A fact and its value. The value is text a file or a filesystem gave, never markup: a
/// name with an ampersand in it would not show, and one with tags would be drawn as they say.
pub(crate) fn row(label: &str, value: &str) -> adw::ActionRow {
    // Set apart, after the row is made: a builder does not set its properties in the order
    // it is given them, and the value would be read as markup once before the switch.
    let r = adw::ActionRow::builder()
        .use_markup(false)
        .subtitle_selectable(true)
        .build();
    r.set_title(label);
    r.set_subtitle(value);
    r.add_css_class("property");
    r
}

// ---- permissions -------------------------------------------------------------------------

// ---- size ----------------------------------------------------------------------------------
