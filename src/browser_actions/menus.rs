//! The context menus: which entries they carry, and where they pop up.

use super::*;

/// Position of the first name cell below `w`, a few levels deep at most.
pub(super) fn first_cell_position(w: &gtk::Widget, depth: u32) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    if let Some(pos) = crate::browser_view::cell_position(w) {
        return Some(pos);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(pos) = first_cell_position(&c, depth + 1) {
            return Some(pos);
        }
        child = c.next_sibling();
    }
    None
}

impl BrowserView {
    /// The ways of opening the selection other than the plain one: a tab, a window, a
    /// terminal. Only the ones on offer are there, each an item of its own rather than a
    /// submenu, which a menu cannot be dismissed out of once it has been opened.
    pub(super) fn sync_open_menu(&self) {
        let imp = self.imp();
        let section = &imp.open_section;
        if imp.open_items.get() == 0 {
            imp.open_items.set(section.n_items() as u32);
        }
        // Ours sit after the two the template starts with; what the template puts there
        // is counted once, so the entries below them can be moved without touching this.
        let ours = 2;
        while section.n_items() as u32 > imp.open_items.get() {
            section.remove(ours);
        }
        let ways = [
            (
                gettext("Open in New _Tab"),
                "view.open-new-tab",
                Some("show-open-new-tab"),
            ),
            (
                gettext("Open in New _Window"),
                "view.open-new-window",
                Some("show-open-new-window"),
            ),
            (gettext("Open in _Terminal"), "view.open-terminal", None),
        ];
        let mut at = ours;
        for (label, action, setting) in ways {
            let name = action.trim_start_matches("view.");
            let enabled = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
                .is_some_and(|a| a.is_enabled());
            if enabled && setting.is_none_or(|key| imp.settings.boolean(key)) {
                section.insert(at, Some(&label), Some(action));
                at += 1;
            }
        }
    }

    /// The entries other programs add that take the selection, each running with it. A
    /// dialog offers none, as it offers no Open With.
    fn sync_drop_ins_menu(&self) {
        let section = &self.imp().drop_ins_section;
        section.remove_all();
        if self.chooser_mode() {
            return;
        }
        let types: Vec<String> = self
            .model()
            .selected_infos()
            .iter()
            .map(|info| file_utils::content_type_of(info).map(|t| t.to_string()))
            .collect::<Option<_>>()
            .unwrap_or_default();
        let entries =
            crate::drop_ins::load(&crate::drop_ins::dirs(crate::drop_ins::ITEM_MENU), None);
        for app in entries
            .iter()
            .filter(|app| crate::drop_ins::takes(app, &types))
        {
            let Some(path) = app.filename() else {
                continue;
            };
            let item = gio::MenuItem::new(Some(&app.name()), None);
            item.set_action_and_target_value(
                Some("view.drop-in"),
                Some(&path.to_string_lossy().to_variant()),
            );
            section.append_item(&item);
        }
    }

    /// Model position of the item cell under (x, y) in `stack` coordinates.
    pub(crate) fn item_at(&self, x: f64, y: f64) -> Option<u32> {
        let stack = &self.imp().stack;
        let mut w = stack.pick(x, y, gtk::PickFlags::DEFAULT)?;
        loop {
            if let Some(pos) = crate::browser_view::cell_position(&w) {
                return Some(pos);
            }
            // Any cell of a list row counts: the name cell is the one that knows the position.
            if w.css_name() == "row"
                && let Some(pos) = first_cell_position(&w, 0)
            {
                return Some(pos);
            }
            if &w == stack.upcast_ref::<gtk::Widget>() {
                return None;
            }
            w = w.parent()?;
        }
    }

    pub(super) fn popup_menu_at(&self, x: f64, y: f64) {
        self.popup_menu_on(self.item_at(x, y), x, y);
    }

    /// The menu of the item at `pos`, which is selected unless it already is, or the
    /// folder's with no item, at a point in `stack` coordinates.
    pub(crate) fn popup_menu_on(&self, pos: Option<u32>, x: f64, y: f64) {
        let imp = self.imp();
        let selection = self.model().selection();
        if let Some(pos) = pos
            && !selection.is_selected(pos)
        {
            selection.select_item(pos, true);
        }
        let on_item = pos.is_some();
        let desktop_menu;
        // The menus that are built rather than laid out follow what the actions say, so
        // they are made once the selection has been taken in.
        self.update_action_state();
        let model: &gio::MenuModel = if on_item {
            self.sync_open_menu();
            self.sync_drop_ins_menu();
            self.sync_tags_menu();
            &imp.item_menu
        } else {
            self.sync_new_menu();
            desktop_menu = imp.canvas.get().map(|c| c.background_menu());
            desktop_menu.as_ref().unwrap_or(&imp.background_menu)
        };
        let point = imp
            .stack
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32));
        let (px, py) = point
            .map(|p| (p.x() as f64, p.y() as f64))
            .unwrap_or((x, y));
        self.popup_model(model, px, py);
        if on_item {
            self.attach_tag_picker();
        }
    }

    /// Show `model` as a popover menu at a point in the view's own coordinates.
    pub(crate) fn popup_model(&self, model: &gio::MenuModel, x: f64, y: f64) {
        let imp = self.imp();
        let existing = imp.popover.borrow().clone();
        let popover = match existing {
            Some(p) => p,
            None => {
                let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
                // Submenus open beside their item instead of sliding into the menu: a
                // sliding one resizes the popover, which then has to move to stay on
                // screen, and the whole menu jumps out from under the pointer.
                p.set_flags(gtk::PopoverMenuFlags::NESTED);
                p.set_parent(self);
                p.set_has_arrow(false);
                p.set_halign(gtk::Align::Start);
                // Closing comes first and the entry activates straight after, so the files
                // have to outlive the close by an iteration; whatever is left then was a
                // menu dismissed without an answer.
                p.connect_closed(glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_| {
                        // The menu took the focus and hands it back to nothing in
                        // particular, which would leave the view keys dead.
                        view.grab_view_focus();
                        glib::idle_add_local_once(move || {
                            view.imp().pending_drop.replace(None);
                        });
                    }
                ));
                imp.popover.replace(Some(p.clone()));
                p
            }
        };
        popover.set_menu_model(Some(model));
        if imp.canvas.get().is_some() {
            crate::desktop::glass::dress_menu(popover.upcast_ref());
        }
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
        if imp.canvas.get().is_some() {
            // GTK lets a menu flip above the point or shrink, never slide, so one that fits
            // on neither side would scroll its last entries out of sight. The desktop has
            // the whole display: such a menu goes as far down as it still fits. Its height
            // is known once it is styled as shown, on its first frame.
            let room = f64::from(self.height());
            popover.add_tick_callback(move |popover, _| {
                if let Some(card) = popover.child().and_then(|card| card.parent()) {
                    let height = f64::from(card.measure(gtk::Orientation::Vertical, -1).1);
                    if y + height > room && y < height {
                        let y = (room - height - 1.0).max(0.0);
                        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                            x as i32, y as i32, 1, 1,
                        )));
                    }
                }
                glib::ControlFlow::Break
            });
        }
    }

    /// Menu or Shift+F10: the menu a right click on the file the keyboard is on would
    /// open, over that file. With the keyboard on no file, the menu of the selection, or
    /// of the folder when nothing is selected, in the middle of the pane: whatever file
    /// sits there is not the one the keys were on.
    pub(super) fn popup_menu_for_selection(&self) {
        let stack = self.imp().stack.upcast_ref::<gtk::Widget>();
        let focused = self
            .root()
            .and_then(|root| root.focus())
            .and_then(|w| crate::browser_view::row_widget(&w))
            .filter(|row| row.is_ancestor(stack))
            .and_then(|row| Some((first_cell_position(&row, 0)?, row.compute_bounds(stack)?)));
        let (w, h) = (stack.width() as f64, stack.height() as f64);
        match focused {
            // A file scrolled out of sight keeps the keyboard; its menu stays on screen.
            Some((pos, b)) => self.popup_menu_on(
                Some(pos),
                f64::from(b.x() + b.width() / 2.0).clamp(0.0, w),
                f64::from(b.y() + b.height() / 2.0).clamp(0.0, h),
            ),
            None => {
                let selected = self.model().selection().selection();
                let pos = (!selected.is_empty()).then(|| selected.minimum());
                self.popup_menu_on(pos, w / 2.0, h / 2.0);
            }
        }
    }
}
