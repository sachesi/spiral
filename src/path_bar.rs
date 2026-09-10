//! Breadcrumb path bar in the Nautilus style: a pill with flat buttons and a current-folder menu.

use std::cell::RefCell;

use gettextrs::gettext;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::file_utils;
use crate::{gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/sachesi/spiral/ui/path_bar.ui")]
    #[properties(wrapper_type = super::PathBar)]
    pub struct PathBar {
        #[template_child]
        pub scrolled: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub buttons_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub crumb_menu: TemplateChild<gio::MenuModel>,
        #[property(get, set = Self::set_location, nullable)]
        location: RefCell<Option<gio::File>>,
        /// The `crumb.*` actions and the folder they act on: the right-clicked crumb, or
        /// the current folder while the ⋮ menu is open.
        pub actions: gio::SimpleActionGroup,
        pub menu_file: RefCell<Option<gio::File>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        /// Root and name of the mount the location is on, looked up in the background;
        /// None for home and `/`.
        pub mount: RefCell<Option<(gio::File, String)>>,
        /// Kept for its mount signals: a chain drawn before its share was mounted is drawn
        /// again from the mount once there is one.
        pub monitor: RefCell<Option<gio::VolumeMonitor>>,
        /// A location and the name it was listed under, standing in for the mount it has
        /// not got: a server reached by a bare address, found on the network.
        pub given: RefCell<Option<(gio::File, String)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PathBar {
        const NAME: &'static str = "SpiralPathBar";
        type Type = super::PathBar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PathBar {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("navigate")
                        .param_types([gio::File::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("edit-location").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let monitor = gio::VolumeMonitor::get();
            let again = glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |_: &gio::VolumeMonitor, _: &gio::Mount| {
                    if let Some(file) = bar.location.borrow().clone() {
                        bar.look_up_mount(file);
                    }
                }
            );
            monitor.connect_mount_added(again.clone());
            monitor.connect_mount_removed(again);
            self.monitor.replace(Some(monitor));
            // Keep the current folder in view as the bar fills up.
            let adj = self.scrolled.hadjustment();
            adj.connect_changed(glib::clone!(
                #[weak(rename_to = scrolled)]
                self.scrolled,
                move |_| {
                    if let Some(viewport) = scrolled.child().and_downcast::<gtk::Viewport>()
                        && let Some(last) = viewport.child().and_then(|b| b.last_child())
                    {
                        viewport.scroll_to(&last, None);
                    }
                }
            ));
            // Vertical wheel scrolls the crumbs horizontally.
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.connect_scroll(glib::clone!(
                #[weak]
                adj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, _, dy| {
                    adj.set_value(adj.value() + dy * adj.step_increment().max(24.0));
                    glib::Propagation::Stop
                }
            ));
            self.scrolled.add_controller(scroll);

            self.obj().setup_actions();
            // The ⋮ menu shares the crumb actions; there they act on the current folder.
            self.menu_button.connect_active_notify(glib::clone!(
                #[weak(rename_to = bar)]
                self.obj(),
                move |b| {
                    if b.is_active() {
                        bar.set_menu_file(bar.location());
                    }
                }
            ));
        }
    }

    impl WidgetImpl for PathBar {}
    impl BoxImpl for PathBar {}

    impl PathBar {
        fn set_location(&self, file: Option<gio::File>) {
            // Rebuilding the same chain would throw away the scroll position.
            let same = match (&*self.location.borrow(), &file) {
                (Some(a), Some(b)) => a.equal(b),
                (None, None) => true,
                _ => false,
            };
            if same {
                return;
            }
            self.location.replace(file.clone());
            self.mount
                .replace(file.as_ref().and_then(|f| self.given(f)));
            self.rebuild(file.as_ref());
            // The mount lookup can talk to gvfs, so the chain is drawn from `/` first and
            // redrawn from the mount point once known.
            if let Some(file) = file {
                self.look_up_mount(file);
            }
        }

        /// Find the mount `file` is on and, where that is news, draw the chain again from
        /// its root. Asked again whenever a mount comes or goes: a share reached from a
        /// bookmark is mounted after its chain was first drawn.
        fn look_up_mount(&self, file: gio::File) {
            let home = gio::File::for_path(glib::home_dir());
            if file.equal(&home) || file.has_prefix(&home) {
                return;
            }
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = bar)]
                self.obj(),
                async move {
                    let lookup = file.clone();
                    // Mount objects are not Send; only the root URI and name travel back.
                    let found = gio::spawn_blocking(move || {
                        lookup
                            .find_enclosing_mount(gio::Cancellable::NONE)
                            .ok()
                            .map(|m| (m.root().uri().to_string(), m.name().to_string()))
                    })
                    .await
                    .ok()
                    .flatten()
                    .map(|(root, name)| (gio::File::for_uri(&root), name))
                    .filter(|(root, _)| root.path().is_none_or(|p| p.as_os_str() != "/"))
                    .or_else(|| bar.imp().given(&file));
                    let imp = bar.imp();
                    if !imp
                        .location
                        .borrow()
                        .as_ref()
                        .is_some_and(|l| l.equal(&file))
                    {
                        return;
                    }
                    let same = match (&*imp.mount.borrow(), &found) {
                        (Some((a, an)), Some((b, bn))) => a.equal(b) && an == bn,
                        (None, None) => true,
                        _ => false,
                    };
                    if same {
                        return;
                    }
                    imp.mount.replace(found);
                    imp.rebuild(Some(&file));
                }
            ));
        }

        /// The name `file` was listed under, if it was and it is `file`'s.
        fn given(&self, file: &gio::File) -> Option<(gio::File, String)> {
            self.given.borrow().clone().filter(|(f, _)| f.equal(file))
        }

        fn rebuild(&self, file: Option<&gio::File>) {
            while let Some(child) = self.buttons_box.first_child() {
                self.buttons_box.remove(&child);
            }
            let Some(file) = file else { return };

            // Chain from the nearest "root": home, a mount point, or the filesystem root.
            let mount = self.mount.borrow().clone();
            let root = chain_root(file, mount.as_ref());
            let mut chain = vec![file.clone()];
            let mut cur = file.clone();
            while !cur.equal(&root) {
                let Some(p) = cur.parent() else { break };
                chain.push(p.clone());
                cur = p;
            }
            chain.reverse();

            let obj = self.obj();
            let last = chain.len() - 1;
            for (i, f) in chain.into_iter().enumerate() {
                let button = gtk::Button::builder()
                    .focus_on_click(false)
                    .css_classes(["spiral-path-button"])
                    .build();
                let click = gtk::GestureClick::builder().button(3).build();
                let target = f.clone();
                click.connect_pressed(glib::clone!(
                    #[weak]
                    obj,
                    #[weak]
                    button,
                    move |g, _, _, _| {
                        g.set_state(gtk::EventSequenceState::Claimed);
                        obj.popup_crumb_menu(&button, &target);
                    }
                ));
                button.add_controller(click);
                let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                let label = gtk::Label::builder().single_line_mode(true).build();
                if i == 0 {
                    let (icon, name) = root_icon_and_name(&f, mount.as_ref());
                    let child = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    child.append(&gtk::Image::from_icon_name(icon));
                    label.set_label(&name);
                    child.append(&label);
                    button.set_child(Some(&child));
                    container.set_spacing(6);
                } else {
                    let sep = gtk::Label::builder()
                        .label("/")
                        .css_classes(["dim-label"])
                        .build();
                    container.append(&sep);
                    label.set_label(&file_utils::location_name(&f));
                    button.set_child(Some(&label));
                }
                // Long names ellipsize down to a floor, never to a lone "…"; the bar scrolls
                // instead. Short names keep their natural width.
                let min_chars = if i == last { 28 } else { 7 };
                if label.label().chars().count() as f64 > min_chars as f64 * 1.5 {
                    label.set_width_chars(min_chars);
                    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                }
                container.append(&button);
                if i != last {
                    let target = gtk::DropTarget::new(
                        gtk::gdk::FileList::static_type(),
                        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
                    );
                    target.connect_enter(|t, _, _| crate::browser_view::preferred_action(t));
                    target.connect_motion(|t, _, _| crate::browser_view::preferred_action(t));
                    let dest = f.clone();
                    target.connect_drop(glib::clone!(
                        #[weak]
                        button,
                        #[upgrade_or]
                        false,
                        move |t, value, x, y| {
                            let view = button
                                .root()
                                .and_downcast::<crate::window::SpiralWindow>()
                                .and_then(|w| w.current_view());
                            match view {
                                Some(v) => v.drop_files(t, value, &dest, x, y),
                                None => false,
                            }
                        }
                    ));
                    button.add_controller(target);
                    // A drag held on a crumb goes there, the same as one held on a folder.
                    crate::browser_view::open_on_hover(
                        &button,
                        glib::clone!(
                            #[weak]
                            obj,
                            #[strong(rename_to = crumb)]
                            f,
                            move || obj.emit_by_name::<()>("navigate", &[&crumb])
                        ),
                    );
                }
                if i == last {
                    button.add_css_class("current-dir");
                    button.set_hexpand(true);
                    label.set_halign(gtk::Align::Start);
                    if i == 0 {
                        button.child().unwrap().set_halign(gtk::Align::Start);
                    }
                    button.set_tooltip_text(Some(&gettext("Edit Location")));
                    button.connect_clicked(glib::clone!(
                        #[weak]
                        obj,
                        move |_| obj.emit_by_name::<()>("edit-location", &[])
                    ));
                } else {
                    button.connect_clicked(glib::clone!(
                        #[weak]
                        obj,
                        move |_| obj.emit_by_name::<()>("navigate", &[&f])
                    ));
                }
                self.buttons_box.append(&container);
            }
        }
    }
}

/// Where the crumb chain starts for `file`.
pub(crate) fn chain_root(file: &gio::File, mount: Option<&(gio::File, String)>) -> gio::File {
    let home = gio::File::for_path(glib::home_dir());
    if file.equal(&home) || file.has_prefix(&home) {
        return home;
    }
    if let Some((root, _)) = mount {
        return root.clone();
    }
    let mut cur = file.clone();
    while let Some(p) = cur.parent() {
        cur = p;
    }
    cur
}

fn root_icon_and_name(
    root: &gio::File,
    mount: Option<&(gio::File, String)>,
) -> (&'static str, String) {
    let home = gio::File::for_path(glib::home_dir());
    if root.equal(&home) {
        return ("user-home-symbolic", gettext("Home"));
    }
    if root.uri().starts_with("trash:") {
        return ("user-trash-symbolic", gettext("Trash"));
    }
    if crate::starred::is_starred_location(root) {
        return ("starred-symbolic", gettext("Favorites"));
    }
    if crate::tags::is_tag_location(root) {
        return ("tag-symbolic", file_utils::location_name(root));
    }
    if root.path().is_some_and(|p| p.as_os_str() == "/") {
        let name = glib::os_info("NAME")
            .map(|s| s.to_string())
            .unwrap_or_else(|| gettext("System"));
        return ("drive-harddisk-symbolic", name);
    }
    if let Some((_, name)) = mount {
        let icon = if root.is_native() {
            "drive-removable-media-symbolic"
        } else {
            "folder-remote-symbolic"
        };
        return (icon, name.clone());
    }
    ("folder-symbolic", file_utils::location_name(root))
}

glib::wrapper! {
    pub struct PathBar(ObjectSubclass<imp::PathBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PathBar {
    /// Tell the bar what a location was listed under, ahead of setting the location, for
    /// one that has no mount to be named after.
    pub fn set_given_name(&self, given: Option<(gio::File, String)>) {
        self.imp().given.replace(given);
    }

    pub fn connect_navigate<F: Fn(&Self, &gio::File) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "navigate",
            false,
            glib::closure_local!(move |bar: &Self, file: &gio::File| f(bar, file)),
        )
    }

    pub fn connect_edit_location<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "edit-location",
            false,
            glib::closure_local!(move |bar: &Self| f(bar)),
        )
    }

    fn setup_actions(&self) {
        let group = &self.imp().actions;
        let add = |name: &str, f: fn(&PathBar, gio::File)| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |_, _| {
                    let file = bar.imp().menu_file.borrow().clone();
                    if let Some(file) = file {
                        f(&bar, file);
                    }
                }
            ));
            group.add_action(&action);
        };
        add("open-new-tab", |bar, file| {
            if let Some(win) = bar.root().and_downcast::<crate::window::SpiralWindow>() {
                win.add_tab(&file, true);
            }
        });
        add("open-new-window", |_, file| {
            if let Some(app) =
                gio::Application::default().and_downcast::<crate::application::SpiralApplication>()
            {
                app.open_window(&[file]);
            }
        });
        add("add-bookmark", |bar, file| {
            crate::bookmarks::add(&file);
            bar.set_menu_file(Some(file));
        });
        add("copy-location", |bar, file| {
            let text = match file.path() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => file.uri().to_string(),
            };
            bar.clipboard().set_text(&text);
        });
        add("properties", |bar, file| {
            crate::dialogs::PropertiesDialog::open(vec![file], bar, || {});
        });
        self.insert_action_group("crumb", Some(group));
    }

    /// Points the `crumb.*` actions at `file` and refreshes what they allow.
    fn set_menu_file(&self, file: Option<gio::File>) {
        let imp = self.imp();
        let enable = |name: &str, on: bool| {
            if let Some(a) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(on);
            }
        };
        // The chooser embeds the bar too, with no tabs or windows to open.
        let in_window = self
            .root()
            .and_downcast::<crate::window::SpiralWindow>()
            .is_some();
        enable("open-new-tab", in_window);
        enable("open-new-window", in_window);
        enable(
            "add-bookmark",
            file.as_ref()
                .is_some_and(|f| !crate::bookmarks::contains(f)),
        );
        imp.menu_file.replace(file);
    }

    fn popup_crumb_menu(&self, button: &gtk::Button, file: &gio::File) {
        let imp = self.imp();
        self.set_menu_file(Some(file.clone()));
        let existing = imp.popover.borrow().clone();
        let popover = existing.unwrap_or_else(|| {
            let p = gtk::PopoverMenu::from_model(Some(&*imp.crumb_menu));
            p.set_parent(self);
            imp.popover.replace(Some(p.clone()));
            p
        });
        let rect = button
            .compute_bounds(self)
            .map(|b| {
                gtk::gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                )
            })
            .unwrap_or_else(|| gtk::gdk::Rectangle::new(0, 0, 1, 1));
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }
}
