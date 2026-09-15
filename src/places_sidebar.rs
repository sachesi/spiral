//! Places sidebar: home, starred, trash, XDG dirs and GTK bookmarks (reorderable, renamable),
//! drives and mounts from `gio::VolumeMonitor`, and the tags while they are on.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};

use crate::object_data::Key;
use crate::{adw, gdk, gio, glib, gtk};

mod devices;
mod dnd;
mod menu;
mod rows;

pub(crate) use devices::*;
use dnd::*;
use rows::*;

static ROW_FILE: Key<gio::File> = Key::new("file");
static BOOKMARK: Key<bool> = Key::new("bookmark");
static EJECT: Key<EjectTarget> = Key::new("eject");
static VOLUME: Key<gio::Volume> = Key::new("volume");
static SECTION: Key<u8> = Key::new("section");
static ROW_TAG: Key<String> = Key::new("tag");

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/spiral/ui/places_sidebar.ui")]
    pub struct PlacesSidebar {
        #[template_child]
        pub list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub row_menu: TemplateChild<gio::MenuModel>,
        #[template_child]
        pub tag_menu: TemplateChild<gio::MenuModel>,
        pub monitor: gio::VolumeMonitor,
        /// Handlers on the monitor, which is shared by every window and outlives them.
        pub monitor_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        /// Handlers on the application's settings, which outlive the window as well.
        pub settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub bookmarks_monitor: RefCell<Option<gio::FileMonitor>>,
        pub trash_monitor: RefCell<Option<gio::FileMonitor>>,
        /// Whether the trash is known to hold nothing, so emptying it is not offered.
        /// Read from the trash and kept, since a right click cannot wait for the answer.
        pub trash_empty: Cell<bool>,
        pub current: RefCell<Option<gio::File>>,
        pub actions: gio::SimpleActionGroup,
        /// Row the context menu was opened on.
        pub menu_row: RefCell<Option<gtk::ListBoxRow>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        /// The row of colours in a tag's menu, kept between menus as the popover keeps it.
        pub color_picker: RefCell<Option<gtk::Box>>,
        /// Whether the tags are left out, as they are where a file is being saved: a tag
        /// lists files, and a file cannot be saved into one.
        pub hide_tags: Cell<bool>,
    }

    impl Default for PlacesSidebar {
        fn default() -> Self {
            Self {
                list: Default::default(),
                row_menu: Default::default(),
                tag_menu: Default::default(),
                monitor: gio::VolumeMonitor::get(),
                monitor_handlers: Default::default(),
                settings_handlers: Default::default(),
                bookmarks_monitor: Default::default(),
                trash_monitor: Default::default(),
                trash_empty: Default::default(),
                current: Default::default(),
                actions: gio::SimpleActionGroup::new(),
                menu_row: Default::default(),
                popover: Default::default(),
                color_picker: Default::default(),
                hide_tags: Default::default(),
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
            let settings = crate::prefs::settings();
            for id in self.settings_handlers.take() {
                settings.disconnect(id);
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

            // Both places are the user's to keep or drop, and so are the tags.
            for key in [
                "show-root",
                "show-favorites",
                "use-network",
                "use-tags",
                "tags",
            ] {
                let id = crate::prefs::settings().connect_changed(
                    Some(key),
                    glib::clone!(
                        #[strong]
                        rebuild,
                        move |_, _| rebuild()
                    ),
                );
                self.settings_handlers.borrow_mut().push(id);
            }

            // The GTK file chooser and other file managers edit the same bookmarks file.
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

            // What the trash holds decides whether emptying it is offered.
            let trash = gio::File::for_uri("trash:///");
            if let Ok(m) =
                trash.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            {
                m.connect_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_, _, _, _| obj.read_trash_state()
                ));
                self.trash_monitor.replace(Some(m));
            }
            obj.read_trash_state();

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

            // A tag goes last when dropped below the rows, where the tags end. Over a place
            // or a device there is nothing it could mean, and the drag says so while it is
            // there; the tags themselves take it through their own rows.
            let below = |list: &gtk::ListBox, y: f64| list.row_at_y(y as i32).is_none();
            let action = move |list: &gtk::ListBox, y: f64| {
                if below(list, y) {
                    gdk::DragAction::MOVE
                } else {
                    gdk::DragAction::empty()
                }
            };
            let target = gtk::DropTarget::new(TagDrag::static_type(), gdk::DragAction::MOVE);
            let list = self.list.get();
            target.connect_enter(glib::clone!(
                #[weak]
                list,
                #[upgrade_or]
                gdk::DragAction::empty(),
                move |_, _, y| action(&list, y)
            ));
            target.connect_motion(glib::clone!(
                #[weak]
                list,
                #[upgrade_or]
                gdk::DragAction::empty(),
                move |_, _, y| action(&list, y)
            ));
            target.connect_drop(glib::clone!(
                #[weak]
                list,
                #[upgrade_or]
                false,
                move |_, value, _, y| {
                    let Ok(drag) = value.get::<TagDrag>() else {
                        return false;
                    };
                    if !below(&list, y) {
                        return false;
                    }
                    crate::tags::move_to(&drag.0, None);
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

impl PlacesSidebar {
    pub fn set_hide_tags(&self, hide: bool) {
        self.imp().hide_tags.set(hide);
        self.rebuild();
    }

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
            BOOKMARK.set(&row, true);
            add_bookmark_dnd(&row, &file);
            list.append(&row);
            seen.push(file);
        }

        // Devices: every volume (mounted or not), then mounts without a volume. What is
        // mounted from another machine is not a device, and waits for the section below.
        for volume in imp.monitor.volumes() {
            list.append(&self.volume_row(&volume));
        }
        let mut servers = Vec::new();
        for mount in imp.monitor.mounts() {
            if mount.is_shadowed() || mount.volume().is_some() {
                continue;
            }
            if crate::prefs::use_network() && crate::network::is_network(&mount.root()) {
                servers.push(mount);
            } else {
                list.append(&self.mount_row(&mount, SECTION_DEVICES));
            }
        }

        // Network: where the machines around are listed, and the servers connected to.
        if crate::prefs::use_network() && crate::network::can_browse() {
            list.append(&place_row(
                "network-workgroup-symbolic",
                &gettext("Network"),
                &gio::File::for_uri(crate::network::NETWORK_URI),
                SECTION_NETWORK,
            ));
        }
        for mount in servers {
            list.append(&self.mount_row(&mount, SECTION_NETWORK));
        }

        if crate::tags::enabled() && !imp.hide_tags.get() {
            for tag in crate::tags::all().iter() {
                list.append(&tag_row(tag));
            }
        }

        let current = imp.current.borrow().clone();
        self.set_selected_location(current.as_ref());
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
            } else if let Some(tag) = row_tag(row) {
                s.rename_tag(row, &tag);
            }
        });
        add("remove", |s, row| {
            if let Some(file) = row_bookmark(row) {
                crate::bookmarks::remove(&file);
                s.rebuild();
            } else if let Some(tag) = row_tag(row) {
                s.remove_tag(tag);
            }
        });
        add("tag-custom-color", |s, row| {
            let Some(tag) = row_tag(row) else { return };
            let start = crate::tags::color_of(&tag)
                .filter(|c| crate::tags::is_custom(c))
                .and_then(|c| gdk::RGBA::parse(&c).ok());
            let dialog = gtk::ColorDialog::builder()
                .with_alpha(false)
                .title(gettext("Tag Colour"))
                .build();
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = sidebar)]
                s,
                async move {
                    let window = sidebar.root().and_downcast::<gtk::Window>();
                    if let Ok(rgba) = dialog
                        .choose_rgba_future(window.as_ref(), start.as_ref())
                        .await
                    {
                        crate::tags::set_color(&tag, &crate::tags::hex(&rgba));
                    }
                }
            ));
        });
        add("new-tag", |s, _| {
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = sidebar)]
                s,
                async move {
                    if let Some(tag) = crate::dialogs::new_tag_dialog(&sidebar).await {
                        crate::tags::add(tag);
                    }
                }
            ));
        });
        // The colour of the tag the menu is over; the menu shows it as the one picked.
        let color = gio::SimpleAction::new_stateful(
            "tag-color",
            Some(glib::VariantTy::STRING),
            &"".to_variant(),
        );
        color.connect_activate(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |action, param| {
                let row = sidebar.imp().menu_row.borrow().clone();
                if let (Some(tag), Some(color)) = (
                    row.as_ref().and_then(row_tag),
                    param.and_then(glib::Variant::str),
                ) {
                    action.set_state(param.unwrap());
                    crate::tags::set_color(&tag, color);
                }
            }
        ));
        group.add_action(&color);
        // The same thing under the two names a row can call it: a disk is ejected, a
        // server is disconnected from.
        let leave: fn(&PlacesSidebar, &gtk::ListBoxRow) = |s, row| {
            if let Some(target) = row_eject(row) {
                s.eject(target);
            }
        };
        add("eject", leave);
        add("disconnect", leave);
        add("empty-trash", |s, _| {
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = sidebar)]
                s,
                async move {
                    let Some(job) = crate::ops::empty_trash_job(&sidebar).await else {
                        return;
                    };
                    if let Some(app) = gio::Application::default()
                        .and_downcast::<crate::application::SpiralApplication>()
                    {
                        app.job_manager().submit(job);
                    }
                }
            ));
        });
        self.insert_action_group("sidebar", Some(group));
    }

    /// Ask the trash how much it holds, and keep the answer for the next context menu.
    /// An unreadable trash counts as one with something in it: emptying it is then offered
    /// and does nothing, which is better than refusing to offer it at all.
    fn read_trash_state(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                let empty = gio::File::for_uri("trash:///")
                    .query_info_future(
                        "trash::item-count",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                    .is_ok_and(|info| {
                        info.has_attribute("trash::item-count")
                            && info.attribute_uint32("trash::item-count") == 0
                    });
                sidebar.imp().trash_empty.set(empty);
            }
        ));
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
