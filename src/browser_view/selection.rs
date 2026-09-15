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
        // Not in a file chooser, where what is selected fills the name to save under.
        if self.chooser_mode() {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let model = view.model();
                let sel = model.selection();
                let n = sel.n_items();
                // A rename, or a file saved over, comes back under a fresh row, which
                // takes the selection instead.
                if n == 0 || !sel.selection().is_empty() || model.expects_back() {
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

    /// Select again what was selected when the directory list read it again, or it was
    /// renamed, and a fresh row came in its place, and give the first of them back the
    /// keyboard if the view had it, which went with the old row. After the change, as for
    /// the neighbour, and not in a file chooser either.
    /// Files landing above the top of the list, as a copy into the folder brings them,
    /// leave the list where it was: at the top. GTK's column view instead follows the row
    /// that was at the top down the list, and in a window opened after the first one it
    /// gets lost doing so, showing no rows at all until it is scrolled. Once per burst.
    pub(super) fn queue_keep_top(&self) {
        let imp = self.imp();
        if imp.column_view.model().is_none() || imp.top_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let imp = view.imp();
                imp.top_pending.set(false);
                if view.model().n_items() > 0 && imp.list_scroll.vadjustment().value() == 0.0 {
                    imp.column_view
                        .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
                }
            }
        ));
    }

    pub(super) fn queue_select_replaced(&self) {
        if self.chooser_mode() || !self.model().expects_back() {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let model = view.model();
                let positions = model.take_back();
                let Some(&first) = positions.first() else {
                    return;
                };
                let sel = model.selection();
                for &pos in &positions {
                    sel.select_item(pos, false);
                }
                if view.focus_child().is_some() {
                    view.reveal_position(first, gtk::ListScrollFlags::FOCUS);
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
        imp.floating_primary.set_text(&primary);
        imp.floating_details.set_text(&if files > 0 {
            format!("({})", crate::prefs::size(size))
        } else {
            String::new()
        });
        imp.floating_bar.set_visible(true);
    }

    /// Select `files` and show the first, as soon as the view has them to select (used by
    /// FileManager1.ShowItems, pasting, "Open Item Location", going back or up and leaving
    /// a search). A folder still listing is waited for: a big folder is put in order at
    /// the end, and a selection made before would not survive it. A search is not, since
    /// its results only ever go on the end. A file written a moment ago reaches the model
    /// through the folder monitor, which lags behind the operation that made it, so what
    /// is missing once nothing loads is given a moment more; a change of folder drops the
    /// lot.
    pub fn select_files_when_loaded(&self, files: Vec<gio::File>) {
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
                    sel.unselect_all();
                    for &pos in &found {
                        sel.select_item(pos, false);
                    }
                    if let Some(&pos) = found.first() {
                        view.reveal_position(pos, gtk::ListScrollFlags::FOCUS);
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

    /// Scroll to `pos` in whichever view is on screen; the others have no model to scroll.
    pub(crate) fn reveal_position(&self, pos: u32, flags: gtk::ListScrollFlags) {
        let imp = self.imp();
        if imp.grid_view.model().is_some() {
            imp.grid_view.scroll_to(pos, flags, None);
        }
        if imp.miller_list.model().is_some() {
            imp.miller_list.scroll_to(pos, flags, None);
        }
        if imp.column_view.model().is_some() {
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
