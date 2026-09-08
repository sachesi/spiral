//! `view.*` actions, context menus, rename and new-folder flows for `BrowserView`.

use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::file_utils;
use crate::ops::{JobKind, JobManager, JobStatus};
use crate::{clipboard, gio, glib, gtk};

/// Position of the first name cell below `w`, a few levels deep at most.
fn first_cell_position(w: &gtk::Widget, depth: u32) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    if let Some(pos) = crate::browser_view::cell_position(w) {
        return Some(pos);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(pos) = first_cell_position(&c, depth + 1) {
            return Some(pos);
        }
        child = c.next_sibling();
    }
    None
}

/// Whether the keyboard is in a text entry, where a space is a space and not a shortcut.
fn is_editing(view: &BrowserView) -> bool {
    view.root()
        .and_downcast::<gtk::Window>()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
        .is_some_and(|w| w.is::<gtk::Editable>())
}

impl BrowserView {
    fn manager(&self) -> Option<JobManager> {
        self.root()
            .and_downcast::<gtk::Window>()
            .and_then(|w| w.application())
            .and_downcast::<SpiralApplication>()
            .map(|a| a.job_manager().clone())
    }

    fn selected(&self) -> Vec<gio::File> {
        self.model().selected_files()
    }

    pub(crate) fn setup_actions(&self) {
        let imp = self.imp();
        let group = &imp.actions;
        let chooser = self.chooser_mode();

        let add = |name: &str, f: fn(&BrowserView)| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _| f(&view)
            ));
            group.add_action(&action);
            action
        };

        add("open", |v| {
            for info in v.model().selected_infos() {
                let file = file_utils::file_of(&info);
                if file_utils::is_dir(&info) {
                    v.go_to(&file);
                    break;
                } else if !v.chooser_mode() {
                    v.launch(&file);
                }
            }
        });
        let open_new_tab = add("open-new-tab", |v| {
            for f in v.selected() {
                v.emit_by_name::<()>("open-in-new-tab", &[&f]);
            }
        });
        add("select-all", |v| {
            v.model().selection().select_all();
        });
        add("context-menu", |v| v.popup_menu_for_selection());
        add("drop-copy", |v| v.finish_drop(gtk::gdk::DragAction::COPY));
        add("drop-move", |v| v.finish_drop(gtk::gdk::DragAction::MOVE));
        add("drop-link", |v| v.finish_drop(gtk::gdk::DragAction::LINK));
        add("new-folder", |v| v.new_folder());
        add("properties", |v| v.show_properties(false));
        add("folder-properties", |v| v.show_properties(true));
        add("open-with", |v| v.open_with());
        add("preview", |v| v.show_preview());
        add("star", |v| v.set_selection_starred(true));
        add("unstar", |v| v.set_selection_starred(false));
        add("open-item-location", |v| v.open_item_location());
        add("bookmark", |v| {
            if let Some(dir) = v.location() {
                crate::bookmarks::add(&dir);
                v.update_action_state();
            }
        });

        let destructive = [
            add("cut", |v| v.copy_to_clipboard(true)),
            add("copy", |v| v.copy_to_clipboard(false)),
            add("paste", |v| v.paste(None)),
            add("paste-into", |v| {
                if let Some(dir) = v.selected().first() {
                    v.paste(Some(dir.clone()));
                }
            }),
            add("trash", |v| {
                v.submit_on_selection(|files| JobKind::Trash { files })
            }),
            add("delete", |v| v.delete_selected()),
            add("delete-permanently", |v| v.delete_selected()),
            add("delete-from-trash", |v| v.delete_selected()),
            add("restore", |v| v.restore_selected()),
            add("create-link", |v| {
                if let Some(dest) = v.location() {
                    v.submit_on_selection(|files| JobKind::Link {
                        files,
                        dest: dest.clone(),
                    });
                }
            }),
            add("paste-link", |v| v.paste_link()),
            add("copy-to", |v| v.transfer_to(false)),
            add("move-to", |v| v.transfer_to(true)),
            add("run", |v| v.run_selected()),
            add("rename", |v| v.rename_selected()),
            add("new-file", |v| {
                if let Some(parent) = v.location() {
                    v.submit(JobKind::CreateFile {
                        parent,
                        name: gettext("Untitled Document"),
                    });
                }
            }),
            add("empty-trash", |v| v.empty_trash()),
            add("extract", |v| {
                if let Some(dest) = v.location() {
                    v.submit_on_selection(|archives| JobKind::Extract {
                        archives,
                        dest: dest.clone(),
                    });
                }
            }),
            add("extract-to", |v| v.extract_to()),
            add("compress", |v| v.compress()),
            add("open-terminal", |v| v.open_terminal(false)),
            add("folder-terminal", |v| v.open_terminal(true)),
        ];
        if chooser {
            for a in &destructive {
                a.set_enabled(false);
            }
            open_new_tab.set_enabled(false);
        }
        self.insert_action_group("view", Some(group));

        // Enable/disable by selection and location.
        let update = glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || view.update_action_state()
        );
        self.model()
            .selection()
            .connect_selection_changed(glib::clone!(
                #[strong]
                update,
                move |_, _, _| update()
            ));
        self.connect_location_notify(glib::clone!(
            #[strong]
            update,
            move |view| {
                // Writability is looked up once per folder, off the main loop's back.
                view.imp().can_write.set(true);
                update();
                let Some(dir) = view.location() else { return };
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    view,
                    #[strong]
                    update,
                    async move {
                        let writable = dir
                            .query_info_future(
                                "access::can-write",
                                gio::FileQueryInfoFlags::NONE,
                                glib::Priority::DEFAULT,
                            )
                            .await
                            .map(|i| i.boolean("access::can-write"))
                            .unwrap_or(true);
                        if view.location().is_some_and(|l| l.equal(&dir)) {
                            view.imp().can_write.set(writable);
                            update();
                            // Cells bound before the answer arrived assumed a writable
                            // folder; rebind them so their lock emblems follow.
                            if !writable {
                                view.refresh_cells();
                            }
                        }
                    }
                ));
            }
        ));
        let clipboard_handler = self.clipboard().connect_changed(glib::clone!(
            #[strong]
            update,
            #[weak(rename_to = view)]
            self,
            move |cb| {
                update();
                // Files cut to the clipboard are dimmed, so follow every change.
                let cb = cb.clone();
                glib::spawn_future_local(async move {
                    if clipboard::refresh_cut(&cb).await {
                        view.refresh_cells();
                    }
                });
            }
        ));
        imp.clipboard_handler.replace(Some(clipboard_handler));
        for key in ["show-delete-permanently", "show-create-link"] {
            imp.settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[strong]
                    update,
                    move |_, _| update()
                ),
            );
        }
        self.update_action_state();

        // Right click: item menu or background menu.
        let click = gtk::GestureClick::builder().button(3).build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                // The menus act on the folder being viewed, which the columns beside it
                // are not; a click there picks that folder first.
                if !view.in_side_column(x, y) {
                    view.popup_menu_at(x, y);
                }
            }
        ));
        imp.stack.add_controller(click);

        // Middle click on a folder opens it in a new tab, as it does in the sidebar.
        let middle = gtk::GestureClick::builder().button(2).build();
        middle.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, _, x, y| {
                let stack = view.imp().stack.clone().upcast::<gtk::Widget>();
                let folder = crate::browser_view::cell_at(&stack, x, y)
                    .and_then(|cell| first_cell_position(&cell, 0))
                    .and_then(|pos| view.model().info_at(pos))
                    .filter(file_utils::is_dir);
                if let Some(info) = folder
                    && !view.in_side_column(x, y)
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    view.emit_by_name::<()>("open-in-new-tab", &[&file_utils::file_of(&info)]);
                }
            }
        ));
        imp.stack.add_controller(middle);

        // Space previews the selection, and the first arrow in a folder with nothing
        // selected picks its first item, where GTK would only give it the focus and leave
        // the press looking lost. Captured, because the window's search entry takes typing
        // before the view is asked; a text entry inside the view keeps its keys.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                use gtk::gdk::{Key, ModifierType as M};
                let held = M::CONTROL_MASK | M::ALT_MASK | M::SHIFT_MASK | M::SUPER_MASK;
                if state.intersects(held) || is_editing(&view) {
                    return glib::Propagation::Proceed;
                }
                match key {
                    Key::space => {
                        let _ = view.activate_action("view.preview", None);
                        glib::Propagation::Stop
                    }
                    Key::Up | Key::Down | Key::Left | Key::Right
                        if view.model().n_items() > 0
                            && view.model().selection().selection().is_empty() =>
                    {
                        view.reveal_position(
                            0,
                            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                        );
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.add_controller(keys);
    }

    /// The preview follows the selection while it is open, and its arrows move that
    /// selection, so a folder can be walked through without closing it.
    fn show_preview(&self) {
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
        let selection = self.model().selection();
        let id = selection.connect_selection_changed(glib::clone!(
            #[weak]
            dialog,
            #[weak(rename_to = view)]
            self,
            move |_, _, _| {
                if let Some(info) = view.model().selected_infos().first() {
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

    fn set_enabled(&self, name: &str, enabled: bool) {
        if let Some(a) = self
            .imp()
            .actions
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            a.set_enabled(enabled);
        }
    }

    fn update_action_state(&self) {
        if self.chooser_mode() {
            return;
        }
        let infos = self.model().selected_infos();
        let n = infos.len();
        let single_dir = n == 1 && file_utils::is_dir(&infos[0]);
        let in_trash = self
            .location()
            .is_some_and(|l| l.uri().starts_with("trash:"));
        // Virtual folders: nothing can be created or pasted there.
        let virtual_dir = in_trash
            || self
                .location()
                .is_some_and(|l| crate::starred::is_starred_location(&l));
        let can_write = !virtual_dir && self.imp().can_write.get();
        let all = |attr: &str| infos.iter().all(|i| file_utils::allows(i, attr));
        let (can_delete, can_trash, can_rename) = (
            all("access::can-delete"),
            all("access::can-trash"),
            all("access::can-rename"),
        );
        let dir_writable = single_dir && file_utils::allows(&infos[0], "access::can-write");
        self.set_enabled("open", n > 0);
        self.set_enabled("preview", n > 0);
        self.set_enabled(
            "open-new-tab",
            n > 0 && infos.iter().all(file_utils::is_dir),
        );
        self.set_enabled("open-with", n > 0 && !infos.iter().any(file_utils::is_dir));
        let has_terminal = crate::terminal::chosen().is_some();
        self.set_enabled(
            "open-terminal",
            self.terminal_dir().is_some() && has_terminal,
        );
        self.set_enabled(
            "folder-terminal",
            self.folder_dir().is_some() && has_terminal,
        );
        self.set_enabled("cut", n > 0 && !in_trash && can_delete);
        self.set_enabled("copy", n > 0);
        let cb = self.clipboard();
        let has_files = clipboard::has_files(&cb);
        // Images are pasted as a new file, so they count too.
        let has_clip = has_files || clipboard::has_image(&cb);
        self.set_enabled("paste", can_write && !in_trash && has_clip);
        self.set_enabled("paste-into", dir_writable && !in_trash && has_clip);
        self.set_enabled("rename", n == 1 && !in_trash && can_rename);
        self.set_enabled("trash", n > 0 && !in_trash && can_trash);
        self.set_enabled("delete", n > 0 && can_delete);
        let show_delete = self.imp().settings.boolean("show-delete-permanently");
        self.set_enabled(
            "delete-permanently",
            n > 0 && can_delete && !in_trash && show_delete,
        );
        self.set_enabled("delete-from-trash", n > 0 && can_delete && in_trash);
        self.set_enabled(
            "restore",
            in_trash && n > 0 && infos.iter().all(|i| i.has_attribute("trash::orig-path")),
        );
        let in_virtual = self.model().searching()
            || self
                .location()
                .is_some_and(|l| crate::starred::is_starred_location(&l));
        self.set_enabled(
            "open-item-location",
            n == 1 && in_virtual && file_utils::file_of(&infos[0]).parent().is_some(),
        );
        self.set_enabled("copy-to", n > 0 && !in_trash);
        self.set_enabled("move-to", n > 0 && !in_trash && can_delete);
        self.set_enabled("run", n == 1 && file_utils::is_program(&infos[0]));
        self.set_enabled("new-folder", can_write && !in_trash);
        self.set_enabled("new-file", can_write && !in_trash);
        self.set_enabled("empty-trash", in_trash && self.model().n_items() > 0);
        self.set_enabled("properties", n > 0 || self.location().is_some());
        self.set_enabled("folder-properties", self.location().is_some());
        let local = infos.iter().all(|i| file_utils::file_of(i).is_native());
        let local_dir = self.location().is_some_and(|l| l.is_native());
        self.set_enabled(
            "create-link",
            n > 0
                && local
                && local_dir
                && can_write
                && self.imp().settings.boolean("show-create-link"),
        );
        self.set_enabled(
            "paste-link",
            can_write && local_dir && !in_trash && has_files,
        );
        let archives = n > 0
            && local
            && infos.iter().all(|i| {
                i.content_type()
                    .is_some_and(|ct| crate::ops::archive::is_archive(&ct))
            });
        self.set_enabled("extract", archives && can_write);
        self.set_enabled("extract-to", archives);
        self.set_enabled("compress", n > 0 && local && can_write);
        let files = self.selected();
        let starred = files
            .iter()
            .filter(|f| crate::starred::is_starred(f))
            .count();
        self.set_enabled("star", !in_trash && starred < n);
        self.set_enabled("unstar", starred > 0);
        self.set_enabled(
            "bookmark",
            !virtual_dir
                && self
                    .location()
                    .is_some_and(|l| !crate::bookmarks::contains(&l)),
        );
    }

    /// The selected folder, or the current one when nothing is selected; local only.
    fn terminal_dir(&self) -> Option<std::path::PathBuf> {
        let infos = self.model().selected_infos();
        let dir = match infos.as_slice() {
            [] => self.location()?,
            [info] if file_utils::is_dir(info) => file_utils::file_of(info),
            _ => return None,
        };
        dir.path()
    }

    /// The folder on screen, local only.
    fn folder_dir(&self) -> Option<std::path::PathBuf> {
        self.location()?.path()
    }

    /// `folder` ignores the selection, for the menu opened over empty space.
    fn open_terminal(&self, folder: bool) {
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

    fn extract_to(&self) {
        let archives = self.selected();
        if archives.is_empty() {
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Extract To"))
            .accept_label(gettext("_Extract"))
            .initial_folder(
                &self
                    .location()
                    .unwrap_or_else(|| gio::File::for_path(glib::home_dir())),
            )
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let win = view.root().and_downcast::<gtk::Window>();
                if let Ok(dest) = dialog.select_folder_future(win.as_ref()).await {
                    view.submit(JobKind::Extract { archives, dest });
                }
            }
        ));
    }

    fn compress(&self) {
        let files = self.selected();
        let (Some(dest), Some(first)) = (self.location(), files.first()) else {
            return;
        };
        let default = match files.len() {
            1 => crate::ops::archive::stem(&crate::ops::name(first)).to_string(),
            _ => gettext("Archive"),
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some((file_name, password)) =
                    crate::dialogs::compress_dialog(&view, &default).await
                {
                    view.submit(JobKind::Compress {
                        files,
                        dest,
                        file_name,
                        password,
                    });
                }
            }
        ));
    }

    fn set_selection_starred(&self, starred: bool) {
        for f in self.selected() {
            crate::starred::set_starred(&f, starred);
        }
        self.update_action_state();
        self.refresh_cells();
    }

    fn submit(&self, kind: JobKind) {
        if let Some(m) = self.manager() {
            m.submit(kind);
        }
    }

    /// Submit and select what the job leaves in the folder, the way pasting should end:
    /// with the pasted files picked out, ready for the next thing done to them.
    fn submit_and_select(&self, kind: JobKind) {
        let Some(job) = self.manager().map(|m| m.submit(kind)) else {
            return;
        };
        job.connect_status_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |job| {
                if job.status() == JobStatus::Done {
                    view.select_files_when_loaded(job.landed());
                }
            }
        ));
    }

    pub fn submit_kind(&self, kind: JobKind) {
        self.submit(kind);
    }

    fn submit_on_selection(&self, make: impl Fn(Vec<gio::File>) -> JobKind) {
        let files = self.selected();
        if !files.is_empty() {
            self.submit(make(files));
        }
    }

    fn copy_to_clipboard(&self, cut: bool) {
        let files = self.selected();
        if !files.is_empty() {
            clipboard::set(&self.clipboard(), &files, cut);
        }
    }

    fn paste(&self, into: Option<gio::File>) {
        let Some(dest) = into.or_else(|| self.location()) else {
            return;
        };
        let cb = self.clipboard();
        if !clipboard::has_files(&cb) {
            // An image and no files: a screenshot, saved into the folder as a PNG.
            if clipboard::has_image(&cb) {
                self.paste_image(dest);
            }
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let Some((files, cut)) = clipboard::read(&cb).await else {
                    return;
                };
                let pairs = files.into_iter().map(|f| (f, dest.clone())).collect();
                view.submit_and_select(JobKind::Transfer {
                    pairs,
                    is_move: cut,
                });
                if cut {
                    cb.set_content(gtk::gdk::ContentProvider::NONE).ok();
                }
            }
        ));
    }

    fn paste_image(&self, dest: gio::File) {
        let cb = self.clipboard();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                match cb.read_texture_future().await {
                    Ok(Some(image)) => view.submit_and_select(JobKind::SaveImage {
                        parent: dest,
                        image,
                    }),
                    Ok(None) => {}
                    Err(e) => view.show_error(&gettext("Could Not Paste Image"), e.message()),
                }
            }
        ));
    }

    /// Choose a folder, then copy or move the selection there.
    fn transfer_to(&self, is_move: bool) {
        let files = self.selected();
        if files.is_empty() {
            return;
        }
        let dialog = gtk::FileDialog::builder()
            .title(if is_move {
                gettext("Move To")
            } else {
                gettext("Copy To")
            })
            .accept_label(if is_move {
                gettext("_Move")
            } else {
                gettext("_Copy")
            })
            .initial_folder(
                &self
                    .location()
                    .unwrap_or_else(|| gio::File::for_path(glib::home_dir())),
            )
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let win = view.root().and_downcast::<gtk::Window>();
                if let Ok(dest) = dialog.select_folder_future(win.as_ref()).await {
                    let pairs = files.into_iter().map(|f| (f, dest.clone())).collect();
                    view.submit(JobKind::Transfer { pairs, is_move });
                }
            }
        ));
    }

    fn paste_link(&self) {
        let Some(dest) = self.location() else { return };
        let cb = self.clipboard();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some((files, _)) = clipboard::read(&cb).await {
                    view.submit_and_select(JobKind::Link { files, dest });
                }
            }
        ));
    }

    /// Move trashed items back to where they came from.
    fn restore_selected(&self) {
        let pairs: Vec<(gio::File, gio::File)> = self
            .model()
            .selected_infos()
            .iter()
            .filter_map(|info| {
                let orig = info.attribute_byte_string("trash::orig-path")?;
                Some((
                    file_utils::file_of(info),
                    gio::File::for_path(orig.as_str()),
                ))
            })
            .collect();
        if !pairs.is_empty() {
            self.submit(JobKind::Restore { pairs });
        }
    }

    /// Go to the folder holding the selected item and select it there.
    fn open_item_location(&self) {
        let files = self.selected();
        let [file] = files.as_slice() else { return };
        let Some(parent) = file.parent() else { return };
        if self.location().is_some_and(|l| l.equal(&parent)) {
            // Already there: only the search has to end, and the header must follow.
            self.model().set_search_text("");
            if let Some(win) = self.root().and_downcast::<crate::window::SpiralWindow>() {
                win.sync_header();
            }
        } else {
            self.go_to(&parent);
        }
        self.select_files_when_loaded(vec![file.clone()]);
    }

    /// Scripts run in the terminal so their output can be seen; binaries start directly.
    fn run_selected(&self) {
        let infos = self.model().selected_infos();
        let [info] = infos.as_slice() else { return };
        let file = file_utils::file_of(info);
        let Some(path) = file.path() else { return };
        let is_script = info
            .content_type()
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

    fn delete_selected(&self) {
        let files = self.selected();
        if files.is_empty() {
            return;
        }
        let heading = match files.len() {
            1 => gettext("Permanently Delete “%s”?").replace("%s", &crate::ops::name(&files[0])),
            n => ngettext(
                "Permanently Delete %d Item?",
                "Permanently Delete %d Items?",
                n as u32,
            )
            .replace("%d", &n.to_string()),
        };
        let dialog = crate::adw::AlertDialog::builder()
            .heading(heading)
            .body(gettext("Permanently deleted items cannot be restored."))
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("delete", &gettext("_Delete")),
        ]);
        dialog.set_response_appearance("delete", crate::adw::ResponseAppearance::Destructive);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if dialog.choose_future(Some(&view)).await == "delete" {
                    view.submit(JobKind::Delete { files });
                }
            }
        ));
    }

    fn empty_trash(&self) {
        let dialog = crate::adw::AlertDialog::builder()
            .heading(gettext("Empty Trash?"))
            .body(gettext(
                "All items in the Trash will be permanently deleted.",
            ))
            .close_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("empty", &gettext("_Empty Trash")),
        ]);
        dialog.set_response_appearance("empty", crate::adw::ResponseAppearance::Destructive);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if dialog.choose_future(Some(&view)).await != "empty" {
                    return;
                }
                // Enumerate the trash itself so hidden and filtered-out items go too.
                let trash = gio::File::for_uri("trash:///");
                let files: Vec<gio::File> = crate::ops::children(&trash, "standard::name")
                    .await
                    .into_iter()
                    .map(|(f, _)| f)
                    .collect();
                view.submit(JobKind::Delete { files });
            }
        ));
    }

    fn new_folder(&self) {
        let Some(parent) = self.location() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some(name) = crate::naming::new_folder_dialog(&view, &parent).await {
                    view.submit(JobKind::CreateFolder { parent, name });
                }
            }
        ));
    }

    fn rename_selected(&self) {
        let infos = self.model().selected_infos();
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
                    view.submit(JobKind::Rename { file, new_name });
                }
            }
        ));
    }

    /// Bounds (in this widget's coordinates) of the visible cell showing model position `pos`.
    fn cell_bounds(&self, pos: u32) -> Option<gtk::gdk::Rectangle> {
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

    /// `folder` ignores the selection, for the menu opened over empty space.
    fn show_properties(&self, folder: bool) {
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
                move || view.model().reload()
            ),
        );
    }

    fn open_with(&self) {
        let infos = self.model().selected_infos();
        let Some(first) = infos.first() else {
            return;
        };
        let content_type = first.content_type().map(|s| s.to_string());
        let files: Vec<gio::File> = infos.iter().map(file_utils::file_of).collect();
        crate::dialogs::OpenWithDialog::new(&files, content_type).present(Some(self));
    }

    /// Model position of the item cell under (x, y) in `stack` coordinates.
    fn item_at(&self, x: f64, y: f64) -> Option<u32> {
        let stack = &self.imp().stack;
        let mut w = stack.pick(x, y, gtk::PickFlags::DEFAULT)?;
        loop {
            if let Some(pos) = crate::browser_view::cell_position(&w) {
                return Some(pos);
            }
            // Any cell of a list row counts: the name cell is the one that knows the position.
            if w.css_name() == "row"
                && let Some(pos) = first_cell_position(&w, 0)
            {
                return Some(pos);
            }
            if &w == stack.upcast_ref::<gtk::Widget>() {
                return None;
            }
            w = w.parent()?;
        }
    }

    fn popup_menu_at(&self, x: f64, y: f64) {
        let imp = self.imp();
        let selection = self.model().selection();
        let model: &gio::MenuModel = match self.item_at(x, y) {
            Some(pos) => {
                if !selection.is_selected(pos) {
                    selection.select_item(pos, true);
                }
                &imp.item_menu
            }
            None => &imp.background_menu,
        };
        self.update_action_state();
        let point = imp
            .stack
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32));
        let (px, py) = point
            .map(|p| (p.x() as f64, p.y() as f64))
            .unwrap_or((x, y));
        self.popup_model(model, px, py);
    }

    /// Show `model` as a popover menu at a point in the view's own coordinates.
    pub(crate) fn popup_model(&self, model: &gio::MenuModel, x: f64, y: f64) {
        let imp = self.imp();
        let existing = imp.popover.borrow().clone();
        let popover = match existing {
            Some(p) => p,
            None => {
                let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
                p.set_parent(self);
                p.set_has_arrow(false);
                p.set_halign(gtk::Align::Start);
                // Closing comes first and the entry activates straight after, so the files
                // have to outlive the close by an iteration; whatever is left then was a
                // menu dismissed without an answer.
                p.connect_closed(glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_| {
                        // The menu took the focus and hands it back to nothing in
                        // particular, which would leave the view keys dead.
                        view.grab_view_focus();
                        glib::idle_add_local_once(move || {
                            view.imp().pending_drop.replace(None);
                        });
                    }
                ));
                imp.popover.replace(Some(p.clone()));
                p
            }
        };
        popover.set_menu_model(Some(model));
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    fn popup_menu_for_selection(&self) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        self.popup_menu_at(w / 2.0, h / 2.0);
    }
}
