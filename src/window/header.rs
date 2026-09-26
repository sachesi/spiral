//! The header bar: the location entry and the view and sort controls.

use super::*;

impl SpiralWindow {
    /// End the search the pane in charge is showing, through the search bar so it goes
    /// with it. Whether there was one.
    pub(super) fn end_search(&self) -> bool {
        let searching = self.current_view().is_some_and(|v| v.model().searching());
        if searching {
            self.imp().search_button.set_active(false);
        }
        searching
    }

    pub(super) fn show_location_entry(&self) {
        let imp = self.imp();
        let text = self
            .current_view()
            .map(|v| crate::location_entry::location_text(&v))
            .unwrap_or_default();
        imp.search_button.set_active(false);
        crate::location_entry::set_text_quiet(&imp.location_entry, &text);
        imp.toolbar_switcher.set_visible_child_name("location");
        imp.location_entry.grab_focus();
        imp.location_entry.set_position(-1);
    }

    pub(super) fn sync_sort_state(&self) {
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
    pub(super) fn sync_columns_item(&self) {
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

    /// The split button shows the view you switch *to*.
    pub(super) fn sync_view_button(&self) {
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
