//! `DirectoryList` -> filter -> sort -> selection pipeline shared by every view of a folder.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::enums::SortKey;

/// How many files a listing may hold and still go on the pipeline before it is complete.
const SHOW_WHILE_LISTING_UP_TO: u32 = 2000;

/// How long a listing may keep the window blank before what has arrived is shown.
const SHOW_LISTING_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// How long a selected file the directory list is reading again waits to be selected
/// again once its fresh row is in.
const REPLACED_FOR: std::time::Duration = std::time::Duration::from_secs(2);

/// The most selected files the monitor looks through for each change it reports.
const EXPECT_UP_TO: u64 = 256;
use crate::file_utils;
use crate::{gio, glib, gtk};

mod lists;
mod search;

use lists::*;

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

        pub dir_list: gtk::DirectoryList,
        /// Sorted flat list, root of the tree the selection sits on.
        sorted: gtk::SortListModel,
        /// Whether folders unfold in place; read by the tree's child-model function.
        tree: Rc<Cell<bool>>,
        /// `DirectoryList` only tracks additions, removals and attribute changes; this one
        /// catches files rewritten in place so sizes, dates and thumbnails follow.
        monitor: RefCell<Option<gio::FileMonitor>>,
        /// Files the monitor reported, waiting for the next pass over the list.
        pub pending: RefCell<Vec<gio::File>>,
        pub refresh_queued: Cell<bool>,
        /// Selected files the directory list is about to put a fresh row in for, under
        /// the name they will have, and when that was heard of; see `take_back`.
        pub replaced: RefCell<Vec<(gio::File, i64)>>,
        /// Whether the listing is on the pipeline; a big one waits there for its end,
        /// see `show_listing`.
        listing_shown: Cell<bool>,
        /// Set while a listing shown before its end is waiting for its order.
        sort_deferred: Cell<bool>,
        /// Root of the pipeline: `dir_list`, or `list_store` for the locations that are a
        /// list of files rather than a folder, `starred:///` and `tag:///`.
        filtered: gtk::FilterListModel,
        /// Holds what a stopped listing had read, since a directory list drops its items
        /// the moment it is told to stop.
        pub stopped_store: gio::ListStore,
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

            let dir_list = gtk::DirectoryList::new(Some(file_utils::ATTRIBUTES), gio::File::NONE);
            dir_list.set_monitored(true);

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
            // it the entry answers nothing but an error.
            let reachable_filter = gtk::CustomFilter::new(|obj| {
                let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
                !file_utils::is_unreachable(info)
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
            let selection =
                gtk::MultiSelection::new(Some(tree_model(&sorted, &every_filter, &sorter, &tree)));

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
                dir_list,
                sorted,
                tree,
                monitor: Default::default(),
                pending: Default::default(),
                refresh_queued: Default::default(),
                replaced: Default::default(),
                listing_shown: Default::default(),
                sort_deferred: Default::default(),
                filtered,
                stopped_store: gio::ListStore::new::<gio::FileInfo>(),
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
            self.dir_list.connect_loading_notify(glib::clone!(
                #[weak]
                obj,
                move |dl| {
                    let imp = obj.imp();
                    if !dl.is_loading() {
                        imp.show_listing();
                        if imp.sort_deferred.replace(false) {
                            imp.sorted.set_sorter(Some(&imp.sorter));
                        }
                    }
                    // While searching, the search decides when loading ends.
                    if !imp.searching.get() {
                        imp.set_loading(dl.is_loading());
                    }
                }
            ));
            // A folder arrives in batches of up to five thousand files. A first batch that
            // is small enough to be the whole folder goes on the pipeline at once; anything
            // more waits for the end of the listing, see `show_listing`.
            self.dir_list.connect_items_changed(glib::clone!(
                #[weak]
                obj,
                move |dl, _, _, added| {
                    if added > 0 && dl.is_loading() && dl.n_items() <= SHOW_WHILE_LISTING_UP_TO {
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
                        )));
                    }
                ),
            );
            self.tree_handler.replace(Some(id));
            self.dir_list.connect_error_notify(glib::clone!(
                #[weak]
                obj,
                move |dl| {
                    obj.imp()
                        .error_message
                        .replace(dl.error().map(|e| e.message().to_string()));
                    obj.notify_error_message();
                }
            ));
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
            if self.dir_list.is_loading() && self.dir_list.n_items() > SHOW_WHILE_LISTING_UP_TO {
                self.sort_deferred.set(true);
                self.sorted.set_sorter(gtk::Sorter::NONE);
            }
            // The search results are what is shown while searching; the listing goes on
            // when the search ends.
            if !self.searching.get() {
                self.filtered.set_model(Some(&self.dir_list));
            }
        }

        /// Show `model` instead of the listing and let the directory list go. What was
        /// read stays on screen, in order: a listing shown before its end may still be
        /// waiting for its sorter.
        pub(super) fn freeze(&self, model: &impl IsA<gio::ListModel>) {
            if self.sort_deferred.replace(false) {
                self.sorted.set_sorter(Some(&self.sorter));
            }
            self.listing_shown.set(true);
            self.filtered.set_model(Some(model));
            self.dir_list.set_file(gio::File::NONE);
        }

        /// List `file`, or nothing, from the start. The old listing comes off the
        /// pipeline first, so the views empty once rather than once per batch of the new
        /// folder, and go on again through `show_listing`.
        pub(super) fn start_listing(&self, file: Option<&gio::File>) {
            self.listing_shown.set(false);
            if !self.searching.get() {
                self.filtered.set_model(gio::ListModel::NONE);
            }
            // What a stopped listing had read is off the pipeline now, and a folder of a
            // hundred thousand files is a lot to go on holding.
            if self.stopped_store.n_items() > 0 {
                self.stopped_store.remove_all();
            }
            if self.sort_deferred.replace(false) {
                self.sorted.set_sorter(Some(&self.sorter));
            }
            // The directory list ignores the file it already has, so listing it again
            // takes a detour through nothing. "Has" as GIO sees it: the same path in
            // another GFile counts.
            if let (Some(old), Some(new)) = (self.dir_list.file(), file)
                && old.equal(new)
            {
                self.dir_list.set_file(gio::File::NONE);
            }
            self.dir_list.set_file(file);
            if file.is_some() {
                glib::timeout_add_local_once(
                    SHOW_LISTING_AFTER,
                    glib::clone!(
                        #[weak(rename_to = obj)]
                        self.obj(),
                        move || {
                            let imp = obj.imp();
                            if imp.dir_list.is_loading() && imp.dir_list.n_items() > 0 {
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
            if let Some(old) = self.monitor.take() {
                old.cancel();
            }
            if let Some(dir) = file.as_ref().filter(|_| !list)
                && let Ok(monitor) = dir
                    .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                monitor.connect_changed(glib::clone!(
                    #[weak(rename_to = obj)]
                    self.obj(),
                    move |_, changed, other, event| match event {
                        gio::FileMonitorEvent::ChangesDoneHint => obj.refresh(changed.clone()),
                        // The directory list reads these again itself -- a tag or a mode
                        // set, a file saved over -- and puts the fresh row in place of
                        // the old one, which the selection does not follow.
                        gio::FileMonitorEvent::AttributeChanged
                        | gio::FileMonitorEvent::Created => obj.expect_back(changed, changed),
                        // A rename takes the old row out and puts a new one in, a
                        // moment later; a file saved over arrives as one too.
                        gio::FileMonitorEvent::Renamed => {
                            if let Some(other) = other {
                                obj.expect_back(changed, other);
                                obj.expect_back(other, other);
                            }
                        }
                        _ => {}
                    }
                ));
                self.monitor.replace(Some(monitor));
            }
            self.location.replace(file);
            self.replaced.take();
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
                        Some(self.dir_list.upcast_ref())
                    } else {
                        None
                    };
                    self.filtered.set_model(root);
                    self.set_loading(!self.is_list() && self.dir_list.is_loading());
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
) -> gtk::TreeListModel {
    let (filter, sorter, tree) = (filter.clone(), sorter.clone(), tree.clone());
    gtk::TreeListModel::new(sorted.clone(), false, false, move |obj| {
        let info = obj.downcast_ref::<gio::FileInfo>()?;
        if !tree.get() || !file_utils::is_dir(info) {
            return None;
        }
        // GTK asks once to learn whether the row can expand and drops the answer, which
        // cancels the listing; the second, kept model is the one that loads.
        let dir = gtk::DirectoryList::new(
            Some(file_utils::ATTRIBUTES),
            Some(&file_utils::file_of(info)),
        );
        dir.set_monitored(true);
        let filtered = gtk::FilterListModel::new(Some(dir), Some(filter.clone()));
        Some(gtk::SortListModel::new(Some(filtered), Some(sorter.clone())).upcast())
    })
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
    /// If `file` is selected, expect it back as `back`: the directory list is reading it
    /// again, or it was renamed, and the row that comes in its place comes unselected.
    /// Asked of the monitor as the change arrives, before the list has its answer. A big
    /// selection is not looked through for every file the folder hears of.
    fn expect_back(&self, file: &gio::File, back: &gio::File) {
        let set = self.selection().selection();
        if set.is_empty() || set.size() > EXPECT_UP_TO {
            return;
        }
        if self.selected_files().iter().any(|f| f.equal(file)) {
            let now = glib::monotonic_time();
            self.imp().replaced.borrow_mut().push((back.clone(), now));
        }
    }

    /// Whether a selected file is on its way back under a fresh row.
    pub fn expects_back(&self) -> bool {
        let since = glib::monotonic_time() - REPLACED_FOR.as_micros() as i64;
        let mut replaced = self.imp().replaced.borrow_mut();
        replaced.retain(|(_, at)| *at >= since);
        !replaced.is_empty()
    }

    /// Where the files expected back are, among those whose fresh rows are in, for the
    /// view to select again; they are expected no more. The others are waited for a
    /// moment longer: the list answers at once, or the change never made it a new row.
    pub fn take_back(&self) -> Vec<u32> {
        if !self.expects_back() {
            return Vec::new();
        }
        let files: Vec<gio::File> = self
            .imp()
            .replaced
            .borrow()
            .iter()
            .map(|(f, _)| f.clone())
            .collect();
        let found = self.positions_of(&files);
        let arrived: Vec<gio::File> = found
            .iter()
            .filter_map(|&pos| self.info_at(pos))
            .map(|info| file_utils::file_of(&info))
            .collect();
        self.imp()
            .replaced
            .borrow_mut()
            .retain(|(f, _)| !arrived.iter().any(|a| a.equal(f)));
        found
    }

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

    /// Stop reading, keeping what has arrived. A search only has to be abandoned; a
    /// listing has to be copied out of the directory list first, which empties itself as
    /// soon as it is told to stop.
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
        let read: Vec<gio::FileInfo> = imp
            .dir_list
            .iter::<glib::Object>()
            .flatten()
            .filter_map(|o| o.downcast::<gio::FileInfo>().ok())
            .collect();
        imp.stopped_store
            .splice(0, imp.stopped_store.n_items(), &read);
        imp.freeze(&imp.stopped_store);
        imp.set_loading(false);
    }

    /// Why the listing stopped, where it did. The message alone is on the property; this
    /// is for the one caller that has to tell one failure from another.
    pub fn error(&self) -> Option<glib::Error> {
        self.imp().dir_list.error()
    }

    /// The scheme of every entry left out of the listing for want of a backend, one per
    /// entry, sorted, so a page can say how many were found and what would open them.
    pub fn unreachable_schemes(&self) -> Vec<String> {
        let dl = &self.imp().dir_list;
        let mut schemes: Vec<String> = (0..dl.n_items())
            .filter_map(|i| dl.item(i).and_downcast::<gio::FileInfo>())
            .filter(file_utils::is_unreachable)
            .filter_map(|info| file_utils::target_of(&info)?.uri_scheme())
            .map(|s| s.to_string())
            .collect();
        schemes.sort();
        schemes
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
