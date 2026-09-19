//! Opening the selection in other ways: the preview, properties, another application, a
//! terminal, running it, and the folder it is in.

use super::*;

impl BrowserView {
    /// The preview follows the selection while it is open, and its arrows move that
    /// selection, so a folder can be walked through without closing it.
    pub(super) fn show_preview(&self) {
        let infos = self.model().selected_infos();
        let Some(info) = infos.first() else { return };
        let dialog = crate::dialogs::PreviewDialog::new();
        // The preview is shaped to what it holds, within what the window can hold.
        if let Some(window) = self.root().and_downcast::<gtk::Window>() {
            dialog.set_bounds(window.width(), window.height());
        }
        dialog.connect_step(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |delta| view.step_selection(delta)
        ));
        dialog.connect_open(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                if let Some(file) = view.selected().first()
                    && let Some(pos) = view.model().position_of(file)
                {
                    view.activate_position(pos);
                }
            }
        ));
        // What the preview shows, as of when it was read: the selection is also put back
        // after a change to other files around it, and the same file, unchanged, is not
        // loaded again for that -- a film would start over.
        let stamp = |info: &gio::FileInfo| {
            (
                file_utils::file_of(info).uri(),
                info.modification_date_time().map(|d| d.to_unix_usec()),
                file_utils::size_of(info),
            )
        };
        let shown = std::rc::Rc::new(std::cell::RefCell::new(stamp(info)));
        let selection = self.model().selection();
        let id = selection.connect_selection_changed(glib::clone!(
            #[weak]
            dialog,
            #[weak(rename_to = view)]
            self,
            move |_, _, _| {
                if let Some(info) = view.model().selected_infos().first() {
                    if shown.replace(stamp(info)) == stamp(info) {
                        return;
                    }
                    let (dialog, info) = (dialog.clone(), info.clone());
                    glib::spawn_future_local(async move { dialog.show_info(&info).await });
                }
            }
        ));
        let id = std::cell::RefCell::new(Some(id));
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                if let Some(id) = id.borrow_mut().take() {
                    selection.disconnect(id);
                }
                // The dialog gives the keyboard back to the item it was taken from; the
                // arrows may have moved the selection since, and the keyboard follows it.
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    view,
                    move || {
                        let selected = view.model().selection().selection();
                        if !selected.is_empty() {
                            view.reveal_position(selected.nth(0), gtk::ListScrollFlags::FOCUS);
                        }
                    }
                ));
            }
        ));
        // The file is shaped before the dialog is shown, so it opens at the shape it keeps;
        // a PDF that will not say how large its pages are is asked as well.
        let info = info.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            // The dialog has no parent until it is presented, so the wait holds it.
            #[strong]
            dialog,
            async move {
                dialog.show_info(&info).await;
                dialog.shape_ahead(&info).await;
                dialog.present(Some(&view));
            }
        ));
    }

    /// Unmount the device the selection is the root of, or eject it where the drive takes
    /// the medium away; the sidebar does the work, since it is the same for its own rows.
    pub(super) fn unmount_selected(&self) {
        let files = self.selected();
        let ([file], Some(win)) = (
            files.as_slice(),
            self.root().and_downcast::<crate::window::SpiralWindow>(),
        ) else {
            return;
        };
        win.sidebar().eject_file(file);
    }

    /// The selected folder, or the current one when nothing is selected; local only.
    pub(super) fn terminal_dir(&self) -> Option<std::path::PathBuf> {
        let infos = self.model().selected_infos();
        let dir = match infos.as_slice() {
            [] => self.location()?,
            [info] if file_utils::is_dir(info) => file_utils::file_of(info),
            _ => return None,
        };
        local_path(self, &dir)
    }

    /// The folder on screen, local only.
    pub(super) fn folder_dir(&self) -> Option<std::path::PathBuf> {
        local_path(self, &self.location()?)
    }

    /// `folder` ignores the selection, for the menu opened over empty space.
    pub(super) fn open_terminal(&self, folder: bool) {
        let dir = if folder {
            self.folder_dir()
        } else {
            self.terminal_dir()
        };
        let Some(dir) = dir else {
            return;
        };
        if let Err(e) = crate::terminal::open(&dir)
            && let Some(win) = self.root().and_downcast::<crate::window::SpiralWindow>()
        {
            win.show_toast(e.message(), false);
        }
    }

    /// Go to the folder holding the selected item and select it there.
    pub(super) fn open_item_location(&self) {
        let files = self.selected();
        let [file] = files.as_slice() else { return };
        let Some(parent) = file.parent() else { return };
        // A step of its own in the history even when the search was of this very folder,
        // so Back returns to the search.
        self.go_to(&parent);
        self.select_files_when_loaded(vec![file.clone()]);
    }

    /// The folder holding the selected item in a tab of its own, behind this one, with the
    /// item selected there; the search or the list stays where it is.
    pub(super) fn open_item_location_in_new_tab(&self) {
        let files = self.selected();
        let [file] = files.as_slice() else { return };
        let Some(parent) = file.parent() else { return };
        let Some(win) = self.root().and_downcast::<crate::window::SpiralWindow>() else {
            return;
        };
        let page = win.add_tab(&parent, false);
        if let Some(view) = crate::window::SpiralWindow::views_of(&page).first() {
            view.select_files_when_loaded(vec![file.clone()]);
        }
    }

    /// Scripts run in the terminal so their output can be seen; binaries start directly.
    pub(super) fn run_selected(&self) {
        let infos = self.model().selected_infos();
        let [info] = infos.as_slice() else { return };
        let file = file_utils::file_of(info);
        let Some(path) = file.path() else { return };
        let is_script = file_utils::content_type_of(info)
            .is_some_and(|ct| gio::content_type_is_a(&ct, "text/plain"));
        let result = if is_script {
            let dir = path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            crate::terminal::run(&dir, &path)
        } else {
            gio::AppInfo::create_from_commandline(
                glib::shell_quote(&path).to_string_lossy().as_ref(),
                None,
                gio::AppInfoCreateFlags::NONE,
            )
            .and_then(|app| app.launch(&[], Some(&self.display().app_launch_context())))
        };
        if let Err(e) = result {
            self.show_error(&gettext("Could Not Open"), e.message());
        }
    }

    /// `folder` ignores the selection, for the menu opened over empty space.
    pub(super) fn show_properties(&self, folder: bool) {
        let files = match self.selected() {
            v if folder || v.is_empty() => self.location().into_iter().collect(),
            v => v,
        };
        if files.is_empty() {
            return;
        }
        crate::dialogs::PropertiesDialog::open(
            files,
            self,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || view.reload()
            ),
        );
    }

    pub(super) fn open_with(&self) {
        let infos = self.model().selected_infos();
        let Some(first) = infos.first() else {
            return;
        };
        let content_type = file_utils::content_type_of(first).map(|s| s.to_string());
        let files: Vec<gio::File> = infos.iter().map(file_utils::file_of).collect();
        crate::dialogs::OpenWithDialog::new(&files, content_type).present(Some(self));
    }

    /// Select every name in the folder matching a shell pattern. The pattern replaces the
    /// selection rather than adding to it.
    pub(super) fn select_pattern(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let Some(pattern) = crate::dialogs::select_pattern_dialog(&view).await else {
                    return;
                };
                let pattern = pattern.trim();
                if pattern.is_empty() {
                    return;
                }
                let model = view.model();
                let matched = gtk::Bitset::new_empty();
                for pos in 0..model.n_items() {
                    if let Some(info) = model.info_at(pos)
                        && file_utils::matches_pattern(&info.display_name(), pattern)
                    {
                        matched.add(pos);
                    }
                }
                let all = gtk::Bitset::new_range(0, model.n_items());
                model.selection().set_selection(&matched, &all);
                if let Some(first) = (!matched.is_empty()).then(|| matched.nth(0)) {
                    view.reveal_position(first, gtk::ListScrollFlags::FOCUS);
                }
            }
        ));
    }
}

/// Where `dir` is on this machine's filesystem, if anywhere. A file on another machine has
/// a path only where gvfs mounted it through FUSE, and asking before gvfs has reached the
/// folder waits for the mount on the main loop.
fn local_path(view: &BrowserView, dir: &gio::File) -> Option<std::path::PathBuf> {
    if !dir.is_native() && !view.imp().reached.get() {
        return None;
    }
    dir.path()
}
