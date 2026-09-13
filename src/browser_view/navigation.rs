//! Going places: the location, the history, searching, reloading, and how a folder was left.

use super::*;

/// The folder in `dir` that holds `file`, or `file` itself when it is in `dir`; `None`
/// when `file` is not below `dir`.
pub(super) fn child_toward(dir: &gio::File, file: &gio::File) -> Option<gio::File> {
    let mut child = file.clone();
    loop {
        let parent = child.parent()?;
        if parent.equal(dir) {
            return Some(child);
        }
        child = parent;
    }
}

/// A step in a tab's history: the folder, and the search it was showing and what was
/// selected when it was left, which going back to it brings back.
pub struct Visit {
    file: gio::File,
    search: Option<Search>,
    selected: Vec<gio::File>,
}

/// A search as it stood: the words, the filters, and what it had found. Only the last
/// search left in a tab keeps what it found, since that can be a great many files; one
/// further back searches again.
#[derive(Clone)]
pub(super) struct Search {
    text: String,
    kind: String,
    date: String,
    matching: String,
    hits: Option<Vec<gio::FileInfo>>,
    /// Whether the search had come to its end, or goes on when shown again.
    finished: bool,
}

impl BrowserView {
    /// Navigate to `file`, pushing onto history.
    pub fn go_to(&self, file: &gio::File) {
        let imp = self.imp();
        // Already there, unless it is showing a search: then the folder itself is a step
        // of its own, and Back returns to the search.
        if imp.model.location().is_some_and(|l| l.equal(file)) && imp.model.search_text().is_empty()
        {
            return;
        }
        self.remember_state();
        {
            let mut hist = imp.history.borrow_mut();
            let pos = imp.history_pos.get();
            if !hist.is_empty() {
                hist.truncate(pos + 1);
            }
            hist.push(Visit {
                file: file.clone(),
                search: None,
                selected: Vec::new(),
            });
            imp.history_pos.set(hist.len() - 1);
        }
        self.set_location_internal(file, None);
    }

    /// Keep the search on screen and the selection with the step of the history being
    /// left.
    pub(super) fn remember_state(&self) {
        let imp = self.imp();
        let model = &imp.model;
        let text = model.search_text();
        let search = (!text.is_empty()).then(|| Search {
            text,
            kind: model.search_kind(),
            date: model.search_date(),
            matching: model.search_match(),
            hits: Some(model.search_hits()),
            finished: !model.loading(),
        });
        let selected = model.selected_files();
        let pos = imp.history_pos.get();
        let mut hist = imp.history.borrow_mut();
        if search.is_some() {
            for visit in hist.iter_mut() {
                if let Some(search) = visit.search.as_mut() {
                    search.hits = None;
                }
            }
        }
        if let Some(visit) = hist.get_mut(pos) {
            visit.search = search;
            visit.selected = selected;
        }
    }

    /// Show `file`, and `search` in it when the step of the history had one.
    pub(super) fn set_location_internal(&self, file: &gio::File, search: Option<Search>) {
        let imp = self.imp();
        imp.mounting.replace(Mounting::Idle);
        imp.model.set_search_text("");
        imp.model.set_location(Some(file));
        if let Some(search) = search {
            imp.model.set_search_kind(search.kind);
            imp.model.set_search_date(search.date);
            imp.model.set_search_match(search.matching);
            match search.hits {
                Some(hits) => imp
                    .model
                    .show_search_hits(&search.text, &hits, search.finished),
                None => imp.model.set_search_text(search.text),
            }
        }
        imp.location.replace(Some(file.clone()));
        self.resolve_view_mode(file);
        // Model is empty right after set_location; scroll once the first items land.
        let sel = imp.model.selection();
        let id = std::rc::Rc::new(std::cell::RefCell::new(None));
        let id2 = id.clone();
        *id.borrow_mut() = Some(sel.connect_items_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |sel, _, _, added| {
                if added == 0 {
                    return;
                }
                view.reveal_position(0, gtk::ListScrollFlags::NONE);
                if let Some(id) = id2.borrow_mut().take() {
                    sel.disconnect(id);
                }
            }
        )));
        let hist = imp.history.borrow();
        let pos = imp.history_pos.get();
        imp.can_go_back.set(pos > 0);
        imp.can_go_forward.set(pos + 1 < hist.len());
        drop(hist);
        self.notify_can_go_back();
        self.notify_can_go_forward();
        self.notify_location();
        self.take_keyboard();
    }

    /// A folder change destroys every row, and with them whatever had the keyboard: the
    /// window is left with the focus nowhere in particular, and the file keys do nothing
    /// until something in the view is clicked. Take it back, unless it is in a text entry
    /// -- the search box, the location bar, a file name -- where typing is the point.
    pub(super) fn take_keyboard(&self) {
        let focus = self.root().and_then(|root| root.focus());
        if focus.is_some_and(|w| w.is::<gtk::Editable>()) {
            return;
        }
        self.grab_view_focus();
    }

    pub fn go_back(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        if pos == 0 {
            return;
        }
        self.remember_state();
        imp.history_pos.set(pos - 1);
        self.show_visit(pos - 1);
    }

    pub fn go_forward(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        if pos + 1 >= imp.history.borrow().len() {
            return;
        }
        self.remember_state();
        imp.history_pos.set(pos + 1);
        self.show_visit(pos + 1);
    }

    /// Show the step at `pos` with what was selected there. A step that had nothing
    /// selected and leads up from the folder being left selects the folder on the way.
    pub(super) fn show_visit(&self, pos: usize) {
        let (file, search, mut selected) = {
            let hist = self.imp().history.borrow();
            let visit = &hist[pos];
            (
                visit.file.clone(),
                visit.search.clone(),
                visit.selected.clone(),
            )
        };
        if selected.is_empty()
            && search.is_none()
            && let Some(child) = self.location().and_then(|l| child_toward(&file, &l))
        {
            selected.push(child);
        }
        self.set_location_internal(&file, search);
        self.select_files_when_loaded(selected);
    }

    /// Backspace: back to the search this folder was opened from, up otherwise.
    pub fn go_back_or_up(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        let from_search = pos > 0 && imp.history.borrow()[pos - 1].search.is_some();
        if from_search {
            self.go_back();
        } else {
            self.go_up();
        }
    }

    /// Up to the parent, with the folder just left selected in it.
    pub fn go_up(&self) {
        let Some(here) = self.location() else { return };
        if let Some(parent) = here.parent() {
            self.go_to(&parent);
            self.select_files_when_loaded(vec![here]);
        }
    }

    /// Search the folder for `text`. With no text the folder comes back, with what was
    /// selected among the results selected in it: the result itself, or the folder in
    /// it that holds the result.
    pub fn search_for(&self, text: &str) {
        let model = self.model();
        if !text.is_empty() || !model.searching() {
            model.set_search_text(text);
            return;
        }
        let mut keep: Vec<gio::File> = Vec::new();
        if let Some(dir) = self.location() {
            for file in model.selected_files() {
                let file = child_toward(&dir, &file).unwrap_or(file);
                if !keep.iter().any(|f| f.equal(&file)) {
                    keep.push(file);
                }
            }
        }
        model.set_search_text("");
        self.select_files_when_loaded(keep);
    }

    /// Read the folder again, leaving the view where it was. The listing is thrown away
    /// and read from the start, which empties the model: the selection goes with it and
    /// every view below it comes back at the first row. Both are put back once the
    /// folder is in again, so a reload shows what was on screen before it.
    pub fn reload(&self) {
        let imp = self.imp();
        // Asked for again by hand: a share whose password was turned down is worth another
        // try, and so is one that has since come back.
        imp.mounting.replace(Mounting::Idle);
        let offset = self.view_adjustment().map_or(0.0, |adj| adj.value());
        let selected = imp.model.selected_files();
        let focused = self.view_has_focus();
        imp.model.reload();
        if offset == 0.0 && selected.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let model = view.model();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while model.loading() && std::time::Instant::now() < deadline {
                    glib::timeout_future(std::time::Duration::from_millis(50)).await;
                }
                let sel = model.selection();
                let found = model.positions_of(&selected);
                for &pos in &found {
                    sel.select_item(pos, false);
                }
                // The rows are new widgets, so the keyboard was left outside the view
                // while the folder was away; it belongs where it was.
                if focused && let Some(&pos) = found.first() {
                    view.reveal_position(pos, gtk::ListScrollFlags::FOCUS);
                    view.grab_view_focus();
                }
                view.restore_offset(offset).await;
            }
        ));
    }

    /// Scroll back to `offset`. A view only works out how tall it is once the rows that
    /// arrived have been laid out, so until then the value is clamped to the little it
    /// believes it has and has to be given again.
    pub(super) async fn restore_offset(&self, offset: f64) {
        for _ in 0..20 {
            let Some(adj) = self.view_adjustment() else {
                return;
            };
            adj.set_value(offset);
            if adj.value() >= offset {
                return;
            }
            glib::timeout_future(std::time::Duration::from_millis(25)).await;
        }
    }

    /// The vertical adjustment of whichever view is on screen; the others have no model
    /// and nothing to scroll.
    pub(super) fn view_adjustment(&self) -> Option<gtk::Adjustment> {
        let imp = self.imp();
        if imp.grid_view.model().is_some() {
            imp.grid_view.vadjustment()
        } else if imp.miller_list.model().is_some() {
            imp.miller_list.vadjustment()
        } else {
            imp.column_view.vadjustment()
        }
    }

    /// Whether the keyboard is on something inside this view.
    pub(super) fn view_has_focus(&self) -> bool {
        self.root()
            .and_then(|root| root.focus())
            .is_some_and(|widget| widget.is_ancestor(self))
    }
}
