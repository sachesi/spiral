//! `DirectoryList` -> filter -> sort -> selection pipeline shared by every view of a folder.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::enums::SortKey;
use crate::file_utils;
use crate::{gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::FolderModel)]
    pub struct FolderModel {
        #[property(get, set = Self::set_location, nullable)]
        location: RefCell<Option<gio::File>>,
        #[property(get, set = Self::set_show_hidden)]
        show_hidden: Cell<bool>,
        #[property(get, set = Self::set_sort_key, builder(SortKey::Name))]
        sort_key: Cell<SortKey>,
        #[property(get, set = Self::set_sort_reversed)]
        sort_reversed: Cell<bool>,
        #[property(get, set = Self::set_search_text)]
        search_text: RefCell<String>,
        #[property(get, set = Self::set_extra_filter, nullable)]
        extra_filter: RefCell<Option<gtk::Filter>>,
        #[property(get)]
        loading: Cell<bool>,
        #[property(get, nullable)]
        error_message: RefCell<Option<String>>,
        #[property(get)]
        selection: gtk::MultiSelection,

        pub dir_list: gtk::DirectoryList,
        /// `DirectoryList` only tracks additions, removals and attribute changes; this one
        /// catches files rewritten in place so sizes, dates and thumbnails follow.
        monitor: RefCell<Option<gio::FileMonitor>>,
        /// Root of the pipeline: `dir_list`, or `starred_store` for `starred:///`.
        filtered: gtk::FilterListModel,
        pub starred_store: gio::ListStore,
        pub starred_gen: Cell<u64>,
        starred_handler: RefCell<Option<glib::SignalHandlerId>>,
        hidden_filter: gtk::CustomFilter,
        search_filter: gtk::CustomFilter,
        every_filter: gtk::EveryFilter,
        sorter: gtk::CustomSorter,
        // Shared with the filter/sorter closures.
        hidden_state: Rc<Cell<bool>>,
        search_state: Rc<RefCell<String>>,
        sort_state: Rc<Cell<(SortKey, bool)>>,
    }

    impl Default for FolderModel {
        fn default() -> Self {
            let hidden_state = Rc::new(Cell::new(false));
            let search_state = Rc::new(RefCell::new(String::new()));
            let sort_state = Rc::new(Cell::new((SortKey::Name, false)));

            let dir_list = gtk::DirectoryList::new(Some(file_utils::ATTRIBUTES), gio::File::NONE);
            dir_list.set_monitored(true);

            let hidden_filter = gtk::CustomFilter::new(glib::clone!(
                #[strong]
                hidden_state,
                move |obj| {
                    let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
                    hidden_state.get() || !(info.is_hidden() || info.is_backup())
                }
            ));
            let search_filter = gtk::CustomFilter::new(glib::clone!(
                #[strong]
                search_state,
                move |obj| {
                    let needle = search_state.borrow();
                    if needle.is_empty() {
                        return true;
                    }
                    let info = obj.downcast_ref::<gio::FileInfo>().unwrap();
                    info.display_name().to_lowercase().contains(needle.as_str())
                }
            ));
            let every_filter = gtk::EveryFilter::new();
            every_filter.append(hidden_filter.clone());
            every_filter.append(search_filter.clone());

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
                gtk::FilterListModel::new(Some(dir_list.clone()), Some(every_filter.clone()));
            let sorted = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
            let selection = gtk::MultiSelection::new(Some(sorted));

            Self {
                location: Default::default(),
                show_hidden: Default::default(),
                sort_key: Default::default(),
                sort_reversed: Default::default(),
                search_text: Default::default(),
                extra_filter: Default::default(),
                loading: Default::default(),
                error_message: Default::default(),
                selection,
                dir_list,
                monitor: Default::default(),
                filtered,
                starred_store: gio::ListStore::new::<gio::FileInfo>(),
                starred_gen: Default::default(),
                starred_handler: Default::default(),
                hidden_filter,
                search_filter,
                every_filter,
                sorter,
                hidden_state,
                search_state,
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
                crate::starred::list().disconnect(id);
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.dir_list.connect_loading_notify(glib::clone!(
                #[weak]
                obj,
                move |dl| obj.imp().set_loading(dl.is_loading())
            ));
            let id = crate::starred::list().connect_items_changed(glib::clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    if obj.imp().is_starred() {
                        obj.load_starred();
                    }
                }
            ));
            self.starred_handler.replace(Some(id));
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
        fn set_location(&self, file: Option<gio::File>) {
            let starred = file
                .as_ref()
                .is_some_and(crate::starred::is_starred_location);
            self.dir_list.set_file(file.as_ref().filter(|_| !starred));
            if let Some(old) = self.monitor.take() {
                old.cancel();
            }
            if let Some(dir) = file.as_ref().filter(|_| !starred)
                && let Ok(monitor) =
                    dir.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            {
                monitor.connect_changed(glib::clone!(
                    #[weak(rename_to = obj)]
                    self.obj(),
                    move |_, changed, _, event| {
                        if event == gio::FileMonitorEvent::ChangesDoneHint {
                            obj.refresh(changed.clone());
                        }
                    }
                ));
                self.monitor.replace(Some(monitor));
            }
            self.location.replace(file);
            if starred {
                self.filtered.set_model(Some(&self.starred_store));
                self.obj().load_starred();
            } else {
                self.filtered.set_model(Some(&self.dir_list));
            }
            self.sync_hidden();
        }

        pub(super) fn is_starred(&self) -> bool {
            self.location
                .borrow()
                .as_ref()
                .is_some_and(crate::starred::is_starred_location)
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

        /// Favorites always show everything that was starred, hidden or not.
        fn sync_hidden(&self) {
            let show = self.show_hidden.get() || self.is_starred();
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
            let old = self.search_state.replace(text.clone());
            self.search_text.replace(text.clone());
            let change = if text.starts_with(&old) {
                gtk::FilterChange::MoreStrict
            } else if old.starts_with(&text) {
                gtk::FilterChange::LessStrict
            } else {
                gtk::FilterChange::Different
            };
            self.search_filter.changed(change);
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
        let sel = self.selection();
        let set = sel.selection();
        let n = set.size();
        (0..n)
            .filter_map(|i| sel.item(set.nth(i as u32)))
            .filter_map(|o| o.downcast::<gio::FileInfo>().ok())
            .map(|info| file_utils::file_of(&info))
            .collect()
    }

    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        let sel = self.selection();
        let set = sel.selection();
        (0..set.size())
            .filter_map(|i| sel.item(set.nth(i as u32)))
            .filter_map(|o| o.downcast::<gio::FileInfo>().ok())
            .collect()
    }

    pub fn n_items(&self) -> u32 {
        self.selection().n_items()
    }

    /// Position of `file` in the current view order, if visible.
    pub fn position_of(&self, file: &gio::File) -> Option<u32> {
        let sel = self.selection();
        (0..sel.n_items()).find(|&i| {
            sel.item(i)
                .and_downcast::<gio::FileInfo>()
                .is_some_and(|info| file_utils::file_of(&info).equal(file))
        })
    }

    pub fn reload(&self) {
        let imp = self.imp();
        if imp.is_starred() {
            self.load_starred();
            return;
        }
        let file = self.location();
        imp.dir_list.set_file(gio::File::NONE);
        imp.dir_list.set_file(file.as_ref());
    }

    /// Re-read `file`'s attributes into the info already in the list, so bound cells rebind
    /// and the sort order follows, while the selection (tracked by object) survives.
    fn refresh(&self, file: gio::File) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                let Ok(fresh) = file
                    .query_info_future(
                        file_utils::ATTRIBUTES,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                else {
                    return;
                };
                let dl = &model.imp().dir_list;
                // ponytail: linear scan per change; index by name if huge folders churn.
                let found = (0..dl.n_items())
                    .filter_map(|i| {
                        dl.item(i)
                            .and_downcast::<gio::FileInfo>()
                            .map(|info| (i, info))
                    })
                    .find(|(_, info)| file_utils::file_of(info).equal(&file));
                if let Some((pos, info)) = found {
                    fresh.set_attribute_object("standard::file", &file);
                    fresh.copy_into(&info);
                    dl.items_changed(pos, 1, 1);
                }
            }
        ));
    }

    /// Query every starred file; entries that no longer exist are unstarred.
    fn load_starred(&self) {
        let imp = self.imp();
        let generation = imp.starred_gen.get() + 1;
        imp.starred_gen.set(generation);
        imp.set_loading(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                let mut infos = Vec::new();
                let mut missing = Vec::new();
                for file in crate::starred::files() {
                    match file
                        .query_info_future(
                            file_utils::ATTRIBUTES,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    {
                        Ok(info) => {
                            info.set_attribute_object("standard::file", &file);
                            infos.push(info);
                        }
                        Err(e) if e.matches(gio::IOErrorEnum::NotFound) => missing.push(file),
                        Err(_) => {}
                    }
                }
                let imp = model.imp();
                if imp.starred_gen.get() != generation {
                    return;
                }
                imp.starred_store
                    .splice(0, imp.starred_store.n_items(), &infos);
                imp.set_loading(false);
                for file in missing {
                    crate::starred::set_starred(&file, false);
                }
            }
        ));
    }
}
