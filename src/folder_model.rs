//! `Listing` -> filter -> sort -> selection pipeline shared by every view of a folder.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::enums::SortKey;

/// How many files a listing may hold and still go on the pipeline before it is complete.
const SHOW_WHILE_LISTING_UP_TO: u32 = 2000;

/// How long a listing may keep the window blank before what has arrived is shown.
const SHOW_LISTING_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

use crate::file_utils;
use crate::{gio, glib, gtk};

mod listing;
mod lists;
mod search;

use listing::Listing;
pub(crate) use listing::touch;

pub(crate) use lists::is_list_location;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::FolderModel)]
    pub struct FolderModel {
        #[property(get, set = Self::set_location, nullable)]
        location: RefCell<Option<gio::File>>,
        #[property(get, set = Self::set_show_hidden)]
        pub show_hidden: Cell<bool>,
        #[property(get, set = Self::set_sort_key, builder(SortKey::Name))]
        sort_key: Cell<SortKey>,
        #[property(get, set = Self::set_sort_reversed)]
        sort_reversed: Cell<bool>,
        #[property(get, set = Self::set_search_text)]
        pub search_text: RefCell<String>,
        /// One of `search::KINDS`.
        #[property(get, set = Self::set_search_kind)]
        pub search_kind: RefCell<String>,
        /// One of the `search::DATES` nicks.
        #[property(get, set = Self::set_search_date)]
        pub search_date: RefCell<String>,
        /// "name", "both" or "content".
        #[property(get, set = Self::set_search_match)]
        pub search_match: RefCell<String>,
        /// True while the list shows search results instead of the folder.
        #[property(get)]
        pub searching: Cell<bool>,
        #[property(get, set = Self::set_extra_filter, nullable)]
        extra_filter: RefCell<Option<gtk::Filter>>,
        #[property(get)]
        loading: Cell<bool>,
        #[property(get, nullable)]
        error_message: RefCell<Option<String>>,
        #[property(get)]
        selection: gtk::MultiSelection,

        pub listing: Listing,
        /// Sorted flat list, root of the tree the selection sits on.
        sorted: gtk::SortListModel,
        /// Whether folders unfold in place; read by the tree's child-model function.
        tree: Rc<Cell<bool>>,
        /// What is selected, by info and by location. A row put in anew -- a file read
        /// again, a folder's children listed again as it unfolds, rows the sort moves --
        /// comes unselected, and is selected again from these; see `keep_selection`.
        kept: Rc<RefCell<Kept>>,
        /// Whether the listing is on the pipeline; a big one waits there for its end,
        /// see `show_listing`.
        listing_shown: Cell<bool>,
        /// Set while a listing shown before its end is waiting for its order.
        sort_deferred: Cell<bool>,
        /// Root of the pipeline: `listing`, or `list_store` for the locations that are a
        /// list of files rather than a folder, `starred:///` and `tag:///`.
        filtered: gtk::FilterListModel,
        pub list_store: gio::ListStore,
        /// The location `list_store` holds the files of.
        pub(super) list_of: RefCell<Option<gio::File>>,
        pub list_gen: Cell<u64>,
        starred_handler: RefCell<Option<crate::lines::WatchId>>,
        tags_handler: RefCell<Option<crate::lines::WatchId>>,
        /// The handlers sit on objects that outlive the model -- the starred list, the tag
        /// index and the settings -- and a column view makes and drops models as it walks,
        /// so they have to come off again.
        tree_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Whether a search walks the folders below this one. A file chooser searches the
        /// folder it is showing and nothing else: it is picking a file, not looking for one.
        pub search_recursive: Cell<bool>,
        /// Root of the pipeline while searching; filled by `search::run`.
        pub search_store: gio::ListStore,
        pub search_gen: Cell<u64>,
        hidden_filter: gtk::CustomFilter,
        every_filter: gtk::EveryFilter,
        sorter: gtk::CustomSorter,
        // Shared with the filter/sorter closures.
        hidden_state: Rc<Cell<bool>>,
        sort_state: Rc<Cell<(SortKey, bool)>>,
    }

    impl Default for FolderModel {
        fn default() -> Self {
            let hidden_state = Rc::new(Cell::new(false));
            let sort_state = Rc::new(Cell::new((SortKey::Name, false)));

            let hidden_filter = gtk::CustomFilter::new(glib::clone!(
                #[strong]
                hidden_state,
                move |obj| {
                    let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
                    hidden_state.get() || !file_utils::is_hidden(info)
                }
            ));
            // An entry that stands for a location no installed backend can open is a dead
            // end: a server on the network needs the backend for its protocol, and without
            // it the entry answers nothing but an error. Neither is the folder an archive
            // is unpacked into, or packed in, before it goes where it belongs.
            let reachable_filter = gtk::CustomFilter::new(|obj| {
                let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
                !file_utils::is_unreachable(info) && !crate::ops::archive::is_work_folder(info)
            });
            let every_filter = gtk::EveryFilter::new();
            every_filter.append(hidden_filter.clone());
            every_filter.append(reachable_filter);

            let sorter = gtk::CustomSorter::new(glib::clone!(
                #[strong]
                sort_state,
                move |a, b| {
                    let (key, rev) = sort_state.get();
                    file_utils::compare(
                        a.downcast_ref().unwrap(),
                        b.downcast_ref().unwrap(),
                        key,
                        rev,
                    )
                    .into()
                }
            ));

            let filtered =
                gtk::FilterListModel::new(None::<gio::ListModel>, Some(every_filter.clone()));
            let sorted = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
            let tree = Rc::new(Cell::new(crate::prefs::tree_view()));
            let kept = Rc::new(RefCell::new(Kept::default()));
            let selection = gtk::MultiSelection::new(Some(tree_model(
                &sorted,
                &every_filter,
                &sorter,
                &tree,
                &kept,
            )));

            Self {
                location: Default::default(),
                show_hidden: Default::default(),
                sort_key: Default::default(),
                sort_reversed: Default::default(),
                search_text: Default::default(),
                search_kind: RefCell::new("any".into()),
                search_date: RefCell::new("any".into()),
                search_match: RefCell::new("name".into()),
                searching: Default::default(),
                extra_filter: Default::default(),
                loading: Default::default(),
                error_message: Default::default(),
                selection,
                listing: Listing::default(),
                sorted,
                tree,
                kept,
                listing_shown: Default::default(),
                sort_deferred: Default::default(),
                filtered,
                list_store: gio::ListStore::new::<gio::FileInfo>(),
                list_of: Default::default(),
                list_gen: Default::default(),
                starred_handler: Default::default(),
                tags_handler: Default::default(),
                tree_handler: Default::default(),
                search_recursive: Cell::new(true),
                search_store: gio::ListStore::new::<gio::FileInfo>(),
                search_gen: Default::default(),
                hidden_filter,
                every_filter,
                sorter,
                hidden_state,
                sort_state,
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderModel {
        const NAME: &'static str = "SpiralFolderModel";
        type Type = super::FolderModel;
    }

    #[glib::derived_properties]
    impl ObjectImpl for FolderModel {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // The folder shown is not where it was: moved, and this is where it
                    // went, or gone, and this is the nearest folder above it still there.
                    glib::subclass::Signal::builder("relocate")
                        .param_types([gio::File::static_type()])
                        .build(),
                ]
            })
        }

        fn dispose(&self) {
            if let Some(id) = self.starred_handler.take() {
                crate::starred::unwatch(id);
            }
            if let Some(id) = self.tags_handler.take() {
                crate::tags::unwatch(id);
            }
            if let Some(id) = self.tree_handler.take() {
                crate::prefs::settings().disconnect(id);
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.listing.connect_loading_notify(glib::clone!(
                #[weak]
                obj,
                move |dl| {
                    let imp = obj.imp();
                    if !dl.loading() {
                        imp.show_listing();
                        if imp.sort_deferred.replace(false) {
                            imp.sorted.set_sorter(Some(&imp.sorter));
                        }
                    }
                    // While searching, the search decides when loading ends.
                    if !imp.searching.get() {
                        imp.set_loading(dl.loading());
                    }
                }
            ));
            // A folder arrives in batches of up to five thousand files. A first batch that
            // is small enough to be the whole folder goes on the pipeline at once; anything
            // more waits for the end of the listing, see `show_listing`.
            self.listing.connect_items_changed(glib::clone!(
                #[weak]
                obj,
                move |dl, _, _, added| {
                    if added > 0 && dl.loading() && dl.n_items() <= SHOW_WHILE_LISTING_UP_TO {
                        obj.imp().show_listing();
                    }
                }
            ));
            let id = crate::starred::watch(glib::clone!(
                #[weak]
                obj,
                move || {
                    if obj.imp().is_starred() {
                        obj.load_list();
                    }
                }
            ));
            self.starred_handler.replace(Some(id));
            let id = crate::tags::watch(glib::clone!(
                #[weak]
                obj,
                move || {
                    if obj.imp().is_tag() {
                        obj.load_list();
                    }
                }
            ));
            self.tags_handler.replace(Some(id));
            let id = crate::prefs::settings().connect_changed(
                Some("use-tree-view"),
                glib::clone!(
                    #[weak]
                    obj,
                    move |s, key| {
                        let imp = obj.imp();
                        imp.tree.set(s.boolean(key));
                        // The tree caches per-row answers, so start a fresh one.
                        imp.selection.set_model(Some(&tree_model(
                            &imp.sorted,
                            &imp.every_filter,
                            &imp.sorter,
                            &imp.tree,
                            &imp.kept,
                        )));
                    }
                ),
            );
            self.tree_handler.replace(Some(id));
            self.listing.connect_error_message_notify(glib::clone!(
                #[weak]
                obj,
                move |dl| {
                    obj.imp().error_message.replace(dl.error_message());
                    obj.notify_error_message();
                }
            ));
            self.listing.connect_gone(glib::clone!(
                #[weak]
                obj,
                move |_| obj.location_gone()
            ));
            LIVE.with(|live| {
                let mut live = live.borrow_mut();
                live.retain(|w| w.upgrade().is_some());
                live.push(obj.downgrade());
            });
            let kept = self.kept.clone();
            self.listing
                .connect_updating(move |_, info| note_unfolded(&kept, info));
            self.kept.borrow_mut().selection = self.selection.downgrade();
            let kept = self.kept.clone();
            self.selection
                .connect_selection_changed(move |sel, position, n| {
                    note_selection(&kept, sel, position, n)
                });
            // After the views have seen the change: a row selected before they have it
            // points them at one they do not know.
            let kept = self.kept.clone();
            self.selection.connect_closure(
                "items-changed",
                true,
                glib::closure_local!(move |sel: gtk::MultiSelection,
                                           position: u32,
                                           removed: u32,
                                           added: u32| {
                    keep_selection(&kept, &sel, position, removed, added)
                }),
            );
        }
    }

    impl FolderModel {
        /// Put the listing on the pipeline, once. Sorting a batch of files into what is
        /// already there makes the sorted model rebuild, and every model and view below
        /// it follow: the view binds hundreds of cells each time. A folder that does not
        /// fit in one batch is therefore kept off the pipeline until it is complete, and
        /// sorted once. One that is taking long is shown anyway, in the order it arrives,
        /// and put in order at the end, so a slow disk or share does not leave the window
        /// blank.
        pub(super) fn show_listing(&self) {
            if self.listing_shown.replace(true) || self.is_list() {
                return;
            }
            if self.listing.loading() && self.listing.n_items() > SHOW_WHILE_LISTING_UP_TO {
                self.sort_deferred.set(true);
                self.sorted.set_sorter(gtk::Sorter::NONE);
            }
            // The search results are what is shown while searching; the listing goes on
            // when the search ends.
            if !self.searching.get() {
                self.filtered.set_model(Some(&self.listing));
            }
        }

        /// List `file`, or nothing, from the start. The old listing comes off the
        /// pipeline first, so the views empty once rather than once per batch of the new
        /// folder, and go on again through `show_listing`.
        pub(super) fn start_listing(&self, file: Option<&gio::File>) {
            self.listing_shown.set(false);
            if !self.searching.get() {
                self.filtered.set_model(gio::ListModel::NONE);
            }
            if self.sort_deferred.replace(false) {
                self.sorted.set_sorter(Some(&self.sorter));
            }
            self.listing.set_file(file);
            if file.is_some() {
                glib::timeout_add_local_once(
                    SHOW_LISTING_AFTER,
                    glib::clone!(
                        #[weak(rename_to = obj)]
                        self.obj(),
                        move || {
                            let imp = obj.imp();
                            if imp.listing.loading() && imp.listing.n_items() > 0 {
                                imp.show_listing();
                            }
                        }
                    ),
                );
            }
        }

        pub(super) fn set_location(&self, file: Option<gio::File>) {
            let list = file.as_ref().is_some_and(is_list_location);
            self.start_listing(file.as_ref().filter(|_| !list));
            self.location.replace(file);
            if list {
                self.filtered.set_model(Some(&self.list_store));
                self.obj().load_list();
            }
            self.sync_hidden();
        }

        pub(super) fn is_starred(&self) -> bool {
            self.location
                .borrow()
                .as_ref()
                .is_some_and(crate::starred::is_starred_location)
        }

        pub(super) fn is_tag(&self) -> bool {
            self.location
                .borrow()
                .as_ref()
                .is_some_and(crate::tags::is_tag_location)
        }

        /// Whether the location is a list of files rather than a folder.
        pub(super) fn is_list(&self) -> bool {
            self.location
                .borrow()
                .as_ref()
                .is_some_and(is_list_location)
        }

        /// Put the search results on the pipeline, unless they are there already.
        pub(super) fn show_search(&self) {
            if !self.searching.replace(true) {
                self.filtered.set_model(Some(&self.search_store));
                self.obj().notify_searching();
            }
        }

        pub(super) fn set_loading(&self, v: bool) {
            if self.loading.replace(v) != v {
                self.obj().notify_loading();
            }
        }

        fn set_show_hidden(&self, v: bool) {
            if self.show_hidden.replace(v) != v {
                self.sync_hidden();
            }
        }

        /// Favorites and tags always show everything in them, hidden or not.
        fn sync_hidden(&self) {
            let show = self.show_hidden.get() || self.is_list();
            if self.hidden_state.replace(show) == show {
                return;
            }
            self.hidden_filter.changed(if show {
                gtk::FilterChange::LessStrict
            } else {
                gtk::FilterChange::MoreStrict
            });
        }

        fn set_sort_key(&self, key: SortKey) {
            self.sort_key.set(key);
            self.sort_state.set((key, self.sort_reversed.get()));
            self.sorter.changed(gtk::SorterChange::Different);
        }

        fn set_sort_reversed(&self, rev: bool) {
            self.sort_reversed.set(rev);
            self.sort_state.set((self.sort_key.get(), rev));
            self.sorter.changed(gtk::SorterChange::Inverted);
        }

        fn set_search_text(&self, text: String) {
            let text = text.to_lowercase();
            if *self.search_text.borrow() == text {
                return;
            }
            self.search_text.replace(text.clone());
            let searching = !text.is_empty();
            if self.searching.replace(searching) != searching {
                if searching {
                    self.filtered.set_model(Some(&self.search_store));
                } else {
                    self.search_gen.set(self.search_gen.get() + 1);
                    self.search_store.remove_all();
                    let root: Option<&gio::ListModel> = if self.is_list() {
                        Some(self.list_store.upcast_ref())
                    } else if self.listing_shown.get() {
                        Some(self.listing.upcast_ref())
                    } else {
                        None
                    };
                    self.filtered.set_model(root);
                    self.set_loading(!self.is_list() && self.listing.loading());
                }
                self.obj().notify_searching();
            }
            if searching {
                self.obj().restart_search();
            }
        }

        fn set_search_kind(&self, kind: String) {
            if self.search_kind.replace(kind.clone()) != kind && self.searching.get() {
                self.obj().restart_search();
            }
        }

        fn set_search_date(&self, date: String) {
            if self.search_date.replace(date.clone()) != date && self.searching.get() {
                self.obj().restart_search();
            }
        }

        fn set_search_match(&self, m: String) {
            if self.search_match.replace(m.clone()) != m && self.searching.get() {
                self.obj().restart_search();
            }
        }

        fn set_extra_filter(&self, filter: Option<gtk::Filter>) {
            if self.extra_filter.replace(filter.clone()).is_some() {
                // Index 2 is always the extra filter when present.
                self.every_filter.remove(2);
            }
            if let Some(f) = filter {
                self.every_filter.append(f);
            }
        }
    }
}

/// Tree over `sorted` whose folders unfold into their own filtered, sorted listing while
/// `tree` is on. Never passthrough, so every item the views see is a `TreeListRow`.
fn tree_model(
    sorted: &gtk::SortListModel,
    filter: &gtk::EveryFilter,
    sorter: &gtk::CustomSorter,
    tree: &Rc<Cell<bool>>,
    kept: &Rc<RefCell<Kept>>,
) -> gtk::TreeListModel {
    let (filter, sorter, tree, kept) = (filter.clone(), sorter.clone(), tree.clone(), kept.clone());
    gtk::TreeListModel::new(sorted.clone(), false, false, move |obj| {
        let info = obj.downcast_ref::<gio::FileInfo>()?;
        if !tree.get() || !file_utils::is_dir(info) {
            return None;
        }
        // GTK asks once to learn whether the row can expand and drops the answer; only
        // the second, kept model gets as far as reading the folder.
        let dir = Listing::deferred(&file_utils::file_of(info));
        let kept = kept.clone();
        dir.connect_updating(move |_, info| note_unfolded(&kept, info));
        let filtered = gtk::FilterListModel::new(Some(dir), Some(filter.clone()));
        Some(gtk::SortListModel::new(Some(filtered), Some(sorter.clone())).upcast())
    })
}

/// What is selected and unfolded, kept for the rows that come back from a change without
/// it. A row goes out and comes back when its file is read again, when the sort moves it,
/// and with every row between where it was and where it goes: the tree puts each of them
/// in anew, and the selection lets them go. The info behind a row stays the same through
/// that; the children of a folder unfolded again are listed afresh, and are known by
/// their location instead.
#[derive(Default)]
struct Kept {
    infos: HashSet<gio::FileInfo>,
    /// Locations of the selected rows inside unfolded folders.
    uris: HashSet<String>,
    /// Folders that were unfolded when their row was about to be put in anew.
    unfolded: HashSet<gio::FileInfo>,
    selection: glib::WeakRef<gtk::MultiSelection>,
    /// Set while a pass that forgets what changes took away for good waits its turn.
    prune_queued: bool,
}

/// The row at `pos` and the info in it.
fn row_info(sel: &gtk::MultiSelection, pos: u32) -> Option<(gtk::TreeListRow, gio::FileInfo)> {
    let row = sel.item(pos).and_downcast::<gtk::TreeListRow>()?;
    let info = row.item().and_downcast::<gio::FileInfo>()?;
    Some((row, info))
}

/// Keep up with the selection over the rows it changed on.
fn note_selection(kept: &RefCell<Kept>, sel: &gtk::MultiSelection, position: u32, n: u32) {
    if sel.selection().is_empty() {
        let mut kept = kept.borrow_mut();
        kept.infos.clear();
        kept.uris.clear();
        return;
    }
    let rows: Vec<(bool, u32, gio::FileInfo)> = (position..position + n)
        .filter_map(|pos| {
            let (row, info) = row_info(sel, pos)?;
            Some((sel.is_selected(pos), row.depth(), info))
        })
        .collect();
    let mut kept = kept.borrow_mut();
    for (selected, depth, info) in rows {
        let uri = (depth > 0).then(|| file_utils::file_of(&info).uri().to_string());
        if selected {
            if let Some(uri) = uri {
                kept.uris.insert(uri);
            }
            kept.infos.insert(info);
        } else {
            if let Some(uri) = uri {
                kept.uris.remove(&uri);
            }
            kept.infos.remove(&info);
        }
    }
}

/// A folder about to be read again in place is folded by the tree as its row goes in
/// anew; note whether it was unfolded, to unfold it again.
fn note_unfolded(kept: &RefCell<Kept>, info: &gio::FileInfo) {
    if !file_utils::is_dir(info) {
        return;
    }
    let Some(sel) = kept.borrow().selection.upgrade() else {
        return;
    };
    let Some(tree) = sel.model().and_downcast::<gtk::TreeListModel>() else {
        return;
    };
    // Nothing unfolded anywhere: every row is one of the folder's own.
    if tree.n_items() == tree.model().n_items() {
        return;
    }
    let unfolded = (0..tree.n_items())
        .filter_map(|pos| tree.row(pos))
        .find(|row| {
            row.item()
                .is_some_and(|item| &item == info.upcast_ref::<glib::Object>())
        })
        .is_some_and(|row| row.is_expanded());
    if unfolded {
        kept.borrow_mut().unfolded.insert(info.clone());
    }
}

/// Select again, and unfold again, the rows a change put back without it. What changes
/// took away for good is forgotten once they are over: a thousand selected files deleted
/// one by one are a thousand changes.
fn keep_selection(
    kept: &Rc<RefCell<Kept>>,
    sel: &gtk::MultiSelection,
    position: u32,
    removed: u32,
    added: u32,
) {
    let empty = {
        let kept = kept.borrow();
        kept.infos.is_empty() && kept.uris.is_empty() && kept.unfolded.is_empty()
    };
    if added > 0 && !empty {
        let mut select = Vec::new();
        let mut unfold = Vec::new();
        for pos in position..position + added {
            let Some((row, info)) = row_info(sel, pos) else {
                continue;
            };
            let kept = kept.borrow();
            if kept.infos.contains(&info)
                || (row.depth() > 0
                    && kept
                        .uris
                        .contains(file_utils::file_of(&info).uri().as_str()))
            {
                select.push(pos);
            }
            if kept.unfolded.contains(&info) {
                unfold.push((row, info));
            }
        }
        for pos in select {
            if !sel.is_selected(pos) {
                sel.select_item(pos, false);
            }
        }
        // From the bottom up, so each unfolding leaves the rows above it where they are.
        for (row, info) in unfold.into_iter().rev() {
            kept.borrow_mut().unfolded.remove(&info);
            row.set_expanded(true);
        }
    }
    if removed > 0 && !std::mem::replace(&mut kept.borrow_mut().prune_queued, true) {
        let kept = kept.clone();
        glib::idle_add_local_once(move || prune(&kept));
    }
}

/// Keep what is selected now, and nothing else; but the locations inside unfolded folders
/// are kept for as long as anything is selected. A folder unfolded again lists its
/// children afresh, a while after the change that unfolded it.
fn prune(kept: &RefCell<Kept>) {
    kept.borrow_mut().prune_queued = false;
    let Some(sel) = kept.borrow().selection.upgrade() else {
        return;
    };
    let infos: Vec<gio::FileInfo> = {
        let set = sel.selection();
        (0..set.size())
            .filter_map(|i| Some(row_info(&sel, set.nth(i as u32))?.1))
            .collect()
    };
    let mut kept = kept.borrow_mut();
    kept.infos = infos.into_iter().collect();
    kept.unfolded.clear();
}

thread_local! {
    /// Every folder model alive, for what an operation did to reach each that shows it.
    static LIVE: RefCell<Vec<glib::WeakRef<FolderModel>>> = const { RefCell::new(Vec::new()) };
    /// What operations moved or renamed a moment ago, and when: a folder shown that goes
    /// away is looked for here before it is taken for gone.
    static MOVED: RefCell<Vec<(gio::File, gio::File, std::time::Instant)>> =
        const { RefCell::new(Vec::new()) };
}

/// How long a move is remembered for a folder that went away to be followed there.
const MOVED_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a folder that went away waits before looking for where: the operation that
/// moved it says so a moment after the folder hears of it.
const GONE_WAIT: std::time::Duration = std::time::Duration::from_millis(300);

/// An operation moved or renamed `from` to `to`.
pub fn moved(from: &gio::File, to: &gio::File) {
    MOVED.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(_, _, at)| at.elapsed() < MOVED_FOR);
        m.push((from.clone(), to.clone(), std::time::Instant::now()));
    });
}

/// Where `file` is now, if it or a folder above it was moved: `to` with the rest of the
/// way from `from` to `file` after it.
fn moved_to(file: &gio::File, moves: &[(gio::File, gio::File)]) -> Option<gio::File> {
    moves.iter().rev().find_map(|(from, to)| {
        if file.equal(from) {
            Some(to.clone())
        } else {
            Some(to.resolve_relative_path(from.relative_path(file)?))
        }
    })
}

/// Whether `file` is `other` or somewhere below it.
fn at_or_below(file: &gio::File, other: &gio::File) -> bool {
    file.equal(other) || file.has_prefix(other)
}

/// Tell every folder shown what an operation did: the folders that hold what it touched
/// read those files again, a search holding them follows them, and a view whose folder it
/// moved or took away goes along.
pub fn files_changed(changes: &crate::ops::Changes) {
    touch(&changes.files());
    let live: Vec<FolderModel> = LIVE.with(|live| {
        let mut live = live.borrow_mut();
        live.retain(|w| w.upgrade().is_some());
        live.iter().filter_map(|w| w.upgrade()).collect()
    });
    for model in live {
        model.follow(changes);
    }
}

/// The file info behind a view item, which is a `TreeListRow` around it.
pub fn info_of(obj: &glib::Object) -> Option<gio::FileInfo> {
    match obj.downcast_ref::<gtk::TreeListRow>() {
        Some(row) => row.item().and_downcast(),
        None => obj.downcast_ref::<gio::FileInfo>().cloned(),
    }
}

glib::wrapper! {
    pub struct FolderModel(ObjectSubclass<imp::FolderModel>);
}

impl Default for FolderModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl FolderModel {
    pub fn new(location: &gio::File) -> Self {
        glib::Object::builder()
            .property("location", location)
            .build()
    }

    /// Files currently selected, in view order.
    pub fn selected_files(&self) -> Vec<gio::File> {
        self.selected_infos()
            .iter()
            .map(file_utils::file_of)
            .collect()
    }

    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        let sel = self.selection();
        let set = sel.selection();
        (0..set.size())
            .filter_map(|i| self.info_at(set.nth(i as u32)))
            .collect()
    }

    /// The file info shown at view position `pos`.
    pub fn info_at(&self, pos: u32) -> Option<gio::FileInfo> {
        self.selection().item(pos).and_then(|o| info_of(&o))
    }

    pub fn row_at(&self, pos: u32) -> Option<gtk::TreeListRow> {
        self.selection().item(pos).and_downcast()
    }

    /// Fold every unfolded folder, for views that cannot show children.
    pub fn collapse_all(&self) {
        let mut i = 0;
        while let Some(row) = self.row_at(i) {
            row.set_expanded(false);
            i += 1;
        }
    }

    pub fn n_items(&self) -> u32 {
        self.selection().n_items()
    }

    /// Items of the folder itself, leaving out those of the folders unfolded in the list.
    pub fn n_top_items(&self) -> u32 {
        self.selection()
            .model()
            .and_downcast::<gtk::TreeListModel>()
            .map_or_else(|| self.n_items(), |tree| tree.model().n_items())
    }

    /// Position of `file` in the current view order, if visible.
    pub fn position_of(&self, file: &gio::File) -> Option<u32> {
        (0..self.n_items()).find(|&i| {
            self.info_at(i)
                .is_some_and(|info| file_utils::file_of(&info).equal(file))
        })
    }

    /// Positions of `files` in the current view order, in one pass. `position_of` walks
    /// the whole list for each file it is asked about, which pasting a hundred files into
    /// a folder of a hundred thousand cannot afford.
    pub fn positions_of(&self, files: &[gio::File]) -> Vec<u32> {
        if files.is_empty() {
            return Vec::new();
        }
        let wanted: std::collections::HashSet<String> =
            files.iter().map(|f| f.uri().to_string()).collect();
        (0..self.n_items())
            .filter(|&i| {
                self.info_at(i)
                    .is_some_and(|info| wanted.contains(file_utils::file_of(&info).uri().as_str()))
            })
            .collect()
    }

    /// Stop reading, keeping what has arrived. A search is abandoned; a listing stops where
    /// it is and goes on following the folder.
    pub fn stop_loading(&self) {
        let imp = self.imp();
        if !self.loading() {
            return;
        }
        if imp.searching.get() || imp.is_list() {
            imp.search_gen.set(imp.search_gen.get() + 1);
            imp.list_gen.set(imp.list_gen.get() + 1);
            imp.set_loading(false);
            return;
        }
        imp.listing.stop();
    }

    /// Why the listing stopped, where it did. The message alone is on the property; this
    /// is for the one caller that has to tell one failure from another.
    pub fn error(&self) -> Option<glib::Error> {
        self.imp().listing.error()
    }

    /// The scheme of every entry left out of the listing for want of a backend, one per
    /// entry, sorted, so a page can say how many were found and what would open them.
    pub fn unreachable_schemes(&self) -> Vec<String> {
        let dl = &self.imp().listing;
        let mut schemes: Vec<String> = (0..dl.n_items())
            .filter_map(|i| dl.item(i).and_downcast::<gio::FileInfo>())
            .filter(file_utils::is_unreachable)
            .filter_map(|info| file_utils::target_of(&info)?.uri_scheme())
            .map(|s| s.to_string())
            .collect();
        schemes.sort();
        schemes
    }

    /// The folder shown went away. Once whatever moved it has had the moment it takes to
    /// say so, follow it there, or else go up to the nearest folder still there.
    fn location_gone(&self) {
        let Some(location) = self.location() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                glib::timeout_future(GONE_WAIT).await;
                if !model.location().is_some_and(|l| l.equal(&location)) {
                    return;
                }
                let moves: Vec<(gio::File, gio::File)> = MOVED.with(|m| {
                    m.borrow()
                        .iter()
                        .filter(|(_, _, at)| at.elapsed() < MOVED_FOR)
                        .map(|(from, to, _)| (from.clone(), to.clone()))
                        .collect()
                });
                if let Some(to) = moved_to(&location, &moves) {
                    model.emit_by_name::<()>("relocate", &[&to]);
                    return;
                }
                model.go_up_from(location).await;
            }
        ));
    }

    /// Go to the nearest folder above `location` that is still there, unless `location`
    /// is back, or the model has moved on meanwhile.
    async fn go_up_from(&self, location: gio::File) {
        let mut at = Some(location.clone());
        while let Some(dir) = at {
            let there = dir
                .query_info_future(
                    "standard::type",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
                .is_ok_and(|info| info.file_type() == gio::FileType::Directory);
            if !self.location().is_some_and(|l| l.equal(&location)) {
                return;
            }
            if there {
                if !dir.equal(&location) {
                    self.emit_by_name::<()>("relocate", &[&dir]);
                }
                return;
            }
            at = dir.parent();
        }
        self.emit_by_name::<()>("relocate", &[&gio::File::for_path(glib::home_dir())]);
    }

    /// Follow what an operation did: to where it moved the folder shown, or up from where
    /// it took it away; and in the results of a search, to where it moved them, or out of
    /// the list where it took them away.
    fn follow(&self, changes: &crate::ops::Changes) {
        let imp = self.imp();
        if let Some(location) = self.location().filter(|_| !imp.is_list()) {
            if let Some(to) = moved_to(&location, &changes.moved) {
                self.emit_by_name::<()>("relocate", &[&to]);
                return;
            }
            if changes.gone.iter().any(|g| at_or_below(&location, g)) {
                glib::spawn_future_local(glib::clone!(
                    #[weak(rename_to = model)]
                    self,
                    async move { model.go_up_from(location).await }
                ));
                return;
            }
        }
        if !imp.searching.get() {
            return;
        }
        let affected: Vec<(gio::FileInfo, gio::File)> = imp
            .search_store
            .iter::<gio::FileInfo>()
            .flatten()
            .filter_map(|info| {
                let file = file_utils::file_of(&info);
                if let Some(to) = moved_to(&file, &changes.moved) {
                    return Some((info, to));
                }
                changes
                    .gone
                    .iter()
                    .any(|g| at_or_below(&file, g))
                    .then_some((info, file))
            })
            .collect();
        if affected.is_empty() {
            return;
        }
        let generation = imp.search_gen.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                for (info, file) in affected {
                    let read = file
                        .query_info_future(
                            file_utils::ATTRIBUTES,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await;
                    let store = &model.imp().search_store;
                    if model.imp().search_gen.get() != generation {
                        return;
                    }
                    let Some(pos) = store.find(&info) else {
                        continue;
                    };
                    match read {
                        // Read into the info the row already has, which keeps the row
                        // selected if it was.
                        Ok(fresh) => {
                            fresh.copy_into(&info);
                            info.set_attribute_object("standard::file", &file);
                            file_utils::forget_sort_keys(&info);
                            store.items_changed(pos, 1, 1);
                        }
                        Err(e) if e.matches(gio::IOErrorEnum::NotFound) => store.remove(pos),
                        Err(_) => {}
                    }
                }
            }
        ));
    }

    pub fn reload(&self) {
        let imp = self.imp();
        if imp.searching.get() {
            self.restart_search();
            return;
        }
        if imp.is_list() {
            self.load_list();
            return;
        }
        imp.start_listing(self.location().as_ref());
    }
}
