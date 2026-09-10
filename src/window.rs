use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use std::cell::RefCell;

use crate::application::SpiralApplication;
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
        pub view_mode_action: gio::SimpleAction,
        /// The pane the header and the shortcuts act on, when a tab has two.
        pub active_view: RefCell<Option<glib::WeakRef<BrowserView>>>,
        /// Set while the window is too narrow for a second pane.
        pub narrow: std::cell::Cell<bool>,
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
                if let Some(page) = win.menu_page() {
                    win.imp().tab_view.close_page(&page);
                }
            });
            klass.install_action("win.close-other-tabs", None, |win, _, _| {
                if let Some(page) = win.menu_page() {
                    win.imp().tab_view.close_other_pages(&page);
                }
            });
            klass.install_action("win.tab-move-left", None, |win, _, _| {
                if let Some(page) = win.menu_page() {
                    win.imp().tab_view.reorder_backward(&page);
                }
            });
            klass.install_action("win.tab-move-right", None, |win, _, _| {
                if let Some(page) = win.menu_page() {
                    win.imp().tab_view.reorder_forward(&page);
                }
            });
            klass.install_action("win.tab-move-new-window", None, |win, _, _| {
                if let Some(page) = win.menu_page()
                    && let Some(other) = win.imp().on_create_window(&win.imp().tab_view)
                {
                    win.imp().tab_view.transfer_page(&page, &other, 0);
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
                win.navigate(BrowserView::go_back)
            });
            klass.install_action("win.forward", None, |win, _, _| {
                win.navigate(BrowserView::go_forward)
            });
            klass.install_action("win.up", None, |win, _, _| win.navigate(BrowserView::go_up));
            klass.install_action("win.back-or-up", None, |win, _, _| {
                win.navigate(BrowserView::go_back_or_up)
            });
            klass.install_action("win.home", None, |win, _, _| {
                win.navigate(|v| v.go_to(&gio::File::for_path(glib::home_dir())))
            });
            klass.install_action("win.reload", None, |win, _, _| {
                win.navigate(BrowserView::reload)
            });
            klass.install_action("win.location-entry", None, |win, _, _| {
                win.show_location_entry()
            });
            klass.install_action("win.search", None, |win, _, _| {
                let b = &win.imp().search_button;
                b.set_active(!b.is_active());
            });
            klass.install_action("win.close-search", None, |win, _, _| {
                win.imp().search_button.set_active(false);
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
            klass.install_action("win.connect-server", None, |win, _, _| {
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    win,
                    async move {
                        // Where the sidebar and the location bar go: the tab in front.
                        if let Some(file) = crate::dialogs::connect_server_dialog(&win).await {
                            match win.current_view() {
                                Some(view) => view.go_to(&file),
                                None => {
                                    win.add_tab(&file, true);
                                }
                            }
                        }
                    }
                ));
            });
            klass.install_action("win.visible-columns", None, |win, _, _| {
                crate::dialogs::columns_dialog().present(Some(win));
            });
            klass.install_action("win.captions", None, |win, _, _| {
                crate::dialogs::captions_dialog().present(Some(win));
            });
            klass.install_action("win.switch-pane", None, |win, _, _| win.switch_pane());
            klass.install_action("win.zoom-in", None, |win, _, _| win.zoom(1));
            klass.install_action("win.zoom-out", None, |win, _, _| win.zoom(-1));
            klass.install_action("win.zoom-reset", None, |win, _, _| win.zoom_reset());
            klass.install_action("win.stop", None, |win, _, _| {
                if let Some(v) = win.current_view() {
                    v.model().stop_loading();
                }
            });
            klass.install_action(
                "win.go-to-tab",
                Some(glib::VariantTy::INT32),
                |win, _, param| {
                    if let Some(n) = param.and_then(|p| p.get::<i32>()) {
                        win.go_to_tab(n);
                    }
                },
            );

            use gtk::gdk::{Key, ModifierType as M};
            klass.add_binding_action(Key::Left, M::ALT_MASK, "win.back");
            klass.add_binding_action(Key::Right, M::ALT_MASK, "win.forward");
            klass.add_binding_action(Key::Up, M::ALT_MASK, "win.up");
            klass.add_binding_action(Key::BackSpace, M::empty(), "win.back-or-up");
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
            klass.add_binding_action(Key::F3, M::empty(), "win.split-view");
            klass.add_binding_action(Key::F6, M::empty(), "win.switch-pane");
            klass.add_binding_action(Key::d, M::CONTROL_MASK, "win.bookmark");
            klass.add_binding_action(Key::plus, M::CONTROL_MASK, "win.zoom-in");
            klass.add_binding_action(Key::equal, M::CONTROL_MASK, "win.zoom-in");
            klass.add_binding_action(Key::minus, M::CONTROL_MASK, "win.zoom-out");
            klass.add_binding_action(Key::_0, M::CONTROL_MASK, "win.zoom-reset");
            klass.add_binding_action(Key::KP_0, M::CONTROL_MASK, "win.zoom-reset");
            // Only bound while something is loading: the action is disabled otherwise, and
            // Escape goes on to whatever else wants it.
            klass.add_binding_action(Key::Escape, M::empty(), "win.stop");
            // With nothing loading, Escape ends a search from wherever the keyboard is, as
            // it does in the search box.
            klass.add_binding_action(Key::Escape, M::empty(), "win.close-search");
            // Alt and a digit for the first nine tabs.
            for (i, key) in [
                Key::_1,
                Key::_2,
                Key::_3,
                Key::_4,
                Key::_5,
                Key::_6,
                Key::_7,
                Key::_8,
                Key::_9,
            ]
            .into_iter()
            .enumerate()
            {
                klass.add_shortcut(
                    &gtk::Shortcut::builder()
                        .trigger(&gtk::KeyvalTrigger::new(key, M::ALT_MASK))
                        .action(&gtk::NamedAction::new("win.go-to-tab"))
                        .arguments(&(i as i32).to_variant())
                        .build(),
                );
            }
            for (key, mode) in [
                (Key::_1, ViewMode::Grid),
                (Key::_2, ViewMode::List),
                (Key::_3, ViewMode::Columns),
            ] {
                // A named action takes its target from the shortcut, which the plain
                // `add_binding_action` has no room for.
                klass.add_shortcut(
                    &gtk::Shortcut::builder()
                        .trigger(&gtk::KeyvalTrigger::new(key, M::CONTROL_MASK))
                        .action(&gtk::NamedAction::new("win.view-mode"))
                        .arguments(&mode.nick().to_variant())
                        .build(),
                );
            }
            // Also at window level so the clipboard keys work with the focus anywhere
            // outside a text entry: the sidebar, the path bar, the tab bar.
            klass.add_binding_action(Key::c, M::CONTROL_MASK, "view.copy");
            klass.add_binding_action(Key::x, M::CONTROL_MASK, "view.cut");
            klass.add_binding_action(Key::v, M::CONTROL_MASK, "view.paste");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SpiralWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            for key in ["show-hidden", "sidebar-visible", "split-view"] {
                obj.add_action(&self.settings.create_action(key));
            }
            obj.action_set_enabled("win.stop", false);
            obj.action_set_enabled("win.close-search", false);
            self.settings.connect_changed(
                Some("split-view"),
                glib::clone!(
                    #[weak(rename_to = win)]
                    obj,
                    move |_, _| win.apply_split()
                ),
            );
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
        }

        #[template_callback]
        fn on_wide(&self, _breakpoint: &adw::Breakpoint) {
            self.narrow.set(false);
            self.obj().apply_split();
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
    pub fn show_progress(&self) {
        let indicator = self.imp().progress_indicator.clone();
        glib::idle_add_local_once(move || {
            if indicator.has_jobs() {
                indicator.popup();
            }
        });
    }

    /// The window a view currently sits in; tabs move between windows, so the
    /// handlers in `add_tab` look it up instead of capturing it.
    fn of(widget: &impl IsA<gtk::Widget>) -> Option<Self> {
        widget.root().and_downcast()
    }

    /// The tab the context menu was opened for, else the selected one.
    fn menu_page(&self) -> Option<adw::TabPage> {
        let imp = self.imp();
        imp.menu_page
            .borrow()
            .clone()
            .or_else(|| imp.tab_view.selected_page())
    }

    /// The panes of a tab, left to right.
    pub(crate) fn views_of(page: &adw::TabPage) -> Vec<BrowserView> {
        let Ok(paned) = page.child().downcast::<gtk::Paned>() else {
            return Vec::new();
        };
        [paned.start_child(), paned.end_child()]
            .into_iter()
            .flatten()
            .filter_map(|c| c.downcast().ok())
            .collect()
    }

    /// The pane everything outside the view acts on: the active one when it belongs to the
    /// tab on screen, else that tab's left pane.
    pub(crate) fn sidebar(&self) -> PlacesSidebar {
        self.imp().sidebar.clone()
    }

    pub fn current_view(&self) -> Option<BrowserView> {
        let page = self.imp().tab_view.selected_page()?;
        let paned = page.child().downcast::<gtk::Paned>().ok()?;
        let active = self
            .imp()
            .active_view
            .borrow()
            .as_ref()
            .and_then(|w| w.upgrade());
        match active {
            Some(v) if v.is_ancestor(&paned) => Some(v),
            _ => paned.start_child().and_downcast(),
        }
    }

    /// Navigate the pane in charge and hand it the focus: a toolbar button keeps the
    /// focus otherwise, and the view keys only fire while a pane holds it.
    fn navigate(&self, f: impl Fn(&BrowserView)) {
        if let Some(v) = self.current_view() {
            f(&v);
            v.grab_view_focus();
        }
    }

    fn set_active_view(&self, view: &BrowserView) {
        let same = self
            .imp()
            .active_view
            .borrow()
            .as_ref()
            .and_then(|w| w.upgrade())
            .is_some_and(|v| &v == view);
        if same {
            return;
        }
        self.imp().active_view.replace(Some(view.downgrade()));
        self.refresh_active();
        // The focus has to follow, not just the outline: `view.copy` and friends resolve
        // from the focused widget, and a focused pane's own actions shadow the window's.
        view.grab_view_focus();
    }

    /// Point the header, the sort state and the `view` actions at the pane in charge.
    fn refresh_active(&self) {
        let imp = self.imp();
        let stale = imp
            .active_view
            .borrow()
            .as_ref()
            .and_then(|w| w.upgrade())
            .is_none_or(|v| v.root().is_none());
        if stale {
            imp.active_view.replace(None);
        }
        let Some(view) = self.current_view() else {
            return;
        };
        imp.active_view.replace(Some(view.downgrade()));
        self.insert_action_group("view", Some(&view.imp().actions));
        if let Some(page) = imp.tab_view.selected_page()
            && view.location().is_some()
        {
            page.set_title(&view.location_title());
        }
        self.mark_panes();
        self.sync_header();
        self.sync_sort_state();
        self.sync_view_button();
        self.zoom(0);
    }

    /// Outline the pane in charge, so it is clear what the header acts on.
    fn mark_panes(&self) {
        let Some(page) = self.imp().tab_view.selected_page() else {
            return;
        };
        let views = Self::views_of(&page);
        let split = views.len() > 1;
        let current = self.current_view();
        for view in &views {
            if split && current.as_ref() == Some(view) {
                view.add_css_class("spiral-pane-active");
            } else {
                view.remove_css_class("spiral-pane-active");
            }
        }
    }

    /// Give every tab a second pane, or take it away, following the setting.
    fn apply_split(&self) {
        let imp = self.imp();
        let want = imp.settings.boolean("split-view") && !imp.narrow.get();
        for i in 0..imp.tab_view.n_pages() {
            let page = imp.tab_view.nth_page(i);
            let Ok(paned) = page.child().downcast::<gtk::Paned>() else {
                continue;
            };
            match (want, paned.end_child()) {
                (true, None) => {
                    let loc = self
                        .take_folded(&page)
                        .or_else(|| {
                            paned
                                .start_child()
                                .and_downcast::<BrowserView>()
                                .and_then(|v| v.location())
                        })
                        .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
                    let view = BrowserView::new(&loc);
                    self.attach_view(&page, &view);
                    paned.set_end_child(Some(&view));
                }
                (false, Some(child)) => {
                    // Folding the pane away throws the view out, so keep the folder it was
                    // showing and open there again rather than beside the left pane.
                    if let Some(loc) = child
                        .downcast_ref::<BrowserView>()
                        .and_then(|v| v.location())
                    {
                        self.remember_folded(&page, loc);
                    }
                    paned.set_end_child(gtk::Widget::NONE);
                }
                _ => {}
            }
        }
        self.refresh_active();
    }

    fn remember_folded(&self, page: &adw::TabPage, location: gio::File) {
        let mut folded = self.imp().folded.borrow_mut();
        folded.retain(|(p, _)| p.upgrade().is_some_and(|p| &p != page));
        folded.push((page.downgrade(), location));
    }

    fn take_folded(&self, page: &adw::TabPage) -> Option<gio::File> {
        let mut folded = self.imp().folded.borrow_mut();
        let at = folded
            .iter()
            .position(|(p, _)| p.upgrade().as_ref() == Some(page))?;
        Some(folded.remove(at).1)
    }

    /// F6: hand the focus to the other pane.
    fn switch_pane(&self) {
        let Some(page) = self.imp().tab_view.selected_page() else {
            return;
        };
        let views = Self::views_of(&page);
        if views.len() < 2 {
            return;
        }
        let current = self.current_view();
        let next = if current.as_ref() == Some(&views[0]) {
            &views[1]
        } else {
            &views[0]
        };
        next.grab_view_focus();
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
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .resize_start_child(true)
            .resize_end_child(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();
        // Half and half once the tab has a width of its own; dragging the handle sticks.
        paned.add_tick_callback(|p, _| {
            if p.width() == 0 {
                return glib::ControlFlow::Continue;
            }
            if !p.is_position_set() {
                p.set_position(p.width() / 2);
            }
            glib::ControlFlow::Break
        });
        let view = BrowserView::new(file);
        let page = imp.tab_view.append(&paned);
        page.set_title(&file_utils::location_name(file));
        self.attach_view(&page, &view);
        paned.set_start_child(Some(&view));
        if select {
            imp.tab_view.set_selected_page(&page);
            view.grab_view_focus();
        }
        self.apply_split();
        page
    }

    /// Keep the header in step with one pane. Every handler asks whether the pane is the
    /// one in charge, since a tab can hold two.
    fn attach_view(&self, page: &adw::TabPage, view: &BrowserView) {
        // Clicking a pane puts it in charge even where there is nothing to focus: the empty
        // space below the files. Capture so the view's own gestures still see the press.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_button(0);
        click.connect_pressed(glib::clone!(
            #[weak]
            view,
            move |_, _, _, _| {
                if let Some(win) = Self::of(&view) {
                    win.set_active_view(&view);
                }
            }
        ));
        view.add_controller(click);
        view.connect_notify_local(
            Some("location"),
            glib::clone!(
                #[weak]
                page,
                move |v, _| {
                    let Some(win) = Self::of(v) else { return };
                    if win.current_view().as_ref() != Some(v) {
                        return;
                    }
                    if v.location().is_some() {
                        page.set_title(&v.location_title());
                    }
                    win.sync_header();
                }
            ),
        );
        for prop in ["sort-key", "sort-reversed"] {
            view.model().connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak]
                    view,
                    move |_, _| {
                        if let Some(win) = Self::of(&view)
                            && win.current_view().as_ref() == Some(&view)
                        {
                            win.sync_sort_state();
                        }
                    }
                ),
            );
        }
        view.connect_open_in_new_tab(|v, f| {
            if let Some(win) = Self::of(v) {
                win.add_tab(f, false);
            }
        });
        let sync = |v: &BrowserView| {
            if let Some(win) = Self::of(v)
                && win.current_view().as_ref() == Some(v)
            {
                win.sync_header();
            }
        };
        // Escape stops a folder or a search that is still coming in; with nothing loading
        // the action is off and the key goes elsewhere.
        view.model().connect_loading_notify(glib::clone!(
            #[weak]
            view,
            move |model| {
                if let Some(win) = Self::of(&view)
                    && win.current_view().as_ref() == Some(&view)
                {
                    win.action_set_enabled("win.stop", model.loading());
                }
            }
        ));
        view.connect_can_go_back_notify(sync);
        view.connect_can_go_forward_notify(sync);
        // A search the view ends itself, going back out of it, takes the search bar with it.
        view.model().connect_searching_notify(glib::clone!(
            #[weak]
            view,
            move |_| sync(&view)
        ));
        view.connect_view_mode_notify(|v| {
            if let Some(win) = Self::of(v)
                && win.current_view().as_ref() == Some(v)
            {
                win.sync_view_button();
                win.zoom(0);
            }
        });
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

    /// Back to the zoom the preference starts at, for the view on screen.
    fn zoom_reset(&self) {
        let grid = self
            .current_view()
            .is_none_or(|v| v.view_mode() == ViewMode::Grid);
        self.imp()
            .settings
            .reset(if grid { "grid-zoom" } else { "list-zoom" });
        self.zoom(0);
    }

    fn go_to_tab(&self, index: i32) {
        let tabs = &self.imp().tab_view;
        if index >= 0 && index < tabs.n_pages() {
            tabs.set_selected_page(&tabs.nth_page(index));
        }
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
        // Called with no step to light the buttons up, which must not write the setting:
        // every window would store the size it just read.
        if sizes[next as usize] != current {
            let _ = s.set_int(key, sizes[next as usize]);
        }
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

    /// The column view is optional: turned off, its item leaves the view menu so the menu
    /// lists the views there actually are. Both split buttons share the one menu.
    fn sync_columns_item(&self) {
        let Some(section) = self
            .imp()
            .view_split_button
            .popover()
            .and_downcast::<gtk::PopoverMenu>()
            .and_then(|p| p.menu_model())
            .and_then(|m| m.item_link(0, gio::MENU_LINK_SECTION.as_str()))
            .and_downcast::<gio::Menu>()
        else {
            return;
        };
        match (crate::prefs::column_view(), section.n_items()) {
            (true, 2) => {
                let item = gio::MenuItem::new(Some(&gettext("_Columns")), None);
                item.set_action_and_target_value(
                    Some("win.view-mode"),
                    Some(&"columns".to_variant()),
                );
                section.append_item(&item);
            }
            (false, 3) => section.remove(2),
            _ => {}
        }
    }

    /// The split button shows the view you switch *to*, like Nautilus.
    fn sync_view_button(&self) {
        let imp = self.imp();
        let mode = self
            .current_view()
            .map(|v| v.view_mode())
            .unwrap_or_default();
        let next = mode.next();
        for button in [&imp.view_split_button, &imp.view_split_button_bottom] {
            button.set_icon_name(next.icon());
            button.set_tooltip_text(Some(&next.label()));
        }
        imp.view_mode_action.set_state(&mode.nick().to_variant());
        // Each view has its own dialog: captions under the grid icons, columns of the
        // list. The Miller columns show names only.
        self.action_set_enabled("win.captions", mode == ViewMode::Grid);
        self.action_set_enabled("win.visible-columns", mode == ViewMode::List);
    }

    /// Refresh header widgets from the selected tab.
    pub(crate) fn sync_header(&self) {
        let imp = self.imp();
        let Some(view) = self.current_view() else {
            return;
        };
        let loc = view.location();
        imp.path_bar.set_given_name(view.given_name());
        imp.path_bar.set_location(loc.as_ref());
        imp.sidebar.set_selected_location(loc.as_ref());
        // Back also leads out of a search.
        let model = view.model();
        self.action_set_enabled("win.back", view.can_go_back() || model.searching());
        self.action_set_enabled("win.close-search", model.searching());
        self.action_set_enabled("win.stop", view.model().loading());
        self.action_set_enabled("win.forward", view.can_go_forward());
        self.set_title(Some(&view.location_title()));
        let search = model.search_text();
        if imp.search_entry.text().as_str() != search {
            imp.search_entry.set_text(&search);
            // Typing on from the files goes on at the end of the words, not before them.
            imp.search_entry.set_position(-1);
        }
        let pos = |list: &[&str], v: &str| list.iter().position(|k| *k == v).unwrap_or(0) as u32;
        imp.syncing_search.set(true);
        imp.search_kind_row
            .set_selected(pos(&crate::search::KINDS, &model.search_kind()));
        let dates: Vec<&str> = crate::search::DATES.iter().map(|(n, _)| *n).collect();
        imp.search_date_row
            .set_selected(pos(&dates, &model.search_date()));
        imp.search_match_row
            .set_selected(pos(&MATCHES, &model.search_match()));
        // A tab showing a search shows the search bar, as when Back brings one back.
        imp.search_button.set_active(!search.is_empty());
        imp.syncing_search.set(false);
    }
}
