//! Making and naming: new folders and documents, the templates they come from, and renaming
//! one file or many.

use super::*;

/// A menu of the templates found, each starting a document from one of them, ending in
/// the empty document the menu has when there are no templates at all.
pub(super) fn templates_menu(entries: &[crate::templates::Entry]) -> gio::Menu {
    use crate::templates::Entry;
    let menu = gio::Menu::new();
    for entry in entries {
        match entry {
            Entry::File { name, file } => {
                let item = gio::MenuItem::new(Some(&mnemonic_safe(name)), None);
                item.set_action_and_target_value(
                    Some("view.new-from-template"),
                    Some(&file.uri().to_variant()),
                );
                menu.append_item(&item);
            }
            Entry::Folder { name, children } => {
                menu.append_submenu(Some(&mnemonic_safe(name)), &templates_menu(children));
            }
        }
    }
    let empty = gio::Menu::new();
    empty.append(Some(&gettext("_Empty Document")), Some("view.new-file"));
    menu.append_section(None, &empty);
    menu
}

impl BrowserView {
    pub(super) fn new_folder(&self) {
        let Some(parent) = self.location() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let Some(name) = crate::naming::new_folder_dialog(&view, &parent, "").await else {
                    return;
                };
                // Selected and given the keyboard once it appears: a folder is made to be
                // used, and what follows -- opening it, renaming it, dragging into it --
                // starts from there.
                if view.manager().is_some() {
                    view.submit_and_select(JobKind::CreateFolder { parent, name });
                    return;
                }
                // The file chooser has no operations to hand it to; making a folder is
                // quick enough to do here.
                let folder = parent.child(&name);
                match folder.make_directory_future(glib::Priority::DEFAULT).await {
                    Ok(()) => view.select_files_when_loaded(vec![folder]),
                    Err(e) => view.show_error(&gettext("Could Not Create Folder"), e.message()),
                }
            }
        ));
    }

    /// Ask for a name and make a folder of it holding the selection. The name starts as
    /// what the selected names have in common, and the folder is selected once made.
    pub(super) fn new_folder_with_selection(&self) {
        let (Some(parent), files) = (self.location(), self.selected()) else {
            return;
        };
        let names: Vec<String> = self
            .model()
            .selected_infos()
            .iter()
            .map(|i| i.display_name().to_string())
            .collect();
        let suggested = crate::naming::common_name(&names);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let Some(name) = crate::naming::new_folder_dialog(&view, &parent, &suggested).await
                else {
                    return;
                };
                let Some(mgr) = view.manager() else { return };
                let folder = parent.child(&name);
                let job = mgr.submit(JobKind::NewFolderWith {
                    parent,
                    name,
                    files,
                });
                // The folder, not what went into it, which is out of sight.
                job.connect_status_notify(glib::clone!(
                    #[weak]
                    view,
                    move |job| {
                        if job.status() == JobStatus::Done {
                            view.select_files_when_loaded(vec![folder.clone()]);
                        }
                    }
                ));
            }
        ));
    }

    /// A document is named before it is made, as a folder is. The name starts as the
    /// template's own, or as the one an empty document is given, and what lands in the
    /// folder is selected.
    pub(super) fn new_document(&self, template: Option<gio::File>) {
        let Some(parent) = self.location() else {
            return;
        };
        let suggested = match &template {
            Some(file) => crate::ops::name(file),
            None => gettext("Untitled Document"),
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some(name) = crate::naming::new_file_dialog(&view, &parent, &suggested).await
                {
                    view.submit_and_select(JobKind::CreateFile {
                        parent,
                        name,
                        template,
                    });
                }
            }
        ));
    }

    /// The document entry of the background menu: an item on its own where the templates
    /// folder is empty or missing, and what is in that folder where it is not.
    pub(super) fn sync_new_menu(&self) {
        let section = &self.imp().new_section;
        // Everything after "New Folder…", which the template puts there.
        while section.n_items() > 1 {
            section.remove(1);
        }
        let entries = crate::templates::entries();
        let label = gettext("New _Document");
        if entries.is_empty() {
            section.append(Some(&label), Some("view.new-file"));
        } else {
            section.append_submenu(Some(&label), &templates_menu(&entries));
        }
    }

    pub(super) fn rename_selected(&self) {
        let infos = self.model().selected_infos();
        if infos.len() > 1 {
            self.rename_many();
            return;
        }
        let [info] = infos.as_slice() else { return };
        let file = file_utils::file_of(info);
        let Some(dir) = file.parent() else { return };
        let old = info.display_name().to_string();
        let is_folder = file_utils::is_dir(info);
        let anchor = self
            .model()
            .position_of(&file)
            .and_then(|pos| self.cell_bounds(pos))
            .unwrap_or_else(|| gtk::gdk::Rectangle::new(self.width() / 2, self.height() / 2, 1, 1));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some(new_name) =
                    crate::naming::rename_popover(&view, &anchor, &dir, &old, is_folder).await
                    && new_name != old
                {
                    view.submit(JobKind::Rename {
                        renames: vec![(file, new_name)],
                    });
                }
            }
        ));
    }

    /// Rename the whole selection by one rule. The names already in the folder are handed
    /// to the dialog, so a clash is shown while the rule is typed rather than met as a
    /// conflict once the job runs.
    pub(super) fn rename_many(&self) {
        let files = self.selected();
        let model = self.model();
        let names: Vec<String> = model
            .selected_infos()
            .iter()
            .map(|i| i.display_name().to_string())
            .collect();
        let renamed: std::collections::HashSet<&String> = names.iter().collect();
        let others: std::collections::HashSet<String> = (0..model.n_items())
            .filter_map(|pos| model.info_at(pos))
            .map(|i| i.display_name().to_string())
            .filter(|n| !renamed.contains(n))
            .collect();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let max = match view.location() {
                    Some(dir) => crate::naming::name_max(&dir).await,
                    None => None,
                };
                let Some(new_names) =
                    crate::dialogs::batch_rename_dialog(&view, names, others, max).await
                else {
                    return;
                };
                let renames: Vec<(gio::File, String)> = files.into_iter().zip(new_names).collect();
                // The renamed files are picked out again where the new names put them,
                // which is rarely where the old ones were.
                view.submit_and_select(JobKind::Rename { renames });
            }
        ));
    }

    /// Bounds (in this widget's coordinates) of the visible cell showing model position `pos`.
    pub(super) fn cell_bounds(&self, pos: u32) -> Option<gtk::gdk::Rectangle> {
        fn find(w: &gtk::Widget, pos: u32, depth: u32) -> Option<gtk::Widget> {
            if depth > 6 {
                return None;
            }
            if crate::browser_view::cell_position(w) == Some(pos) {
                return Some(w.clone());
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                if let Some(found) = find(&c, pos, depth + 1) {
                    return Some(found);
                }
                child = c.next_sibling();
            }
            None
        }
        let cell = find(self.imp().stack.upcast_ref(), pos, 0)?;
        let bounds = cell.compute_bounds(self)?;
        Some(gtk::gdk::Rectangle::new(
            bounds.x() as i32,
            bounds.y() as i32,
            bounds.width() as i32,
            bounds.height() as i32,
        ))
    }
}
