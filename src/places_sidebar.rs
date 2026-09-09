//! Places sidebar: home, starred, trash, XDG dirs and GTK bookmarks (reorderable, renamable),
//! plus drives and mounts from `gio::VolumeMonitor`.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;

use crate::{adw, gdk, gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/spiral/ui/places_sidebar.ui")]
    pub struct PlacesSidebar {
        #[template_child]
        pub list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub row_menu: TemplateChild<gio::MenuModel>,
        pub monitor: gio::VolumeMonitor,
        /// Handlers on the monitor, which is shared by every window and outlives them.
        pub monitor_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub bookmarks_monitor: RefCell<Option<gio::FileMonitor>>,
        pub current: RefCell<Option<gio::File>>,
        pub actions: gio::SimpleActionGroup,
        /// Row the context menu was opened on.
        pub menu_row: RefCell<Option<gtk::ListBoxRow>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
    }

    impl Default for PlacesSidebar {
        fn default() -> Self {
            Self {
                list: Default::default(),
                row_menu: Default::default(),
                monitor: gio::VolumeMonitor::get(),
                monitor_handlers: Default::default(),
                bookmarks_monitor: Default::default(),
                current: Default::default(),
                actions: gio::SimpleActionGroup::new(),
                menu_row: Default::default(),
                popover: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlacesSidebar {
        const NAME: &'static str = "SpiralPlacesSidebar";
        type Type = super::PlacesSidebar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PlacesSidebar {
        fn dispose(&self) {
            for id in self.monitor_handlers.take() {
                self.monitor.disconnect(id);
            }
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("open-location")
                        .param_types([gio::File::static_type(), bool::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.list.set_header_func(|row, before| {
                let separated = before.is_some_and(|b| row_section(b) != row_section(row));
                row.set_header(
                    separated
                        .then(|| gtk::Separator::new(gtk::Orientation::Horizontal))
                        .as_ref(),
                );
            });
            obj.rebuild();

            let rebuild = glib::clone!(
                #[weak]
                obj,
                move || obj.rebuild()
            );
            let m = &self.monitor;
            let handlers = [
                m.connect_mount_added(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_mount_removed(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_mount_changed(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_volume_added(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_volume_removed(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_volume_changed(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_drive_connected(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
                m.connect_drive_disconnected(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _| rebuild()
                )),
            ];
            self.monitor_handlers.replace(handlers.into());

            // Both places are the user's to keep or drop.
            for key in ["show-root", "show-favorites"] {
                crate::prefs::settings().connect_changed(
                    Some(key),
                    glib::clone!(
                        #[strong]
                        rebuild,
                        move |_, _| rebuild()
                    ),
                );
            }

            // The GTK file chooser and Nautilus edit the same bookmarks file.
            let bookmarks = gio::File::for_path(crate::bookmarks::path());
            if let Ok(m) =
                bookmarks.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            {
                m.connect_changed(glib::clone!(
                    #[strong]
                    rebuild,
                    move |_, _, _, _| {
                        // Written by another program, or by us: either way what is held in
                        // memory is out of date.
                        crate::bookmarks::forget();
                        rebuild();
                    }
                ));
                self.bookmarks_monitor.replace(Some(m));
            }

            // Middle click opens in a new tab.
            let click = gtk::GestureClick::builder().button(2).build();
            click.connect_released(glib::clone!(
                #[weak]
                obj,
                move |g, _, _, y| {
                    if let Some(row) = obj.imp().list.row_at_y(y as i32)
                        && let Some(file) = row_file(&row)
                    {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        obj.emit_by_name::<()>("open-location", &[&file, &true]);
                    }
                }
            ));
            self.list.add_controller(click);

            // Right click: row menu.
            let click = gtk::GestureClick::builder().button(3).build();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |g, _, x, y| {
                    if let Some(row) = obj.imp().list.row_at_y(y as i32) {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        obj.popup_row_menu(&row, x, y);
                    }
                }
            ));
            self.list.add_controller(click);
            obj.setup_actions();

            // Empty space below the rows: drop a bookmark to move it last, or folders to
            // bookmark them.
            let target = gtk::DropTarget::new(
                glib::Type::INVALID,
                gdk::DragAction::COPY | gdk::DragAction::MOVE,
            );
            target.set_types(&[BookmarkDrag::static_type(), gdk::FileList::static_type()]);
            target.connect_drop(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                false,
                move |_, value, _, _| {
                    if let Ok(drag) = value.get::<BookmarkDrag>() {
                        crate::bookmarks::move_to(&gio::File::for_uri(&drag.0), None);
                    } else if let Ok(files) = value.get::<gdk::FileList>() {
                        // Whether each one is a folder is a question for the filesystem,
                        // which may be a share that has stopped answering: ask it off the
                        // main loop and bookmark what comes back.
                        glib::spawn_future_local(glib::clone!(
                            #[weak]
                            obj,
                            async move {
                                let mut added = false;
                                for f in files.files() {
                                    if is_dir_future(&f).await {
                                        crate::bookmarks::add(&f);
                                        added = true;
                                    }
                                }
                                if added {
                                    obj.rebuild();
                                }
                            }
                        ));
                        return true;
                    } else {
                        return false;
                    }
                    obj.rebuild();
                    true
                }
            ));
            self.list.add_controller(target);
        }
    }

    impl WidgetImpl for PlacesSidebar {}
    impl BoxImpl for PlacesSidebar {}

    #[gtk::template_callbacks]
    impl PlacesSidebar {
        #[template_callback]
        pub(super) fn on_row_activated(&self, row: &gtk::ListBoxRow, _list: &gtk::ListBox) {
            let obj = self.obj();
            if let Some(file) = row_file(row) {
                obj.emit_by_name::<()>("open-location", &[&file, &false]);
            } else if let Some(volume) = row_volume(row) {
                obj.mount_and_open(volume);
            }
        }
    }
}

glib::wrapper! {
    pub struct PlacesSidebar(ObjectSubclass<imp::PlacesSidebar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

fn row_file(row: &gtk::ListBoxRow) -> Option<gio::File> {
    unsafe { row.data::<gio::File>("file").map(|p| p.as_ref().clone()) }
}

/// The file of a row from the GTK bookmarks file (not the fixed XDG folders).
fn row_bookmark(row: &gtk::ListBoxRow) -> Option<gio::File> {
    let is_bookmark = unsafe { row.data::<bool>("bookmark").is_some() };
    is_bookmark.then(|| row_file(row)).flatten()
}

fn row_eject(row: &gtk::ListBoxRow) -> Option<EjectTarget> {
    unsafe { row.data::<EjectTarget>("eject").map(|p| p.as_ref().clone()) }
}

async fn is_dir_future(file: &gio::File) -> bool {
    file.query_info_future(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .is_ok_and(|info| info.file_type() == gio::FileType::Directory)
}

/// Drag payload for reordering bookmarks.
#[derive(Clone, glib::Boxed)]
#[boxed_type(name = "SpiralBookmarkDrag")]
struct BookmarkDrag(String);

/// Bookmark rows can be dragged among themselves; dropping on one inserts before or after it.
fn add_bookmark_dnd(row: &gtk::ListBoxRow, file: &gio::File) {
    let uri = file.uri().to_string();
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    source.connect_prepare(move |_, _, _| {
        Some(gdk::ContentProvider::for_value(
            &BookmarkDrag(uri.clone()).to_value(),
        ))
    });
    source.connect_drag_begin(glib::clone!(
        #[weak]
        row,
        move |src, _| {
            let paintable = gtk::WidgetPaintable::new(Some(&row));
            src.set_icon(Some(&paintable), 0, row.height() / 2);
        }
    ));
    row.add_controller(source);

    let target = gtk::DropTarget::new(BookmarkDrag::static_type(), gdk::DragAction::MOVE);
    let file = file.clone();
    target.connect_drop(glib::clone!(
        #[weak]
        row,
        #[upgrade_or]
        false,
        move |_, value, _, y| {
            let Ok(drag) = value.get::<BookmarkDrag>() else {
                return false;
            };
            let after = y > f64::from(row.height()) / 2.0;
            crate::bookmarks::move_to(&gio::File::for_uri(&drag.0), Some((&file, after)));
            if let Some(sidebar) = row.ancestor(PlacesSidebar::static_type()) {
                sidebar.downcast::<PlacesSidebar>().unwrap().rebuild();
            }
            true
        }
    ));
    row.add_controller(target);
}

fn row_volume(row: &gtk::ListBoxRow) -> Option<gio::Volume> {
    unsafe {
        row.data::<gio::Volume>("volume")
            .map(|p| p.as_ref().clone())
    }
}

const SECTION_PLACES: u8 = 0;
const SECTION_BOOKMARKS: u8 = 1;
const SECTION_DEVICES: u8 = 2;

fn make_row(icon: &gio::Icon, title: &str, section: u8) -> (gtk::ListBoxRow, gtk::Box) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&gtk::Image::builder().gicon(icon).margin_end(8).build());
    content.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .margin_end(2)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build(),
    );
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .focus_on_click(false)
        .build();
    unsafe { row.set_data("section", section) };
    (row, content)
}

fn place_row(icon: &str, title: &str, file: &gio::File, section: u8) -> gtk::ListBoxRow {
    let (row, _) = make_row(&gio::ThemedIcon::new(icon).upcast(), title, section);
    unsafe { row.set_data("file", file.clone()) };
    add_drop_target(&row, file);
    row
}

/// Files dropped on a sidebar row are copied/moved into that location.
fn add_drop_target(row: &gtk::ListBoxRow, file: &gio::File) {
    if crate::starred::is_starred_location(file) {
        return;
    }
    if file.uri().starts_with("trash:") {
        add_trash_drop_target(row);
        return;
    }
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.connect_enter(|t, _, _| crate::browser_view::preferred_action(t));
    target.connect_motion(|t, _, _| crate::browser_view::preferred_action(t));
    let hovered = file.clone();
    let file = file.clone();
    target.connect_drop(glib::clone!(
        #[weak]
        row,
        #[upgrade_or]
        false,
        move |t, value, x, y| {
            let view = row
                .root()
                .and_downcast::<crate::window::SpiralWindow>()
                .and_then(|w| w.current_view());
            match view {
                Some(v) => v.drop_files(t, value, &file, x, y),
                None => false,
            }
        }
    ));
    row.add_controller(target);
    crate::browser_view::open_on_hover(
        row,
        glib::clone!(
            #[weak]
            row,
            move || {
                if let Some(sidebar) = row
                    .ancestor(PlacesSidebar::static_type())
                    .and_downcast::<PlacesSidebar>()
                {
                    sidebar.emit_by_name::<()>("open-location", &[&hovered, &false]);
                }
            }
        ),
    );
}

/// Files dropped on Trash are trashed. Nothing is asked first: unlike a folder, there is
/// only one thing a drop on the trash can mean, and undo puts them back.
fn add_trash_drop_target(row: &gtk::ListBoxRow) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::MOVE);
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let files = list.files();
        let app =
            gio::Application::default().and_downcast::<crate::application::SpiralApplication>();
        let (Some(app), false) = (app, files.is_empty()) else {
            return false;
        };
        app.job_manager()
            .submit(crate::ops::JobKind::Trash { files });
        true
    });
    row.add_controller(target);
}

fn row_section(row: &gtk::ListBoxRow) -> u8 {
    unsafe { row.data::<u8>("section").map(|p| *p.as_ref()).unwrap_or(0) }
}

impl PlacesSidebar {
    pub fn rebuild(&self) {
        let imp = self.imp();
        let list = &imp.list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        let home = gio::File::for_path(glib::home_dir());
        list.append(&place_row(
            "user-home-symbolic",
            &gettext("Home"),
            &home,
            SECTION_PLACES,
        ));
        if let Some(desktop) = glib::user_special_dir(glib::UserDirectory::Desktop)
            && desktop != glib::home_dir()
            && desktop.is_dir()
        {
            list.append(&place_row(
                "user-desktop-symbolic",
                &gettext("Desktop"),
                &gio::File::for_path(&desktop),
                SECTION_PLACES,
            ));
        }
        if crate::prefs::show_root() {
            list.append(&place_row(
                "drive-harddisk-symbolic",
                &gettext("Root"),
                &gio::File::for_path("/"),
                SECTION_PLACES,
            ));
        }
        if crate::prefs::show_favorites() {
            list.append(&place_row(
                "starred-symbolic",
                &gettext("Favorites"),
                &gio::File::for_uri(crate::starred::URI),
                SECTION_PLACES,
            ));
        }
        list.append(&place_row(
            "user-trash-symbolic",
            &gettext("Trash"),
            &gio::File::for_uri("trash:///"),
            SECTION_PLACES,
        ));

        // Bookmarks: the XDG user folders, then the GTK bookmarks file.
        let mut seen: Vec<gio::File> = vec![home.clone()];
        let dirs = [
            (glib::UserDirectory::Documents, "folder-documents-symbolic"),
            (glib::UserDirectory::Downloads, "folder-download-symbolic"),
            (glib::UserDirectory::Music, "folder-music-symbolic"),
            (glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
            (glib::UserDirectory::Videos, "folder-videos-symbolic"),
        ];
        for (dir, icon) in dirs {
            let Some(path) = glib::user_special_dir(dir) else {
                continue;
            };
            if path == glib::home_dir() || !path.is_dir() {
                continue;
            }
            let file = gio::File::for_path(&path);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            list.append(&place_row(icon, &name, &file, SECTION_BOOKMARKS));
            seen.push(file);
        }
        for (file, label) in crate::bookmarks::load() {
            if seen.iter().any(|f| f.equal(&file)) {
                continue;
            }
            let name = label.unwrap_or_else(|| crate::file_utils::location_name(&file));
            let icon = if file.is_native() {
                "folder-symbolic"
            } else {
                "folder-remote-symbolic"
            };
            let row = place_row(icon, &name, &file, SECTION_BOOKMARKS);
            unsafe { row.set_data("bookmark", true) };
            add_bookmark_dnd(&row, &file);
            list.append(&row);
            seen.push(file);
        }

        // Devices: every volume (mounted or not), then mounts without a volume.
        for volume in imp.monitor.volumes() {
            list.append(&self.volume_row(&volume));
        }
        for mount in imp.monitor.mounts() {
            if mount.is_shadowed() || mount.volume().is_some() {
                continue;
            }
            list.append(&self.mount_row(&mount));
        }

        let current = imp.current.borrow().clone();
        self.set_selected_location(current.as_ref());
    }

    fn volume_row(&self, volume: &gio::Volume) -> gtk::ListBoxRow {
        let (row, content) = make_row(&volume.symbolic_icon(), &volume.name(), SECTION_DEVICES);
        if let Some(mount) = volume.get_mount() {
            unsafe { row.set_data("file", mount.default_location()) };
            add_drop_target(&row, &mount.default_location());
            if mount.can_unmount() || mount.can_eject() {
                content.append(&self.eject_button(&row, EjectTarget::Mount(mount)));
            }
        } else {
            unsafe { row.set_data("volume", volume.clone()) };
            if volume.can_eject() {
                content.append(&self.eject_button(&row, EjectTarget::Volume(volume.clone())));
            }
        }
        row
    }

    fn mount_row(&self, mount: &gio::Mount) -> gtk::ListBoxRow {
        let (row, content) = make_row(&mount.symbolic_icon(), &mount.name(), SECTION_DEVICES);
        unsafe { row.set_data("file", mount.default_location()) };
        add_drop_target(&row, &mount.default_location());
        if mount.can_unmount() || mount.can_eject() {
            content.append(&self.eject_button(&row, EjectTarget::Mount(mount.clone())));
        }
        row
    }

    fn eject_button(&self, row: &gtk::ListBoxRow, target: EjectTarget) -> gtk::Button {
        unsafe { row.set_data("eject", target.clone()) };
        let button = gtk::Button::builder()
            .icon_name("media-eject-symbolic")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .margin_start(4)
            .tooltip_text(gettext("Eject"))
            .css_classes(["flat"])
            .build();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |_| sidebar.eject(target.clone())
        ));
        button
    }

    fn mount_operation(&self) -> gtk::MountOperation {
        let win = self.root().and_downcast::<gtk::Window>();
        gtk::MountOperation::new(win.as_ref())
    }

    fn mount_and_open(&self, volume: gio::Volume) {
        let op = self.mount_operation();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volume
                    .mount_future(gio::MountMountFlags::NONE, Some(&op))
                    .await
                {
                    Ok(()) => {
                        if let Some(mount) = volume.get_mount() {
                            sidebar.emit_by_name::<()>(
                                "open-location",
                                &[&mount.default_location(), &false],
                            );
                        }
                    }
                    Err(e) => sidebar.show_error(&gettext("Could Not Mount"), &e),
                }
            }
        ));
    }

    /// Unmount or eject the device `file` is the root of, if it is the root of one.
    /// Returns whether there was anything to unmount.
    pub(crate) fn eject_file(&self, file: &gio::File) -> bool {
        let Some(mount) = mount_of(file) else {
            return false;
        };
        self.eject(EjectTarget::Mount(mount));
        true
    }

    fn eject(&self, target: EjectTarget) {
        let op = self.mount_operation();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                let flags = gio::MountUnmountFlags::NONE;
                // A volume's own trash is lost once it is unplugged; offer to empty it first.
                if let EjectTarget::Mount(m) = &target
                    && let Some(trash) = mount_trash(&m.root()).await
                    && !sidebar.offer_empty_trash(trash).await
                {
                    return;
                }
                // Removable media: warn while cached writes flush, then say when it is safe.
                let (name, drive) = match &target {
                    EjectTarget::Mount(m) => (m.name(), m.drive()),
                    EjectTarget::Volume(v) => (v.name(), v.drive()),
                };
                let removable = drive.is_some_and(|d| d.is_removable() || d.is_media_removable());
                let app = gio::Application::default();
                if removable && let Some(app) = &app {
                    let n = gio::Notification::new(
                        &gettext("Writing data to “%s”").replace("%s", &name),
                    );
                    n.set_body(Some(&gettext("Don’t unplug until finished")));
                    app.send_notification(Some("unmount"), &n);
                }
                let result = match target {
                    EjectTarget::Mount(m) if m.can_eject() => {
                        m.eject_with_operation_future(flags, Some(&op)).await
                    }
                    EjectTarget::Mount(m) => {
                        m.unmount_with_operation_future(flags, Some(&op)).await
                    }
                    EjectTarget::Volume(v) => v.eject_with_operation_future(flags, Some(&op)).await,
                };
                if removable && let Some(app) = &app {
                    app.withdraw_notification("unmount");
                }
                match result {
                    Ok(()) if removable => {
                        if let Some(app) = &app {
                            let n = gio::Notification::new(
                                &gettext("You can now unplug “%s”").replace("%s", &name),
                            );
                            app.send_notification(Some("unmount-done"), &n);
                        }
                    }
                    Err(e) if !e.matches(gio::IOErrorEnum::FailedHandled) => {
                        sidebar.show_error(&gettext("Could Not Eject"), &e);
                    }
                    _ => {}
                }
            }
        ));
    }

    /// Asks about the trash on a volume about to be unmounted. Returns false to abort.
    async fn offer_empty_trash(&self, trash: gio::File) -> bool {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Empty Trash Before Unmounting?"))
            .body(gettext(
                "To regain the free space on this volume the trash must be emptied. All trashed items on the volume will be permanently lost.",
            ))
            .build();
        dialog.add_response("cancel", &gettext("_Cancel"));
        dialog.add_response("keep", &gettext("Do _Not Empty"));
        dialog.add_response("empty", &gettext("_Empty Trash"));
        dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("cancel");
        match dialog.choose_future(Some(self)).await.as_str() {
            "empty" => {
                let Some(app) = gio::Application::default()
                    .and_downcast::<crate::application::SpiralApplication>()
                else {
                    return true;
                };
                let job = app
                    .job_manager()
                    .submit(crate::ops::JobKind::Delete { files: vec![trash] });
                wait_finished(&job).await;
                true
            }
            "keep" => true,
            _ => false,
        }
    }

    fn show_error(&self, heading: &str, error: &glib::Error) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(error.message())
            .build();
        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self));
    }

    fn setup_actions(&self) {
        let group = &self.imp().actions;
        let add = |name: &str, f: fn(&PlacesSidebar, &gtk::ListBoxRow)| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = sidebar)]
                self,
                move |_, _| {
                    let row = sidebar.imp().menu_row.borrow().clone();
                    if let Some(row) = row {
                        f(&sidebar, &row);
                    }
                }
            ));
            group.add_action(&action);
        };
        add("open", |s, row| {
            s.imp().on_row_activated(row, &s.imp().list)
        });
        add("open-new-tab", |s, row| {
            if let Some(file) = row_file(row) {
                s.emit_by_name::<()>("open-location", &[&file, &true]);
            }
        });
        add("rename", |s, row| {
            if let Some(file) = row_bookmark(row) {
                s.rename_bookmark(row, &file);
            }
        });
        add("remove", |s, row| {
            if let Some(file) = row_bookmark(row) {
                crate::bookmarks::remove(&file);
                s.rebuild();
            }
        });
        add("eject", |s, row| {
            if let Some(target) = row_eject(row) {
                s.eject(target);
            }
        });
        self.insert_action_group("sidebar", Some(group));
    }

    fn popup_row_menu(&self, row: &gtk::ListBoxRow, x: f64, y: f64) {
        let imp = self.imp();
        imp.menu_row.replace(Some(row.clone()));
        let enable = |name: &str, on: bool| {
            if let Some(a) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(on);
            }
        };
        let bookmark = row_bookmark(row).is_some();
        enable("open-new-tab", row_file(row).is_some());
        enable("rename", bookmark);
        enable("remove", bookmark);
        enable("eject", row_eject(row).is_some());
        let existing = imp.popover.borrow().clone();
        let popover = existing.unwrap_or_else(|| {
            let p = gtk::PopoverMenu::from_model(Some(&*imp.row_menu));
            p.set_parent(self);
            p.set_has_arrow(false);
            p.set_halign(gtk::Align::Start);
            imp.popover.replace(Some(p.clone()));
            p
        });
        let p = imp
            .list
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
        popover.popup();
    }

    /// Entry popover over `row` for the bookmark's label; empty restores the folder name.
    fn rename_bookmark(&self, row: &gtk::ListBoxRow, file: &gio::File) {
        let label = crate::bookmarks::load()
            .into_iter()
            .find(|(f, _)| f.equal(file))
            .and_then(|(_, l)| l)
            .unwrap_or_else(|| crate::file_utils::location_name(file));
        let entry = gtk::Entry::builder().text(&label).build();
        let button = gtk::Button::builder()
            .label(gettext("_Rename"))
            .use_underline(true)
            .css_classes(["suggested-action"])
            .build();
        let bx = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        bx.append(&entry);
        bx.append(&button);
        let bounds = row
            .compute_bounds(self)
            .map(|b| {
                gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                )
            })
            .unwrap_or_else(|| gdk::Rectangle::new(0, 0, 1, 1));
        let popover = gtk::Popover::builder()
            .child(&bx)
            .pointing_to(&bounds)
            .build();
        popover.set_parent(self);
        let accept = glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            #[weak]
            entry,
            #[weak]
            popover,
            #[strong]
            file,
            move || {
                crate::bookmarks::rename(&file, &entry.text());
                popover.popdown();
                sidebar.rebuild();
            }
        );
        button.connect_clicked(glib::clone!(
            #[strong]
            accept,
            move |_| accept()
        ));
        entry.connect_activate(move |_| accept());
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
        entry.grab_focus();
    }

    /// Highlight the row matching `file`, or none.
    pub fn set_selected_location(&self, file: Option<&gio::File>) {
        let list = &self.imp().list;
        self.imp().current.replace(file.cloned());
        list.unselect_all();
        let Some(file) = file else { return };
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if row_file(&row).is_some_and(|f| f.equal(file)) {
                list.select_row(Some(&row));
                return;
            }
            i += 1;
        }
    }

    pub fn connect_open_location<F: Fn(&Self, &gio::File, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "open-location",
            false,
            glib::closure_local!(move |s: &Self, file: &gio::File, new_tab: bool| f(
                s, file, new_tab
            )),
        )
    }
}

/// The current user's trash directory on `root`, if it holds anything.
async fn mount_trash(root: &gio::File) -> Option<gio::File> {
    let uid = unsafe { libc::getuid() };
    let trash = root.child(format!(".Trash-{uid}"));
    let en = trash
        .child("files")
        .enumerate_children_future(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        )
        .await
        .ok()?;
    let first = en
        .next_files_future(1, glib::Priority::DEFAULT)
        .await
        .ok()?;
    (!first.is_empty()).then_some(trash)
}

async fn wait_finished(job: &crate::ops::Job) {
    if job.is_finished() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = job.connect_status_notify(move |job| {
        if job.is_finished()
            && let Some(tx) = tx.take()
        {
            let _ = tx.send(());
        }
    });
    let _ = rx.await;
    job.disconnect(id);
}

/// The mount `file` is the root of: a device, not a folder that merely lives on one.
pub(crate) fn mount_of(file: &gio::File) -> Option<gio::Mount> {
    gio::VolumeMonitor::get()
        .mounts()
        .into_iter()
        .find(|m| m.root().equal(file) || m.default_location().equal(file))
}

#[derive(Clone)]
enum EjectTarget {
    Mount(gio::Mount),
    Volume(gio::Volume),
}
