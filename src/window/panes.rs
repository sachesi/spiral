//! The split view with a second pane, and the details panel beside the panes.

use super::*;

impl SpiralWindow {
    /// Point the header, the sort state and the `view` actions at the pane in charge.
    pub(super) fn refresh_active(&self) {
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
        imp.details.set_view(&view);
        // What the other pane shows decides whether files can go there.
        view.update_action_state();
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
    pub(super) fn mark_panes(&self) {
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
    pub(super) fn apply_split(&self) {
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

    /// Show the details panel as the setting says, while the window has room for it.
    pub(super) fn apply_details(&self) {
        let imp = self.imp();
        let show =
            imp.settings.boolean("details-visible") && !imp.narrow.get() && !imp.cramped.get();
        // Hidden with the keyboard in it, the panel hands the keyboard back to the pane: it
        // would otherwise be on nothing, and the keys of the files with it.
        if !show
            && gtk::prelude::GtkWindowExt::focus(self)
                .is_some_and(|focus| focus.is_ancestor(&*imp.details_sidebar))
            && let Some(view) = self.current_view()
        {
            view.grab_view_focus();
        }
        // Hidden, it is taken out of the layout as well: the split view still measures a
        // hidden sidebar, at whatever width is left while the window changes breakpoint.
        imp.details_sidebar.set_visible(show);
        imp.details_view.set_show_sidebar(show);
    }

    /// F6: hand the focus to the other pane.
    pub(super) fn switch_pane(&self) {
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
}
