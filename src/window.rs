use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use std::cell::RefCell;

use crate::browser_view::BrowserView;
use crate::enums::{SortKey, ViewMode};
use crate::file_utils;
use crate::path_bar::PathBar;
use crate::places_sidebar::PlacesSidebar;
use crate::progress_indicator::ProgressIndicator;
use crate::{adw, gio, glib, gtk};

const GRID_ZOOM_SIZES: [i32; 5] = [48, 64, 96, 168, 256];
/// Search match modes in the order of the "Match" row.
const MATCHES: [&str; 3] = ["name", "both", "content"];
const LIST_ZOOM_SIZES: [i32; 5] = [16, 24, 32, 48, 64];
/// How many closed tabs "Restore Closed Tab" remembers per window.
const CLOSED_TABS_MAX: usize = 20;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/spiral/ui/window.ui")]
    pub struct SpiralWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub tab_overview: TemplateChild<adw::TabOverview>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub sidebar: TemplateChild<PlacesSidebar>,
        #[template_child]
        pub progress_indicator: TemplateChild<ProgressIndicator>,
        #[template_child]
        pub path_bar: TemplateChild<PathBar>,
        #[template_child]
        pub toolbar_switcher: TemplateChild<gtk::Stack>,
        #[template_child]
        pub location_entry: TemplateChild<gtk::Entry>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub search_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_kind_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub search_date_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub search_match_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub view_split_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        pub view_split_button_bottom: TemplateChild<adw::SplitButton>,
        pub settings: gio::Settings,
        pub sort_action: gio::SimpleAction,
        /// Locations of tabs closed in this window, most recent last.
        pub closed_tabs: RefCell<Vec<gio::File>>,
    }

    impl Default for SpiralWindow {
        fn default() -> Self {
            Self {
                toast_overlay: Default::default(),
                tab_overview: Default::default(),
                tab_view: Default::default(),
                split_view: Default::default(),
                sidebar: Default::default(),
                progress_indicator: Default::default(),
                path_bar: Default::default(),
                toolbar_switcher: Default::default(),
                location_entry: Default::default(),
                search_entry: Default::default(),
                search_button: Default::default(),
                search_kind_row: Default::default(),
                search_date_row: Default::default(),
                search_match_row: Default::default(),
                view_split_button: Default::default(),
                view_split_button_bottom: Default::default(),
                settings: gio::Settings::new(crate::config::APP_ID),
                sort_action: gio::SimpleAction::new_stateful(
                    "sort",
                    Some(glib::VariantTy::STRING),
                    &"name-asc".to_variant(),
                ),
                closed_tabs: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SpiralWindow {
        const NAME: &'static str = "SpiralWindow";
        type Type = super::SpiralWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            BrowserView::ensure_type();
            PathBar::ensure_type();
            PlacesSidebar::ensure_type();
            ProgressIndicator::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();

            klass.install_action("win.new-tab", None, |win, _, _| {
                let loc = win.current_view().and_then(|v| v.location());
                win.add_tab(
                    loc.as_ref()
                        .unwrap_or(&gio::File::for_path(glib::home_dir())),
                    true,
                );
            });
            klass.install_action("win.open-current-new-tab", None, |win, _, _| {
                if let Some(loc) = win.current_view().and_then(|v| v.location()) {
                    win.add_tab(&loc, true);
                }
            });
            klass.install_action("win.close-tab", None, |win, _, _| {
                if let Some(page) = win.imp().tab_view.selected_page() {
                    win.imp().tab_view.close_page(&page);
                }
            });
            klass.install_action("win.restore-tab", None, |win, _, _| {
                let loc = win.imp().closed_tabs.borrow_mut().pop();
                if let Some(loc) = loc {
                    win.add_tab(&loc, true);
                }
                win.action_set_enabled(
                    "win.restore-tab",
                    !win.imp().closed_tabs.borrow().is_empty(),
                );
            });
            klass.install_action("win.back", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.go_back()
                }
            });
            klass.install_action("win.forward", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.go_forward()
                }
            });
            klass.install_action("win.up", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.go_up()
                }
            });
            klass.install_action("win.home", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.go_to(&gio::File::for_path(glib::home_dir()))
                }
            });
            klass.install_action("win.reload", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.reload()
                }
            });
            klass.install_action("win.location-entry", None, |win, _, _| {
                win.show_location_entry()
            });
            klass.install_action("win.search", None, |win, _, _| {
                let b = &win.imp().search_button;
                b.set_active(!b.is_active());
            });
            klass.install_action("win.tab-overview", None, |win, _, _| {
                win.imp().tab_overview.set_open(true);
            });
            klass.install_action("win.toggle-view-mode", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.toggle_view_mode();
                }
            });
            klass.install_action("win.bookmark", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.action_group().activate_action("bookmark", None);
                }
            });
            klass.install_action("win.visible-columns", None, |win, _, _| {
                crate::dialogs::columns_dialog().present(Some(win));
            });
            klass.install_action("win.captions", None, |win, _, _| {
                crate::dialogs::captions_dialog().present(Some(win));
            });
            klass.install_action("win.zoom-in", None, |win, _, _| win.zoom(1));
            klass.install_action("win.zoom-out", None, |win, _, _| win.zoom(-1));

            use gtk::gdk::{Key, ModifierType as M};
            klass.add_binding_action(Key::Left, M::ALT_MASK, "win.back");
            klass.add_binding_action(Key::Right, M::ALT_MASK, "win.forward");
            klass.add_binding_action(Key::Up, M::ALT_MASK, "win.up");
            klass.add_binding_action(Key::BackSpace, M::empty(), "win.up");
            klass.add_binding_action(Key::Home, M::ALT_MASK, "win.home");
            klass.add_binding_action(Key::F5, M::empty(), "win.reload");
            klass.add_binding_action(Key::r, M::CONTROL_MASK, "win.reload");
            klass.add_binding_action(Key::l, M::CONTROL_MASK, "win.location-entry");
            klass.add_binding_action(Key::f, M::CONTROL_MASK, "win.search");
            klass.add_binding_action(Key::t, M::CONTROL_MASK, "win.new-tab");
            klass.add_binding_action(Key::w, M::CONTROL_MASK, "win.close-tab");
            klass.add_binding_action(Key::t, M::CONTROL_MASK | M::SHIFT_MASK, "win.restore-tab");
            klass.add_binding_action(Key::o, M::CONTROL_MASK | M::SHIFT_MASK, "win.tab-overview");
            klass.add_binding_action(Key::h, M::CONTROL_MASK, "win.show-hidden");
            klass.add_binding_action(Key::F9, M::empty(), "win.sidebar-visible");
            klass.add_binding_action(Key::d, M::CONTROL_MASK, "win.bookmark");
            klass.add_binding_action(Key::plus, M::CONTROL_MASK, "win.zoom-in");
            klass.add_binding_action(Key::equal, M::CONTROL_MASK, "win.zoom-in");
            klass.add_binding_action(Key::minus, M::CONTROL_MASK, "win.zoom-out");
            klass.add_binding_action(Key::_1, M::CONTROL_MASK, "win.toggle-view-mode");
            klass.add_binding_action(Key::_2, M::CONTROL_MASK, "win.toggle-view-mode");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SpiralWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            for key in ["show-hidden", "sidebar-visible"] {
                obj.add_action(&self.settings.create_action(key));
            }
            self.settings
                .bind("sidebar-visible", &*self.split_view, "show-sidebar")
                .build();

            // "sort" is a string action ("name-asc", "size-desc", ...) on the current view.
            self.sort_action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |_, v| {
                    let Some(v) = v.and_then(|v| v.str()) else {
                        return;
                    };
                    let Some((key, dir)) = v.split_once('-') else {
                        return;
                    };
                    if let Some(key) = SortKey::from_nick(key)
                        && let Some(view) = win.current_view()
                    {
                        view.set_sort(key, dir == "desc");
                    }
                }
            ));
            obj.add_action(&self.sort_action);
            obj.action_set_enabled("win.restore-tab", false);
            obj.sync_view_button();
            obj.zoom(0);

            // Location entry: Escape or losing focus returns to the crumbs.
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, k, _, _| {
                    let entry = &win.imp().location_entry;
                    if k == gtk::gdk::Key::Escape {
                        win.imp().on_location_entry_cancel(&gtk::Button::new());
                        glib::Propagation::Stop
                    } else if k == gtk::gdk::Key::Tab && entry.selection_bounds().is_some() {
                        // Accept the inline completion.
                        entry.set_position(-1);
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                }
            ));
            self.location_entry.add_controller(key);
            crate::location_entry::attach(&self.location_entry);
            let focus = gtk::EventControllerFocus::new();
            focus.connect_leave(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |_| {
                    let imp = win.imp();
                    if imp
                        .toolbar_switcher
                        .visible_child_name()
                        .is_some_and(|n| n == "location")
                    {
                        imp.toolbar_switcher.set_visible_child_name("pathbar");
                    }
                }
            ));
            self.location_entry.add_controller(focus);

            self.search_entry.set_key_capture_widget(Some(&*obj));
            self.search_entry.connect_search_started(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |_| win.imp().search_button.set_active(true)
            ));

            let (w, h) = self.settings.get::<(i32, i32)>("window-size");
            obj.set_default_size(w, h);
            if self.settings.boolean("window-maximized") {
                obj.maximize();
            }
        }
    }

    impl WidgetImpl for SpiralWindow {}

    impl WindowImpl for SpiralWindow {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            let (w, h) = obj.default_size();
            let _ = self.settings.set("window-size", (w, h));
            let _ = self
                .settings
                .set_boolean("window-maximized", obj.is_maximized());
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for SpiralWindow {}
    impl AdwApplicationWindowImpl for SpiralWindow {}

    #[gtk::template_callbacks]
    impl SpiralWindow {
        #[template_callback]
        fn on_create_tab(&self, _overview: &adw::TabOverview) -> adw::TabPage {
            let loc = self
                .obj()
                .current_view()
                .and_then(|v| v.location())
                .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
            self.obj().add_tab(&loc, true)
        }

        #[template_callback]
        fn on_path_bar_navigate(&self, file: &gio::File, _bar: &PathBar) {
            if let Some(v) = self.obj().current_view() {
                v.go_to(file);
                v.grab_view_focus();
            }
        }

        #[template_callback]
        fn on_edit_location(&self, _bar: &PathBar) {
            self.obj().show_location_entry();
        }

        #[template_callback]
        fn on_location_entry_activate(&self, entry: &gtk::Entry) {
            let text = entry.text();
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let file = if text.contains("://") {
                gio::File::for_uri(text)
            } else if let Some(rest) = text.strip_prefix('~') {
                gio::File::for_path(glib::home_dir())
                    .resolve_relative_path(rest.trim_start_matches('/'))
            } else {
                gio::File::for_commandline_arg(text)
            };
            self.toolbar_switcher.set_visible_child_name("pathbar");
            if let Some(v) = self.obj().current_view() {
                v.go_to(&file);
                v.grab_view_focus();
            }
        }

        #[template_callback]
        pub(super) fn on_location_entry_cancel(&self, _button: &gtk::Button) {
            self.toolbar_switcher.set_visible_child_name("pathbar");
            if let Some(v) = self.obj().current_view() {
                v.grab_view_focus();
            }
        }

        #[template_callback]
        fn on_sidebar_open_location(&self, file: &gio::File, new_tab: bool, _sb: &PlacesSidebar) {
            let obj = self.obj();
            match (new_tab, obj.current_view()) {
                (false, Some(v)) => {
                    v.go_to(file);
                    v.grab_view_focus();
                }
                _ => {
                    obj.add_tab(file, !new_tab);
                }
            }
            if self.split_view.is_collapsed() {
                self.split_view.set_show_sidebar(false);
            }
        }

        #[template_callback]
        fn on_selected_page_changed(&self, _pspec: glib::ParamSpec, _tv: &adw::TabView) {
            let obj = self.obj();
            if let Some(v) = obj.current_view() {
                obj.insert_action_group("view", Some(&v.imp().actions));
            }
            obj.sync_header();
            obj.sync_sort_state();
            obj.sync_view_button();
            obj.zoom(0);
        }

        #[template_callback]
        fn on_close_page(&self, page: &adw::TabPage, tab_view: &adw::TabView) -> bool {
            if let Some(loc) = page
                .child()
                .downcast_ref::<BrowserView>()
                .and_then(|v| v.location())
            {
                let mut closed = self.closed_tabs.borrow_mut();
                if closed.len() == CLOSED_TABS_MAX {
                    closed.remove(0);
                }
                closed.push(loc);
                self.obj().action_set_enabled("win.restore-tab", true);
            }
            tab_view.close_page_finish(page, true);
            if tab_view.n_pages() == 0 {
                self.obj().close();
            }
            true
        }

        #[template_callback]
        fn on_search_toggled(&self, button: &gtk::ToggleButton) {
            if button.is_active() {
                self.toolbar_switcher.set_visible_child_name("search");
                self.search_entry.grab_focus();
            } else {
                self.search_entry.set_text("");
                self.toolbar_switcher.set_visible_child_name("pathbar");
                if let Some(v) = self.obj().current_view() {
                    v.grab_view_focus();
                }
            }
        }

        #[template_callback]
        fn on_search_changed(&self, entry: &gtk::SearchEntry) {
            if let Some(v) = self.obj().current_view() {
                v.model().set_search_text(entry.text().as_str());
            }
        }

        #[template_callback]
        fn on_stop_search(&self, _entry: &gtk::SearchEntry) {
            self.search_button.set_active(false);
        }

        /// The filter rows write to the current tab's model; each tab keeps its own.
        #[template_callback]
        fn on_search_filter_changed(&self, _pspec: glib::ParamSpec, _row: &adw::ComboRow) {
            let Some(v) = self.obj().current_view() else {
                return;
            };
            let model = v.model();
            model.set_search_kind(crate::search::KINDS[self.search_kind_row.selected() as usize]);
            model.set_search_date(crate::search::DATES[self.search_date_row.selected() as usize].0);
            model.set_search_match(MATCHES[self.search_match_row.selected() as usize]);
        }
    }
}

glib::wrapper! {
    pub struct SpiralWindow(ObjectSubclass<imp::SpiralWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl SpiralWindow {
    pub fn new(app: &crate::application::SpiralApplication) -> Self {
        let win: Self = glib::Object::builder().property("application", app).build();
        // `application` is not a construct property, so it is still unset in `constructed`.
        win.imp().progress_indicator.set_manager(app.job_manager());
        win
    }

    pub fn show_toast(&self, message: &str, undoable: bool) {
        let toast = adw::Toast::new(message);
        if undoable {
            toast.set_button_label(Some(&gettext("Undo")));
            toast.set_action_name(Some("app.undo"));
        }
        self.imp().toast_overlay.add_toast(toast);
    }

    /// Pop up the file operations list (used when relaunched while jobs run).
    pub fn show_progress(&self) {
        let indicator = self.imp().progress_indicator.clone();
        glib::idle_add_local_once(move || {
            if indicator.has_jobs() {
                indicator.popup();
            }
        });
    }

    pub fn current_view(&self) -> Option<BrowserView> {
        self.imp()
            .tab_view
            .selected_page()
            .and_then(|p| p.child().downcast().ok())
    }

    /// Open `file` in the current tab, or a new tab if there is none.
    pub fn open_location(&self, file: &gio::File) {
        match self.current_view() {
            Some(v) if self.imp().tab_view.n_pages() == 1 && v.location().is_none() => {
                v.go_to(file)
            }
            _ => {
                self.add_tab(file, true);
            }
        }
    }

    pub fn add_tab(&self, file: &gio::File, select: bool) -> adw::TabPage {
        let imp = self.imp();
        let view = BrowserView::new(file);
        let page = imp.tab_view.append(&view);
        page.set_title(&file_utils::location_name(file));
        view.connect_notify_local(
            Some("location"),
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                page,
                move |v, _| {
                    if let Some(loc) = v.location() {
                        page.set_title(&file_utils::location_name(&loc));
                    }
                    if win.imp().tab_view.selected_page().as_ref() == Some(&page) {
                        win.sync_header();
                    }
                }
            ),
        );
        for prop in ["sort-key", "sort-reversed"] {
            view.model().connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = win)]
                    self,
                    #[weak]
                    page,
                    move |_, _| {
                        if win.imp().tab_view.selected_page().as_ref() == Some(&page) {
                            win.sync_sort_state();
                        }
                    }
                ),
            );
        }
        view.connect_open_in_new_tab(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, f| {
                win.add_tab(f, false);
            }
        ));
        let sync = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_: &BrowserView| win.sync_header()
        );
        view.connect_can_go_back_notify(sync.clone());
        view.connect_can_go_forward_notify(sync);
        view.connect_view_mode_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[weak]
            page,
            move |_| {
                if win.imp().tab_view.selected_page().as_ref() == Some(&page) {
                    win.sync_view_button();
                    win.zoom(0);
                }
            }
        ));
        if select {
            imp.tab_view.set_selected_page(&page);
            view.grab_view_focus();
        }
        page
    }

    fn show_location_entry(&self) {
        let imp = self.imp();
        let text = self
            .current_view()
            .and_then(|v| v.location())
            .map(|f| match f.path() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => f.uri().to_string(),
            })
            .unwrap_or_default();
        imp.search_button.set_active(false);
        crate::location_entry::set_text_quiet(&imp.location_entry, &text);
        imp.toolbar_switcher.set_visible_child_name("location");
        imp.location_entry.grab_focus();
        imp.location_entry.set_position(-1);
    }

    fn zoom(&self, step: i32) {
        let s = &self.imp().settings;
        let grid = self
            .current_view()
            .is_none_or(|v| v.view_mode() == ViewMode::Grid);
        let (key, sizes) = if grid {
            ("grid-zoom", &GRID_ZOOM_SIZES)
        } else {
            ("list-zoom", &LIST_ZOOM_SIZES)
        };
        let current = s.int(key);
        let idx = sizes.iter().position(|&z| z >= current).unwrap_or(2) as i32;
        let next = (idx + step).clamp(0, sizes.len() as i32 - 1);
        let _ = s.set_int(key, sizes[next as usize]);
        self.action_set_enabled("win.zoom-in", next < sizes.len() as i32 - 1);
        self.action_set_enabled("win.zoom-out", next > 0);
    }

    fn sync_sort_state(&self) {
        let Some(view) = self.current_view() else {
            return;
        };
        let model = view.model();
        let dir = if model.sort_reversed() { "desc" } else { "asc" };
        self.imp()
            .sort_action
            .set_state(&format!("{}-{dir}", model.sort_key().nick()).to_variant());
    }

    /// The split button shows the view you switch *to*, like Nautilus.
    fn sync_view_button(&self) {
        let imp = self.imp();
        let grid = self
            .current_view()
            .is_none_or(|v| v.view_mode() == ViewMode::Grid);
        imp.view_split_button.set_icon_name(if grid {
            "view-list-symbolic"
        } else {
            "view-grid-symbolic"
        });
        imp.view_split_button.set_tooltip_text(Some(&if grid {
            gettext("List View")
        } else {
            gettext("Grid View")
        }));
        // Each view has its own dialog: captions under the grid icons, columns of the list.
        self.action_set_enabled("win.captions", grid);
        self.action_set_enabled("win.visible-columns", !grid);
    }

    /// Refresh header widgets from the selected tab.
    fn sync_header(&self) {
        let imp = self.imp();
        let Some(view) = self.current_view() else {
            return;
        };
        let loc = view.location();
        imp.path_bar.set_location(loc.as_ref());
        imp.sidebar.set_selected_location(loc.as_ref());
        self.action_set_enabled("win.back", view.can_go_back());
        self.action_set_enabled("win.forward", view.can_go_forward());
        self.set_title(Some(
            &loc.map(|l| file_utils::location_name(&l))
                .unwrap_or_default(),
        ));
        let model = view.model();
        let search = model.search_text();
        if imp.search_entry.text().as_str() != search {
            imp.search_entry.set_text(&search);
        }
        let pos = |list: &[&str], v: &str| list.iter().position(|k| *k == v).unwrap_or(0) as u32;
        imp.search_kind_row
            .set_selected(pos(&crate::search::KINDS, &model.search_kind()));
        let dates: Vec<&str> = crate::search::DATES.iter().map(|(n, _)| *n).collect();
        imp.search_date_row
            .set_selected(pos(&dates, &model.search_date()));
        imp.search_match_row
            .set_selected(pos(&MATCHES, &model.search_match()));
        if search.is_empty() && imp.search_button.is_active() {
            imp.search_button.set_active(false);
        }
    }
}
