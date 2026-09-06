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
        #[property(get, set = Self::set_location, nullable)]
        location: RefCell<Option<gio::File>>,
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
            self.rebuild(file.as_ref());
        }

        fn rebuild(&self, file: Option<&gio::File>) {
            while let Some(child) = self.buttons_box.first_child() {
                self.buttons_box.remove(&child);
            }
            let Some(file) = file else { return };

            // Chain from the nearest "root": home, a mount point, or the filesystem root.
            let root = chain_root(file);
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
                let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                let label = gtk::Label::builder().single_line_mode(true).build();
                if i == 0 {
                    let (icon, name) = root_icon_and_name(&f);
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
                        move |t, value, _, _| {
                            let view = button
                                .root()
                                .and_downcast::<crate::window::SpiralWindow>()
                                .and_then(|w| w.current_view());
                            match view {
                                Some(v) => v.drop_files(t, value, &dest),
                                None => false,
                            }
                        }
                    ));
                    button.add_controller(target);
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
fn chain_root(file: &gio::File) -> gio::File {
    let home = gio::File::for_path(glib::home_dir());
    if file.equal(&home) || file.has_prefix(&home) {
        return home;
    }
    if let Ok(mount) = file.find_enclosing_mount(gio::Cancellable::NONE) {
        let root = mount.root();
        if root.path().is_none_or(|p| p.as_os_str() != "/") {
            return root;
        }
    }
    let mut cur = file.clone();
    while let Some(p) = cur.parent() {
        cur = p;
    }
    cur
}

fn root_icon_and_name(root: &gio::File) -> (&'static str, String) {
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
    if root.path().is_some_and(|p| p.as_os_str() == "/") {
        let name = glib::os_info("NAME")
            .map(|s| s.to_string())
            .unwrap_or_else(|| gettext("System"));
        return ("drive-harddisk-symbolic", name);
    }
    if let Ok(mount) = root.find_enclosing_mount(gio::Cancellable::NONE) {
        let icon = if root.is_native() {
            "drive-removable-media-symbolic"
        } else {
            "folder-remote-symbolic"
        };
        return (icon, mount.name().to_string());
    }
    ("folder-symbolic", file_utils::location_name(root))
}

glib::wrapper! {
    pub struct PathBar(ObjectSubclass<imp::PathBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PathBar {
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
}
