//! The window's `win.*` actions, and the keys bound to them and to view actions at window
//! level.

use super::*;

/// Install the actions and their keys on the window's class.
pub(super) fn install(klass: &mut <imp::SpiralWindow as ObjectSubclass>::Class) {
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
    // While searching, Back and Backspace end the search and stay in the folder.
    klass.install_action("win.back", None, |win, _, _| {
        if !win.end_search() {
            win.navigate(BrowserView::go_back);
        }
    });
    klass.install_action("win.forward", None, |win, _, _| {
        win.navigate(BrowserView::go_forward)
    });
    klass.install_action("win.up", None, |win, _, _| win.navigate(BrowserView::go_up));
    klass.install_action("win.back-or-up", None, |win, _, _| {
        if !win.end_search() {
            win.navigate(BrowserView::go_back_or_up);
        }
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
    klass.install_action("win.location-menu", None, |win, _, _| {
        let imp = win.imp();
        if imp.toolbar_switcher.visible_child_name().as_deref() == Some("pathbar") {
            imp.path_bar.popup_menu();
        }
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
            // The bookmarks may have changed since the view last looked, in another
            // window or another program, and a disabled action ignores the key.
            v.update_action_state();
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
    // F10 opens the menu of the current folder, not the main menu, see `constructed`.
    klass.add_binding_action(Key::F10, M::empty(), "win.location-menu");
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
    klass.add_binding_action(Key::c, M::CONTROL_MASK, "view.copy-network-address");
    klass.add_binding_action(Key::x, M::CONTROL_MASK, "view.cut");
    klass.add_binding_action(Key::v, M::CONTROL_MASK, "view.paste");
    // And from the search box, to the folder of the result that is selected.
    klass.add_binding_action(
        Key::o,
        M::CONTROL_MASK | M::ALT_MASK,
        "view.open-item-location",
    );
}
