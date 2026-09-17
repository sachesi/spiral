//! The file manager's window: tabs of browser views, the sidebar, the header bar with the
//! location entry and search, the details panel, and the `win.*` actions.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use std::cell::RefCell;

use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::details_panel::DetailsPanel;
use crate::enums::{SortKey, ViewMode};
use crate::file_utils;
use crate::path_bar::PathBar;
use crate::places_sidebar::PlacesSidebar;
use crate::progress_indicator::ProgressIndicator;
use crate::{adw, gio, glib, gtk};

mod actions;
mod header;
mod panes;
mod tabs;

/// Search match modes in the order of the "Match" row.
const MATCHES: [&str; 3] = ["name", "both", "content"];
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
        pub details_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub details_sidebar: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub details: TemplateChild<DetailsPanel>,
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
        pub view_mode_action: gio::SimpleAction,
        /// The pane the header and the shortcuts act on, when a tab has two.
        pub active_view: RefCell<Option<glib::WeakRef<BrowserView>>>,
        /// Set while the window is too narrow for a second pane.
        pub narrow: std::cell::Cell<bool>,
        /// Set while the window is too narrow for the details panel.
        pub cramped: std::cell::Cell<bool>,
        /// Where each tab's second pane was looking when it was folded away.
        pub folded: RefCell<Vec<(glib::WeakRef<adw::TabPage>, gio::File)>>,
        /// Locations of tabs closed in this window, most recent last.
        pub closed_tabs: RefCell<Vec<gio::File>>,
        /// The tab whose context menu is open, if any.
        pub menu_page: RefCell<Option<adw::TabPage>>,
        /// Set while the search filter rows are being updated from a tab, not the user.
        pub syncing_search: std::cell::Cell<bool>,
    }

    impl Default for SpiralWindow {
        fn default() -> Self {
            Self {
                toast_overlay: Default::default(),
                tab_overview: Default::default(),
                tab_view: Default::default(),
                split_view: Default::default(),
                sidebar: Default::default(),
                details_view: Default::default(),
                details_sidebar: Default::default(),
                details: Default::default(),
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
                view_mode_action: gio::SimpleAction::new_stateful(
                    "view-mode",
                    Some(glib::VariantTy::STRING),
                    &"grid".to_variant(),
                ),
                active_view: Default::default(),
                narrow: Default::default(),
                cramped: Default::default(),
                folded: Default::default(),
                closed_tabs: Default::default(),
                menu_page: Default::default(),
                syncing_search: Default::default(),
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
            DetailsPanel::ensure_type();
            PathBar::ensure_type();
            PlacesSidebar::ensure_type();
            ProgressIndicator::ensure_type();
            klass.bind_template();
            klass.bind_template_callbacks();
            actions::install(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SpiralWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.add_action(&self.settings.create_action("show-hidden"));
            // The panels belong to the window they are toggled in; the setting is only what
            // the next window starts with.
            for (key, apply) in [
                (
                    "sidebar-visible",
                    super::SpiralWindow::apply_sidebar as fn(&super::SpiralWindow),
                ),
                ("details-visible", super::SpiralWindow::apply_details),
                ("split-view", super::SpiralWindow::apply_split),
            ] {
                let action = gio::SimpleAction::new_stateful(
                    key,
                    None,
                    &self.settings.boolean(key).to_variant(),
                );
                action.connect_change_state(glib::clone!(
                    #[weak(rename_to = win)]
                    obj,
                    move |action, value| {
                        let Some(value) = value else { return };
                        action.set_state(value);
                        let _ = win.imp().settings.set_value(key, value);
                        apply(&win);
                    }
                ));
                obj.add_action(&action);
            }
            obj.action_set_enabled("win.stop", false);
            obj.action_set_enabled("win.close-search", false);
            // The main menu takes F10 otherwise, ahead of the binding for the folder menu.
            obj.set_handle_menubar_accel(false);
            obj.apply_details();
            // Which pane is in charge follows the focus, and stays put while the focus is
            // off in the sidebar or the path bar.
            obj.connect_focus_widget_notify(|win| {
                let Some(focus) = gtk::prelude::GtkWindowExt::focus(win) else {
                    return;
                };
                let view = focus
                    .downcast_ref::<BrowserView>()
                    .cloned()
                    .or_else(|| focus.ancestor(BrowserView::static_type()).and_downcast());
                if let Some(view) = view {
                    win.set_active_view(&view);
                }
            });
            obj.apply_sidebar();

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
            self.view_mode_action.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |_, v| {
                    if let Some(mode) = v.and_then(|v| v.str()).and_then(ViewMode::from_nick)
                        && let Some(view) = win.current_view()
                    {
                        view.choose_view_mode(mode);
                        view.grab_view_focus();
                    }
                }
            ));
            obj.add_action(&self.view_mode_action);
            obj.action_set_enabled("win.restore-tab", false);
            obj.action_set_enabled("win.connect-server", crate::prefs::use_network());
            self.settings.connect_changed(
                Some("use-network"),
                glib::clone!(
                    #[weak(rename_to = win)]
                    obj,
                    move |_, _| {
                        win.action_set_enabled("win.connect-server", crate::prefs::use_network());
                    }
                ),
            );
            obj.sync_columns_item();
            self.settings.connect_changed(
                Some("use-column-view"),
                glib::clone!(
                    #[weak(rename_to = win)]
                    obj,
                    move |_, _| {
                        win.sync_columns_item();
                        win.sync_view_button();
                    }
                ),
            );
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
                    } else if matches!(k, gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab) {
                        // Accept the inline completion. Tab never moves the focus on:
                        // leaving the entry puts the crumbs back, which loses the path
                        // half typed whenever the completion had nothing to add yet.
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
            // Down goes from the search box to the result that is selected, where the
            // arrows go on from; GTK would hand the keyboard to the column headers.
            let down = gtk::EventControllerKey::new();
            down.set_propagation_phase(gtk::PropagationPhase::Capture);
            down.connect_key_pressed(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, state| {
                    let Some(v) = win.current_view() else {
                        return glib::Propagation::Proceed;
                    };
                    let model = v.model();
                    if !matches!(key, gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down)
                        || !state.is_empty()
                        || model.n_items() == 0
                    {
                        return glib::Propagation::Proceed;
                    }
                    let selected = model.selection().selection();
                    let flags = if selected.is_empty() {
                        gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT
                    } else {
                        gtk::ListScrollFlags::FOCUS
                    };
                    v.grab_view_focus();
                    v.reveal_position(selected.minimum().min(model.n_items() - 1), flags);
                    glib::Propagation::Stop
                }
            ));
            self.search_entry.add_controller(down);
            // Enter opens the result that is selected, the first one unless another was
            // picked.
            self.search_entry.connect_activate(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |_| {
                    if let Some(v) = win.current_view()
                        && v.model().searching()
                    {
                        let _ = v.activate_action("view.open", None);
                    }
                }
            ));

            // The side buttons of a mouse go back and forward, wherever the pointer is.
            let buttons = gtk::GestureClick::builder().button(0).build();
            buttons.set_propagation_phase(gtk::PropagationPhase::Capture);
            buttons.connect_pressed(glib::clone!(
                #[weak(rename_to = win)]
                obj,
                move |gesture, _, _, _| {
                    let action = match gesture.current_button() {
                        8 => "win.back",
                        9 => "win.forward",
                        _ => return,
                    };
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let _ = WidgetExt::activate_action(&win, action, None);
                }
            ));
            obj.add_controller(buttons);

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
            let file = crate::location_entry::resolve(text);
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
            self.obj().refresh_active();
        }

        /// Too narrow for two panes: fold the second one away, keeping the setting.
        #[template_callback]
        fn on_narrow(&self, _breakpoint: &adw::Breakpoint) {
            self.narrow.set(true);
            self.obj().apply_split();
            self.obj().apply_details();
        }

        #[template_callback]
        fn on_wide(&self, _breakpoint: &adw::Breakpoint) {
            self.narrow.set(false);
            self.obj().apply_split();
            self.obj().apply_details();
        }

        /// Expanding again shows the sidebar whatever it was, and does so after `on_wide`
        /// has run: put the window's own toggle back once it has.
        #[template_callback]
        fn on_collapsed_changed(
            &self,
            _pspec: glib::ParamSpec,
            split_view: &adw::OverlaySplitView,
        ) {
            if !split_view.is_collapsed() {
                self.obj().apply_sidebar();
            }
        }

        /// Too narrow for the details panel: hide it, keeping the setting.
        #[template_callback]
        fn on_cramped(&self, _breakpoint: &adw::Breakpoint) {
            self.cramped.set(true);
            self.obj().apply_details();
        }

        #[template_callback]
        fn on_roomy(&self, _breakpoint: &adw::Breakpoint) {
            self.cramped.set(false);
            self.obj().apply_details();
        }

        /// The tab menu is opening for `page`, or closing when it is `None`.
        #[template_callback]
        fn on_setup_menu(&self, page: Option<&adw::TabPage>, tab_view: &adw::TabView) {
            self.menu_page.replace(page.cloned());
            let obj = self.obj();
            let pos = obj.menu_page().map_or(0, |p| tab_view.page_position(&p));
            obj.action_set_enabled("win.tab-move-left", pos > 0);
            obj.action_set_enabled("win.tab-move-right", pos < tab_view.n_pages() - 1);
        }

        /// A tab dragged out of the tab bar, or "Move Tab to New Window".
        #[template_callback]
        pub(super) fn on_create_window(&self, _tab_view: &adw::TabView) -> Option<adw::TabView> {
            let app = self
                .obj()
                .application()
                .and_downcast::<SpiralApplication>()?;
            Some(app.new_window().imp().tab_view.clone())
        }

        /// A tab brought over from another window takes this window's second pane, or its
        /// lack of one. A new tab is attached before it has a view, and sees to it itself.
        #[template_callback]
        fn on_page_attached(&self, page: &adw::TabPage, _pos: i32, _tab_view: &adw::TabView) {
            if page
                .child()
                .downcast_ref::<gtk::Paned>()
                .is_some_and(|p| p.start_child().is_some())
            {
                self.obj().apply_split();
            }
        }

        /// Closed or moved to another window: the last tab leaving closes the window.
        #[template_callback]
        fn on_page_detached(&self, _page: &adw::TabPage, _pos: i32, tab_view: &adw::TabView) {
            if tab_view.n_pages() == 0 {
                self.obj().close();
            }
        }

        #[template_callback]
        fn on_close_page(&self, page: &adw::TabPage, tab_view: &adw::TabView) -> bool {
            if let Some(loc) = super::SpiralWindow::views_of(page)
                .first()
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
            true
        }

        #[template_callback]
        fn on_search_toggled(&self, button: &gtk::ToggleButton) {
            if button.is_active() {
                self.toolbar_switcher.set_visible_child_name("search");
                // Shown again for a search coming back with the folder, the keyboard stays
                // with the files.
                if !self.syncing_search.get() {
                    self.search_entry.grab_focus();
                }
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
                v.search_for(entry.text().as_str());
            }
        }

        #[template_callback]
        fn on_stop_search(&self, _entry: &gtk::SearchEntry) {
            self.search_button.set_active(false);
        }

        /// The filter rows write to the current tab's model; each tab keeps its own.
        #[template_callback]
        fn on_search_filter_changed(&self, _pspec: glib::ParamSpec, _row: &adw::ComboRow) {
            if self.syncing_search.get() {
                return;
            }
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
    /// Say that an operation is done with what it put in `folder`: with a way there, which
    /// selects the files that `landed`, when the folder is not the one on screen, and on
    /// its own when it is but the operation was `slow`. Files that appear in the folder
    /// being looked at say it themselves otherwise.
    pub fn show_done_toast(
        &self,
        message: &str,
        folder: &gio::File,
        landed: Vec<gio::File>,
        slow: bool,
    ) {
        let here = self
            .current_view()
            .and_then(|v| v.location())
            .is_some_and(|l| l.equal(folder));
        if here && !slow {
            return;
        }
        let toast = adw::Toast::new(message);
        if !here {
            toast.set_button_label(Some(&gettext("Open Folder")));
            toast.connect_button_clicked(glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[strong]
                folder,
                move |_| {
                    win.navigate(|v| {
                        v.go_to(&folder);
                        v.select_files_when_loaded(landed.clone());
                    })
                }
            ));
        }
        self.imp().toast_overlay.add_toast(toast);
    }

    /// Show `folder` in the pane in charge, with `select` selected in it.
    pub(crate) fn reveal(&self, folder: &gio::File, select: Vec<gio::File>) {
        self.navigate(|v| {
            v.go_to(folder);
            v.select_files_when_loaded(select.clone());
        });
    }

    /// A toast whose Undo button runs `undo`, for a change the undo of the operations does
    /// not keep.
    pub fn show_undo_toast(&self, message: &str, undo: impl Fn() + 'static) {
        let toast = adw::Toast::new(message);
        toast.set_button_label(Some(&gettext("Undo")));
        toast.connect_button_clicked(move |_| undo());
        self.imp().toast_overlay.add_toast(toast);
    }

    pub fn show_progress(&self) {
        let indicator = self.imp().progress_indicator.clone();
        glib::idle_add_local_once(move || {
            if indicator.has_jobs() {
                indicator.popup();
            }
        });
    }

    /// The pane everything outside the view acts on: the active one when it belongs to the
    /// tab on screen, else that tab's left pane.
    pub(crate) fn sidebar(&self) -> PlacesSidebar {
        self.imp().sidebar.clone()
    }
}
