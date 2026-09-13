//! Searching: the hits a search brings, shown in the model, and running it again when what
//! it depends on changes.

use super::*;

impl FolderModel {
    /// What the search on screen has found, in the order it arrived.
    pub fn search_hits(&self) -> Vec<gio::FileInfo> {
        self.imp()
            .search_store
            .iter::<gio::FileInfo>()
            .flatten()
            .collect()
    }

    /// Show `hits` as the results of searching for `text`, as they were when the folder
    /// was left, instead of searching again: coming back to them is instant and they stay
    /// in their order. A search that had not `finished` goes on from there, adding what
    /// it had not found yet. What has gone since is taken out as it is found missing.
    pub fn show_search_hits(&self, text: &str, hits: &[gio::FileInfo], finished: bool) {
        let imp = self.imp();
        imp.search_gen.set(imp.search_gen.get() + 1);
        imp.search_text.replace(text.to_lowercase());
        imp.search_store.splice(0, imp.search_store.n_items(), hits);
        imp.show_search();
        self.notify_search_text();
        if finished {
            imp.set_loading(false);
        } else {
            self.search(true);
        }
        let generation = imp.search_gen.get();
        let hits = hits.to_vec();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                for info in hits {
                    let gone = file_utils::file_of(&info)
                        .query_info_future(
                            "standard::type",
                            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                            glib::Priority::LOW,
                        )
                        .await
                        .is_err_and(|e| e.matches(gio::IOErrorEnum::NotFound));
                    let imp = model.imp();
                    if imp.search_gen.get() != generation {
                        return;
                    }
                    if gone && let Some(pos) = imp.search_store.find(&info) {
                        imp.search_store.remove(pos);
                    }
                }
            }
        ));
    }

    /// Keep a search inside the folder being shown, wherever the preference stands.
    pub fn set_search_recursive(&self, recursive: bool) {
        self.imp().search_recursive.set(recursive);
    }

    /// Drop the results and search again after a short pause, so typing does not start a
    /// walk per keystroke.
    pub(super) fn restart_search(&self) {
        self.search(false);
    }

    /// Search after a short pause. `keep`: what is on screen stays, and only what is not
    /// there yet is added, for a search that comes back unfinished and goes on.
    pub(super) fn search(&self, keep: bool) {
        let imp = self.imp();
        let generation = imp.search_gen.get() + 1;
        imp.search_gen.set(generation);
        let mut seen = std::collections::HashSet::new();
        if keep {
            seen.extend(
                imp.search_store
                    .iter::<gio::FileInfo>()
                    .flatten()
                    .map(|info| file_utils::file_of(&info).uri().to_string()),
            );
        } else {
            imp.search_store.remove_all();
        }
        let mut new = move |hits: Vec<gio::FileInfo>| -> Vec<gio::FileInfo> {
            if !keep {
                return hits;
            }
            hits.into_iter()
                .filter(|info| seen.insert(file_utils::file_of(info).uri().to_string()))
                .collect()
        };
        let Some(root) = self.location() else { return };
        imp.set_loading(true);
        let query = crate::search::Query {
            text: imp.search_text.borrow().clone(),
            matching: match imp.search_match.borrow().as_str() {
                "both" => crate::search::Match::NameOrContent,
                "content" => crate::search::Match::Content,
                _ => crate::search::Match::Name,
            },
            kind: imp.search_kind.borrow().clone(),
            since: crate::search::since_for(&imp.search_date.borrow()),
            recursive: imp.search_recursive.get()
                && !imp.is_list()
                && crate::prefs::recursive_search_for(&root),
            show_hidden: imp.show_hidden.get(),
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                glib::timeout_future(std::time::Duration::from_millis(150)).await;
                let alive = || model.imp().search_gen.get() == generation;
                if !alive() {
                    return;
                }
                // Favorites and tags are a list, not a folder: search among them directly.
                if model.imp().is_list() {
                    let store = &model.imp().list_store;
                    let hits: Vec<gio::FileInfo> = store
                        .iter::<gio::FileInfo>()
                        .flatten()
                        .filter(|i| i.display_name().to_lowercase().contains(&query.text))
                        .collect();
                    let search_store = &model.imp().search_store;
                    search_store.splice(search_store.n_items(), 0, &new(hits));
                } else {
                    crate::search::run(root, query, alive, |hits| {
                        let hits = new(hits);
                        if !hits.is_empty() {
                            let search_store = &model.imp().search_store;
                            search_store.splice(search_store.n_items(), 0, &hits);
                        }
                    })
                    .await;
                }
                if alive() {
                    model.imp().set_loading(false);
                }
            }
        ));
    }
}
