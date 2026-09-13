//! Favorites and tags: lists of files rather than folders, loaded one file at a time, and
//! files changed in place, read again.

use super::*;

/// Favorites and tags are lists of files, not folders: nothing is listed or watched, the
/// files are asked about one by one.
pub(super) fn is_list_location(file: &gio::File) -> bool {
    crate::starred::is_starred_location(file) || crate::tags::is_tag_location(file)
}

impl FolderModel {
    /// Re-read the attributes of files that changed into the infos already in the list, so
    /// bound cells rebind and the sort order follows, while the selection (tracked by
    /// object) survives. The monitor reports one file at a time and a busy folder reports
    /// many in a row, so they are gathered for a moment and answered together: a pass per
    /// file would walk a large folder once per file written into it.
    pub(super) fn refresh(&self, file: gio::File) {
        let imp = self.imp();
        imp.pending.borrow_mut().push(file);
        if imp.refresh_queued.replace(true) {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                glib::timeout_future(std::time::Duration::from_millis(50)).await;
                model.imp().refresh_queued.set(false);
                let files = std::mem::take(&mut *model.imp().pending.borrow_mut());
                model.apply_refresh(files).await;
            }
        ));
    }

    pub(super) async fn apply_refresh(&self, files: Vec<gio::File>) {
        let mut fresh = std::collections::HashMap::new();
        for file in files {
            if let Ok(info) = file
                .query_info_future(
                    file_utils::ATTRIBUTES,
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
            {
                info.set_attribute_object("standard::file", &file);
                fresh.insert(file.uri().to_string(), info);
            }
        }
        let dl = &self.imp().dir_list;
        let hits: Vec<(u32, gio::FileInfo, gio::FileInfo)> = (0..dl.n_items())
            .filter_map(|i| {
                let info = dl.item(i).and_downcast::<gio::FileInfo>()?;
                let new = fresh.get(file_utils::file_of(&info).uri().as_str())?;
                Some((i, info, new.clone()))
            })
            .collect();
        if hits.is_empty() {
            return;
        }
        // Rewriting a row makes the sorted model rebuild, and the selection, which it
        // keeps by position, goes with it: one file being written is enough to clear the
        // lot. A pasted screenshot, written the moment after it was created, would lose
        // the selection it was just given. Remember the files and select them again where
        // they end up.
        let selected = self.selected_files();
        for (pos, info, new) in hits {
            new.copy_into(&info);
            // The keys cached on it describe the name it had a moment ago.
            file_utils::forget_sort_keys(&info);
            dl.items_changed(pos, 1, 1);
        }
        if !selected.is_empty() {
            let sel = self.selection();
            sel.unselect_all();
            for pos in self.positions_of(&selected) {
                sel.select_item(pos, false);
            }
        }
    }

    /// Query every file of the list. A starred file that no longer exists is unstarred;
    /// a file the tag index has wrong -- gone, or without the tag any more -- is taken out
    /// of the index.
    pub(super) fn load_list(&self) {
        let imp = self.imp();
        let generation = imp.list_gen.get() + 1;
        imp.list_gen.set(generation);
        imp.set_loading(true);
        let Some(location) = self.location() else {
            return;
        };
        let starred = crate::starred::is_starred_location(&location);
        let tag = crate::tags::tag_of_location(&location);
        let files = if starred {
            crate::starred::files()
        } else {
            crate::tags::files_with(tag.as_deref())
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                let mut infos = Vec::new();
                let mut missing = Vec::new();
                // (file, tags it was down for and has not got)
                let mut stale: Vec<(gio::File, Vec<String>)> = Vec::new();
                // A file the index has under two paths -- a folder reached through a
                // link or a bind mount as well as where it is -- is one file, listed
                // once: the index keeps both, as each is true.
                // The change time as well, which moves with the mode, the owner and the
                // tags, so a row can tell it is stale.
                let attributes = format!(
                    "{},id::file,time::changed,time::changed-usec",
                    file_utils::ATTRIBUTES
                );
                let mut seen = std::collections::HashSet::new();
                for file in files {
                    match file
                        .query_info_future(
                            &attributes,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    {
                        Ok(info) => {
                            info.set_attribute_object("standard::file", &file);
                            let again = info
                                .attribute_string("id::file")
                                .is_some_and(|id| !seen.insert(id));
                            if starred {
                                if !again {
                                    infos.push(info);
                                }
                                continue;
                            }
                            let has = crate::tags::of_info(&info);
                            let wrong: Vec<String> = crate::tags::indexed(&file)
                                .into_iter()
                                .filter(|t| !has.contains(t))
                                .collect();
                            let listed = match &tag {
                                Some(t) => has.contains(t),
                                None => has.iter().any(|t| !wrong.contains(t)),
                            };
                            if listed && !again {
                                infos.push(info);
                            }
                            if !wrong.is_empty() {
                                stale.push((file, wrong));
                            }
                        }
                        Err(e) if e.matches(gio::IOErrorEnum::NotFound) => missing.push(file),
                        Err(_) => {}
                    }
                }
                let imp = model.imp();
                if imp.list_gen.get() != generation {
                    return;
                }
                // A file tagged, starred or let go in another window or another Spiral
                // touches only its own row, so the rest keep the selection, the focus and
                // the scroll. The list of another location goes in whole, so nothing
                // selected in the one before stays selected in it.
                let store = &imp.list_store;
                let same = imp.list_of.replace(Some(location.clone()));
                let key = |i: &gio::FileInfo| file_utils::file_of(i).uri();
                let changed = |old: &gio::FileInfo, new: &gio::FileInfo| {
                    let ctime = |i: &gio::FileInfo| {
                        (
                            i.attribute_uint64("time::changed"),
                            i.attribute_uint32("time::changed-usec"),
                        )
                    };
                    ctime(old) != ctime(new)
                        || old.modification_date_time() != new.modification_date_time()
                        || old.attribute_string(crate::tags::ATTRIBUTE)
                            != new.attribute_string(crate::tags::ATTRIBUTE)
                };
                let mut fresh: std::collections::HashMap<glib::GString, gio::FileInfo> =
                    infos.iter().map(|i| (key(i), i.clone())).collect();
                let old: Vec<gio::FileInfo> = store.iter().flatten().collect();
                if same.is_some_and(|l| l.equal(&location)) {
                    // A changed file's row goes in anew, which drops it from the selection:
                    // it is selected again once it is in.
                    let selected: std::collections::HashSet<glib::GString> =
                        model.selected_infos().iter().map(key).collect();
                    let mut reselect = Vec::new();
                    for (pos, old) in old.iter().enumerate().rev() {
                        let pos = pos as u32;
                        match fresh.remove(&key(old)) {
                            None => store.remove(pos),
                            Some(new) if changed(old, &new) => {
                                if selected.contains(&key(old)) {
                                    reselect.push(file_utils::file_of(&new));
                                }
                                store.splice(pos, 1, &[new]);
                            }
                            Some(_) => {}
                        }
                    }
                    let added: Vec<gio::FileInfo> = infos
                        .into_iter()
                        .filter(|i| fresh.contains_key(&key(i)))
                        .collect();
                    store.extend_from_slice(&added);
                    let selection = model.selection();
                    for pos in model.positions_of(&reselect) {
                        selection.select_item(pos, false);
                    }
                } else {
                    store.splice(0, store.n_items(), &infos);
                }
                imp.set_loading(false);
                for file in missing {
                    if starred {
                        crate::starred::set_starred(&file, false);
                    } else {
                        for t in crate::tags::indexed(&file) {
                            crate::tags::forget(&t, &file);
                        }
                    }
                }
                for (file, wrong) in stale {
                    for t in wrong {
                        crate::tags::forget(&t, &file);
                    }
                }
            }
        ));
    }
}
