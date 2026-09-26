//! Tabs: opening one, the views it holds and how they are wired to the window, and which
//! view is the active one.

use super::*;

impl SpiralWindow {
    /// The window a view currently sits in; tabs move between windows, so the
    /// handlers in `add_tab` look it up instead of capturing it.
    pub(super) fn of(widget: &impl IsA<gtk::Widget>) -> Option<Self> {
        widget.root().and_downcast()
    }

    /// The tab the context menu was opened for, else the selected one.
    pub(super) fn menu_page(&self) -> Option<adw::TabPage> {
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
    pub(super) fn navigate(&self, f: impl Fn(&BrowserView)) {
        if let Some(v) = self.current_view() {
            f(&v);
            v.grab_view_focus();
        }
    }

    pub(super) fn set_active_view(&self, view: &BrowserView) {
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

    pub(super) fn remember_folded(&self, page: &adw::TabPage, location: gio::File) {
        let mut folded = self.imp().folded.borrow_mut();
        folded.retain(|(p, _)| p.upgrade().is_some_and(|p| &p != page));
        folded.push((page.downgrade(), location));
    }

    pub(super) fn take_folded(&self, page: &adw::TabPage) -> Option<gio::File> {
        let mut folded = self.imp().folded.borrow_mut();
        let at = folded
            .iter()
            .position(|(p, _)| p.upgrade().as_ref() == Some(page))?;
        Some(folded.remove(at).1)
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
    pub(super) fn attach_view(&self, page: &adw::TabPage, view: &BrowserView) {
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
        // The details panel follows the selection of the pane in charge, and its folder.
        let details = |v: &BrowserView| {
            if let Some(win) = Self::of(v)
                && win.current_view().as_ref() == Some(v)
            {
                win.imp().details.queue_update();
            }
        };
        let selection = view.model().selection();
        selection.connect_selection_changed(glib::clone!(
            #[weak]
            view,
            move |_, _, _| details(&view)
        ));
        selection.connect_items_changed(glib::clone!(
            #[weak]
            view,
            move |_, _, _, _| details(&view)
        ));
        // Its count waits for the listing, and is not given for a folder that failed.
        let model = view.model();
        model.connect_loading_notify(glib::clone!(
            #[weak]
            view,
            move |_| details(&view)
        ));
        model.connect_error_message_notify(glib::clone!(
            #[weak]
            view,
            move |_| details(&view)
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

    pub(super) fn go_to_tab(&self, index: i32) {
        let tabs = &self.imp().tab_view;
        if index >= 0 && index < tabs.n_pages() {
            tabs.set_selected_page(&tabs.nth_page(index));
        }
    }
}
