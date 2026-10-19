//! What is selected: keeping it through changes, stepping it with the keys, and the bar that
//! says what it is.

use super::*;

pub(super) fn folders_selected(n: usize) -> String {
    ngettext("%d folder selected", "%d folders selected", n as u32).replace("%d", &n.to_string())
}

pub(super) fn items_selected(n: usize) -> String {
    ngettext("%d item selected", "%d items selected", n as u32).replace("%d", &n.to_string())
}

impl BrowserView {
    /// Selection mode on or off. While it is on a tap picks a file out or leaves it, and
    /// the bar under the view says how many are picked and acts on them.
    pub(crate) fn set_selection_mode(&self, on: bool) {
        let imp = self.imp();
        imp.selection_mode.set(on);
        // A chooser picks files and does nothing else with them.
        let acts = !self.chooser_mode();
        for button in [
            &imp.selection_more,
            &imp.selection_trash,
            &imp.selection_copy,
            &imp.selection_cut,
        ] {
            button.set_visible(acts);
        }
        self.update_floating_bar();
        imp.selection_bar.set_reveal_child(on);
        self.update_action_state();
    }

    /// A tap of a finger opens what it lands on, whatever the mouse takes to open it, and
    /// in selection mode picks it out or leaves it instead. It is taken as the finger
    /// lifts: a finger that scrolls or is held is no tap.
    pub(super) fn setup_taps(&self) {
        let tap = gtk::GestureClick::builder()
            .button(gdk::BUTTON_PRIMARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let taps_here = glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            None,
            move |gesture: &gtk::GestureClick, x: f64, y: f64| {
                if !crate::touch::is_touchscreen(gesture.device().as_ref())
                    || view.desktop_mode()
                    || view.in_side_column(x, y)
                {
                    return None;
                }
                view.item_at(x, y)
            }
        );
        // The second tap of a double tap would open again what the first opened.
        tap.connect_pressed(glib::clone!(
            #[strong]
            taps_here,
            move |gesture, n, x, y| {
                if n > 1 && taps_here(gesture, x, y).is_some() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        ));
        tap.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, n, x, y| {
                let Some(pos) = taps_here(gesture, x, y).filter(|_| n == 1) else {
                    return;
                };
                let selection = view.model().selection();
                if view.imp().selection_mode.get() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    if selection.is_selected(pos) {
                        selection.unselect_item(pos);
                    } else {
                        selection.select_item(pos, false);
                    }
                } else if view.view_mode() != ViewMode::Columns {
                    // The column view opens on one click anyway.
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    selection.select_item(pos, true);
                    view.activate_position(pos);
                }
            }
        ));
        self.imp().stack.add_controller(tap);
    }

    /// While searching, keep the first result selected until another one is picked, so
    /// Enter in the search box opens it. The first result changes as results arrive in
    /// order, and the selection follows it. Not while the results are changing: the views
    /// are told of the change after the model, and a selection moved before that points
    /// them at rows they do not have yet.
    pub(super) fn queue_pick_first_result(&self) {
        if self.imp().pick_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().pick_pending.set(false);
                view.pick_first_result();
            }
        ));
    }

    /// What was selected went away -- deleted, moved elsewhere -- leaving nothing selected:
    /// select what took its place, or the last item where it was at the end, and give it
    /// the keyboard if the view had it, so the arrows go on from there and not from the
    /// top. After the change, for the same reason as picking the first result; not where
    /// the whole folder went, as it does on leaving it.
    pub(super) fn queue_select_neighbor(&self, position: u32) {
        // Not in a file chooser, where what is selected fills the name to save under, nor
        // on the desktop, whose icons stand in no order that has a neighbour.
        if self.chooser_mode() || self.desktop_mode() {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let model = view.model();
                let sel = model.selection();
                let n = sel.n_items();
                if n == 0 || !sel.selection().is_empty() {
                    return;
                }
                let pos = position.min(n - 1);
                sel.select_item(pos, true);
                let flags = if view.focus_child().is_some() {
                    gtk::ListScrollFlags::FOCUS
                } else {
                    gtk::ListScrollFlags::NONE
                };
                view.reveal_position(pos, flags);
            }
        ));
    }

    /// Files landing above the top of the list or the grid, as a copy into the folder or
    /// the next batch of a share's listing brings them, leave the view where it was: at
    /// the top. GTK instead follows the item that was at the top down the view, and the
    /// column view, in a window opened after the first one, gets lost doing so, showing no
    /// rows at all until it is scrolled. Once per burst. Whether the view was at the top
    /// is read now, before GTK has moved it.
    pub(super) fn queue_keep_top(&self) {
        let imp = self.imp();
        if (imp.column_view.model().is_none() && imp.grid_view.model().is_none())
            || imp.top_pending.replace(true)
        {
            return;
        }
        let list_at_top = imp.list_scroll.vadjustment().value() == 0.0;
        let grid_at_top = imp
            .grid_view
            .vadjustment()
            .is_some_and(|adj| adj.value() == 0.0);
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let imp = view.imp();
                imp.top_pending.set(false);
                // The column view's own model, which can still be empty while the folder's
                // is not: GTK asserts on a row it does not have.
                let rows = imp.column_view.model().map_or(0, |m| m.n_items());
                if rows > 0 && list_at_top {
                    imp.column_view
                        .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
                }
                let cells = imp.grid_view.model().map_or(0, |m| m.n_items());
                if cells > 0 && grid_at_top {
                    imp.grid_view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
                }
            }
        ));
    }

    pub(super) fn pick_first_result(&self) {
        let imp = self.imp();
        let model = &imp.model;
        // Not in a file chooser, where what is selected fills the name to save under.
        if self.chooser_mode() || !model.searching() || model.n_items() == 0 {
            return;
        }
        let sel = model.selection();
        let set = sel.selection();
        let only_first = set.size() == 1 && set.contains(0);
        if !set.is_empty() && (!imp.first_picked.get() || only_first) {
            return;
        }
        imp.picking.set(true);
        sel.select_item(0, true);
        imp.picking.set(false);
        imp.first_picked.set(true);
    }

    /// A rubber band changes the selection with every motion event, and reading it out
    /// walks the folder; a folder being listed changes it once per batch of files. Once
    /// per turn of the main loop is as often as a status bar and a menu need telling.
    pub(crate) fn queue_selection_update(&self) {
        if self.imp().selection_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().selection_pending.set(false);
                view.update_floating_bar();
                view.update_action_state();
            }
        ));
    }

    /// Bottom-right status: selection summary while something is selected, spinner while loading.
    pub(super) fn update_floating_bar(&self) {
        let imp = self.imp();
        // The desktop says nothing over the wallpaper.
        if self.desktop_mode() {
            imp.floating_bar.set_visible(false);
            return;
        }
        let infos = imp.model.selected_infos();
        let loading = imp.model.loading();
        imp.floating_spinner.set_visible(loading);
        if infos.is_empty() {
            imp.floating_primary.set_text(&if loading {
                gettext("Loading…")
            } else {
                String::new()
            });
            imp.floating_details.set_text("");
            imp.floating_bar.set_visible(loading);
            return;
        }
        let folders = infos.iter().filter(|i| file_utils::is_dir(i)).count();
        let files = infos.len() - folders;
        let size: u64 = infos
            .iter()
            .filter(|i| !file_utils::is_dir(i))
            .map(file_utils::size_of)
            .sum();
        let primary = match (folders, files) {
            (1, 0) | (0, 1) => gettext("“%s” selected").replace("%s", &infos[0].display_name()),
            (f, 0) => folders_selected(f),
            (0, n) => items_selected(n),
            (f, n) => {
                let others = ngettext(
                    "%d other item selected",
                    "%d other items selected",
                    n as u32,
                )
                .replace("%d", &n.to_string());
                // Translators: %f is "%d folders selected", %n is "%d other items selected".
                gettext("%f, %n")
                    .replace("%f", &folders_selected(f))
                    .replace("%n", &others)
            }
        };
        let details = if files > 0 {
            format!("({})", crate::prefs::size(size))
        } else {
            String::new()
        };
        // In selection mode its own bar says it.
        if imp.selection_mode.get() {
            imp.selection_count
                .set_label(format!("{primary} {details}").trim_end());
            imp.floating_bar.set_visible(false);
            return;
        }
        imp.floating_primary.set_text(&primary);
        imp.floating_details.set_text(&details);
        imp.floating_bar.set_visible(true);
    }

    /// Select `files` and show the first, as soon as the view has them to select (used by
    /// FileManager1.ShowItems, pasting, "Open Item Location", going back or up and leaving
    /// a search). A folder still listing is waited for: a big folder is put in order at
    /// the end, and a selection made before would not survive it. A search is not, since
    /// its results only ever go on the end. A file written a moment ago reaches the model
    /// through the folder monitor, which lags behind the operation that made it, so what
    /// is missing once nothing loads is given a moment more; a change of folder drops the
    /// lot, and so does a selection made meanwhile, which is what the user wants now.
    pub fn select_files_when_loaded(&self, files: Vec<gio::File>) {
        self.select_files_since(files, self.selection_snapshot());
    }

    /// What is selected now, by location. Whatever starts an operation keeps it for
    /// `select_files_since`, so what the user selects while the operation runs is told
    /// from what was selected when it started.
    pub fn selection_snapshot(&self) -> std::collections::HashSet<String> {
        self.model()
            .selected_files()
            .iter()
            .map(|f| f.uri().to_string())
            .collect()
    }

    /// `select_files_when_loaded`, unless the selection is no longer `before` by then.
    pub fn select_files_since(
        &self,
        files: Vec<gio::File>,
        before: std::collections::HashSet<String>,
    ) {
        use futures_util::StreamExt;
        if files.is_empty() {
            return;
        }
        let model = self.model();
        let sel = model.selection();
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<()>();
        let poke = tx.clone();
        let loading_id = model.connect_loading_notify(move |_| {
            let _ = poke.unbounded_send(());
        });
        let items_id = sel.connect_items_changed(move |_, _, _, _| {
            let _ = tx.unbounded_send(());
        });
        let location = self.location();
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            const GRACE: std::time::Duration = std::time::Duration::from_secs(2);
            let mut missing_since: Option<std::time::Instant> = None;
            loop {
                let Some(view) = view.upgrade() else { break };
                let moved = match (view.location(), &location) {
                    (Some(now), Some(then)) => !now.equal(then),
                    (now, then) => now.is_some() != then.is_some(),
                };
                if moved {
                    break;
                }
                let found = model.positions_of(&files);
                let waiting = model.loading() && !model.searching();
                let all = found.len() >= files.len();
                let given_up = !waiting
                    && !all
                    && missing_since
                        .get_or_insert_with(std::time::Instant::now)
                        .elapsed()
                        >= GRACE;
                if !waiting && (all || given_up) {
                    // None of them at all -- hidden, or never made -- is nothing to
                    // trade the selection for.
                    if found.is_empty() {
                        break;
                    }
                    let now = view.selection_snapshot();
                    if !now.is_empty() && now != before {
                        break;
                    }
                    sel.unselect_all();
                    for &pos in &found {
                        sel.select_item(pos, false);
                    }
                    if let Some(&pos) = found.first() {
                        view.reveal_position(pos, view.focus_flags());
                    }
                    break;
                }
                drop(view);
                let wait = match missing_since {
                    Some(since) if !waiting => GRACE.saturating_sub(since.elapsed()),
                    _ => std::time::Duration::from_secs(3600),
                };
                // Nothing left to tell of a change: the model has gone.
                if let Ok(None) = glib::future_with_timeout(wait, rx.next()).await {
                    break;
                }
            }
            model.disconnect(loading_id);
            sel.disconnect(items_id);
        });
    }

    /// How to scroll to what is selected for the user: taking the keyboard along, unless
    /// they are typing somewhere else, in the path bar or the search box. The rename
    /// popover is the view's own, and still has the keyboard as the rename it asked for
    /// ends.
    pub(super) fn focus_flags(&self) -> gtk::ListScrollFlags {
        let typing = self
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|widget| widget.is::<gtk::Text>() && !widget.is_ancestor(self));
        if typing {
            gtk::ListScrollFlags::NONE
        } else {
            gtk::ListScrollFlags::FOCUS
        }
    }

    /// Scroll to `pos` in whichever view is on screen; the others have no model to scroll.
    /// A view whose model does not hold `pos` yet is left alone: GTK asserts on it.
    pub(crate) fn reveal_position(&self, pos: u32, flags: gtk::ListScrollFlags) {
        let imp = self.imp();
        let holds = |model: Option<gtk::SelectionModel>| model.is_some_and(|m| pos < m.n_items());
        if holds(imp.grid_view.model()) {
            imp.grid_view.scroll_to(pos, flags, None);
        }
        if holds(imp.miller_list.model()) {
            imp.miller_list.scroll_to(pos, flags, None);
        }
        if holds(imp.column_view.model()) {
            imp.column_view.scroll_to(pos, None, flags, None);
        }
    }

    /// Select the item `delta` places along, for the preview's arrows. The focus stays
    /// where it is, because it is in the preview.
    pub(crate) fn step_selection(&self, delta: i32) {
        let model = self.model();
        let selected = model.selection().selection();
        if selected.size() == 0 || model.n_items() == 0 {
            return;
        }
        let last = model.n_items() as i32 - 1;
        let pos = (selected.nth(0) as i32 + delta).clamp(0, last) as u32;
        model.selection().select_item(pos, true);
        self.reveal_position(pos, gtk::ListScrollFlags::NONE);
    }
}
