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

/// A menu of the templates found, each starting a document from one of them, ending in
/// the empty document the menu has when there are no templates at all.
fn templates_menu(entries: &[crate::templates::Entry]) -> gio::Menu {
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

/// Where a selection stands with a tag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TagState {
    /// On every selected file.
    All,
    /// On some of them.
    Some,
    None,
}

/// A file name shown as a menu label: an underscore in it is a character, not a mnemonic.
fn mnemonic_safe(name: &str) -> String {
    name.replace('_', "__")
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
                } else if let Some(target) = file_utils::target_of(&info) {
                    v.open_target(target, file_utils::listed_as(&info));
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
        let open_new_window = add("open-new-window", |v| {
            let Some(app) = v
                .root()
                .and_downcast::<gtk::Window>()
                .and_then(|w| w.application())
                .and_downcast::<SpiralApplication>()
            else {
                return;
            };
            // A window each, the way GNOME Files opens them.
            for f in v.selected() {
                app.open_window(std::slice::from_ref(&f));
            }
        });
        add("select-all", |v| {
            v.model().selection().select_all();
        });
        add("invert-selection", |v| {
            let model = v.model();
            let all = gtk::Bitset::new_range(0, model.n_items());
            let inverted = all.copy();
            inverted.subtract(&model.selection().selection());
            model.selection().set_selection(&inverted, &all);
        });
        add("select-pattern", |v| v.select_pattern());
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
            add("new-file", |v| v.new_document(None)),
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
            add("unmount", |v| v.unmount_selected()),
            add("eject", |v| v.unmount_selected()),
            add("open-terminal", |v| v.open_terminal(false)),
            add("folder-terminal", |v| v.open_terminal(true)),
        ];
        // The only action here with a target: which template to start the document from.
        let from_template =
            gio::SimpleAction::new("new-from-template", Some(glib::VariantTy::STRING));
        from_template.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, param| {
                if let Some(uri) = param.and_then(glib::Variant::str) {
                    view.new_document(Some(gio::File::for_uri(uri)));
                }
            }
        ));
        group.add_action(&from_template);
        if chooser {
            for a in &destructive {
                a.set_enabled(false);
            }
            from_template.set_enabled(false);
            open_new_tab.set_enabled(false);
            open_new_window.set_enabled(false);
        }
        self.insert_action_group("view", Some(group));

        // Enable/disable by selection and location.
        let update = glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || view.update_action_state()
        );
        // The selection is not watched here: the view coalesces its own changes and calls
        // `update_action_state` from there, so a rubber band costs one update, not one
        // per motion event.
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
        for key in [
            "show-delete-permanently",
            "show-create-link",
            "show-open-new-tab",
            "show-open-new-window",
            "show-copy-to",
            "show-move-to",
        ] {
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

    pub(crate) fn update_action_state(&self) {
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
        let is_list = |l: &gio::File| {
            crate::starred::is_starred_location(l) || crate::tags::is_tag_location(l)
        };
        let virtual_dir = in_trash || self.location().is_some_and(|l| is_list(&l));
        let can_write = !virtual_dir && self.imp().can_write.get();
        let all = |attr: &str| infos.iter().all(|i| file_utils::allows(i, attr));
        let (can_delete, can_rename) = (all("access::can-delete"), all("access::can-rename"));
        let dir_writable = single_dir && file_utils::allows(&infos[0], "access::can-write");
        self.set_enabled("open", n > 0);
        self.set_enabled("preview", n > 0);
        let all_folders = n > 0 && infos.iter().all(file_utils::is_dir);
        let shown = |key: &str| self.imp().settings.boolean(key);
        self.set_enabled("open-new-tab", all_folders && shown("show-open-new-tab"));
        self.set_enabled(
            "open-new-window",
            all_folders && shown("show-open-new-window"),
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
        self.set_enabled("rename", n > 0 && !in_trash && can_rename);
        // Not `access::can-trash`: where there is no trash, the job offers to delete instead.
        self.set_enabled("trash", n > 0 && !in_trash && can_delete);
        self.set_enabled("delete", n > 0 && can_delete);
        self.set_enabled(
            "delete-permanently",
            n > 0 && can_delete && !in_trash && shown("show-delete-permanently"),
        );
        self.set_enabled("delete-from-trash", n > 0 && can_delete && in_trash);
        self.set_enabled(
            "restore",
            in_trash && n > 0 && infos.iter().all(|i| i.has_attribute("trash::orig-path")),
        );
        let in_virtual = self.model().searching() || self.location().is_some_and(|l| is_list(&l));
        self.set_enabled(
            "open-item-location",
            n == 1 && in_virtual && file_utils::file_of(&infos[0]).parent().is_some(),
        );
        self.set_enabled("copy-to", n > 0 && !in_trash && shown("show-copy-to"));
        self.set_enabled(
            "move-to",
            n > 0 && !in_trash && can_delete && shown("show-move-to"),
        );
        self.set_enabled("run", n == 1 && file_utils::is_program(&infos[0]));
        // A device listed in the folder can be sent away from here, as it can from the
        // sidebar; a drive that takes its medium back is ejected, the rest unmounted.
        let mount = (n == 1)
            .then(|| crate::places_sidebar::mount_of(&file_utils::file_of(&infos[0])))
            .flatten();
        self.set_enabled(
            "eject",
            mount
                .as_ref()
                .is_some_and(gio::prelude::MountExt::can_eject),
        );
        self.set_enabled(
            "unmount",
            mount
                .as_ref()
                .is_some_and(|m| m.can_unmount() && !m.can_eject()),
        );
        self.set_enabled("new-folder", can_write && !in_trash);
        self.set_enabled("new-file", can_write && !in_trash);
        self.set_enabled("new-from-template", can_write && !in_trash);
        self.set_enabled("empty-trash", in_trash && self.model().n_items() > 0);
        self.set_enabled("properties", n > 0 || self.location().is_some());
        self.set_enabled("folder-properties", self.location().is_some());
        let local = infos.iter().all(|i| file_utils::file_of(i).is_native());
        let local_dir = self.location().is_some_and(|l| l.is_native());
        self.set_enabled(
            "create-link",
            n > 0 && local && local_dir && can_write && shown("show-create-link"),
        );
        self.set_enabled(
            "paste-link",
            can_write && local_dir && !in_trash && has_files,
        );
        let archives = n > 0
            && local
            && infos.iter().all(|i| {
                file_utils::content_type_of(i)
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

    /// Unmount the device the selection is the root of, or eject it where the drive takes
    /// the medium away; the sidebar does the work, since it is the same for its own rows.
    fn unmount_selected(&self) {
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
        let start = self
            .location()
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let dest = crate::dialogs::folder_chooser_dialog(
                    &view,
                    &gettext("Extract To"),
                    &gettext("_Extract"),
                    &start,
                )
                .await;
                if let Some(dest) = dest {
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

    /// How the selection stands with a tag: on every file, on some, or on none.
    fn tag_state(&self, name: &str) -> TagState {
        let infos = self.model().selected_infos();
        let with = infos
            .iter()
            .filter(|i| crate::tags::of_info(i).iter().any(|t| t == name))
            .count();
        match with {
            0 => TagState::None,
            n if n == infos.len() => TagState::All,
            _ => TagState::Some,
        }
    }

    /// Whether the selection can be tagged: tags are on, and the files are local, since
    /// the extended attribute is the local filesystems' to keep, and not in the trash.
    fn can_tag(&self) -> bool {
        let infos = self.model().selected_infos();
        crate::tags::enabled()
            && !self.chooser_mode()
            && !infos.is_empty()
            && infos.iter().all(|i| {
                let file = file_utils::file_of(i);
                file.is_native() && !file.uri().starts_with("trash:")
            })
    }

    /// Put a tag on the whole selection or take it off the whole selection. The infos
    /// on screen are told as well: the folder monitor will say the same a moment later,
    /// but the dots should not wait for it.
    pub(crate) fn set_selection_tag(&self, name: &str, on: bool) {
        for info in self.model().selected_infos() {
            let file = file_utils::file_of(&info);
            if let Err(e) = crate::tags::set(&file, name, on) {
                // The reason is nearly always that the filesystem keeps no extended
                // attributes, which GIO says at length; the log gets its wording.
                glib::g_debug!("spiral", "cannot tag {}: {e}", file.uri());
                if let Some(win) = self.root().and_downcast::<crate::window::SpiralWindow>() {
                    win.show_toast(
                        &gettext("Could not tag “%s”").replace("%s", &info.display_name()),
                        false,
                    );
                }
                break;
            }
            let mut names = crate::tags::of_info(&info);
            names.retain(|t| t != name);
            if on {
                names.push(name.to_string());
            }
            match crate::tags::attribute_value(&names) {
                Some(v) => info.set_attribute_string(crate::tags::ATTRIBUTE, &v),
                None => info.remove_attribute(crate::tags::ATTRIBUTE),
            }
        }
        self.refresh_cells();
    }

    /// The tags section of the item menu: the coloured tags as a row of dots to click,
    /// while tags are on and the selection can take one; nothing otherwise. It is only
    /// touched when that changes: the popover loses the slot the dots go in when the
    /// item is taken out and put back, and would not take them again.
    fn sync_tags_menu(&self) {
        let section = &self.imp().tags_section;
        let picker = self.can_tag() && crate::tags::all().iter().any(|t| !t.color.is_empty());
        if section.n_items() == i32::from(picker) {
            return;
        }
        while section.n_items() > 0 {
            section.remove(0);
        }
        if picker {
            let item = gio::MenuItem::new(None, None);
            item.set_attribute_value("custom", Some(&"tags".to_variant()));
            section.append_item(&item);
        }
    }

    /// The row of dots for the menu on show: one per coloured tag, marked where the
    /// selection carries it. Built once and kept; the popover lets go of it whenever it
    /// builds a menu again, and it goes back in the slot the section leaves for it.
    fn attach_tag_picker(&self) {
        let imp = self.imp();
        if !self.can_tag() {
            return;
        }
        let existing = imp.tag_picker.borrow().clone();
        let picker = existing.unwrap_or_else(|| {
            let bx = gtk::Box::builder()
                .spacing(2)
                .css_classes(["spiral-tag-picker"])
                .build();
            imp.tag_picker.replace(Some(bx.clone()));
            bx
        });
        while let Some(child) = picker.first_child() {
            picker.remove(&child);
        }
        for tag in crate::tags::all().iter().filter(|t| !t.color.is_empty()) {
            let dot = crate::browser_view::tag_dot(&tag.color);
            let button = gtk::Button::builder()
                .child(&dot)
                .tooltip_text(&tag.name)
                .css_classes(["flat", "circular"])
                .build();
            unsafe { button.set_data("tag", tag.name.clone()) };
            let name = tag.name.clone();
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| {
                    // The menu goes first: the rows are rewritten under it otherwise.
                    if let Some(p) = view.imp().popover.borrow().as_ref() {
                        p.popdown();
                    }
                    let on = !matches!(view.tag_state(&name), TagState::All);
                    view.set_selection_tag(&name, on);
                }
            ));
            picker.append(&button);
        }
        self.sync_tag_picker();
        if picker.parent().is_none()
            && let Some(popover) = imp.popover.borrow().as_ref()
        {
            popover.add_child(&picker, "tags");
        }
    }

    /// Mark each dot of the picker as the selection stands with its tag.
    fn sync_tag_picker(&self) {
        let Some(picker) = self.imp().tag_picker.borrow().clone() else {
            return;
        };
        let mut child = picker.first_child();
        while let Some(button) = child {
            child = button.next_sibling();
            let (Some(name), Some(dot)) = (
                unsafe { button.data::<String>("tag").map(|p| p.as_ref().clone()) },
                button.first_child(),
            ) else {
                continue;
            };
            let state = self.tag_state(&name);
            dot.set_css_classes(&[
                "spiral-tag-dot",
                &crate::tags::dot_class(&crate::tags::color_of(&name).unwrap_or_default()),
            ]);
            if state == TagState::Some {
                dot.add_css_class("spiral-tag-some");
            }
            let image = dot.downcast_ref::<gtk::Image>().unwrap();
            image.set_icon_name((state != TagState::None).then_some("object-select-symbolic"));
        }
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
        let (title, accept) = if is_move {
            (gettext("Move To"), gettext("_Move"))
        } else {
            (gettext("Copy To"), gettext("_Copy"))
        };
        let start = self
            .location()
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let dest =
                    crate::dialogs::folder_chooser_dialog(&view, &title, &accept, &start).await;
                if let Some(dest) = dest {
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
        // A step of its own in the history even when the search was of this very folder,
        // so Back returns to the search.
        self.go_to(&parent);
        self.select_files_when_loaded(vec![file.clone()]);
    }

    /// Scripts run in the terminal so their output can be seen; binaries start directly.
    fn run_selected(&self) {
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
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Some(job) = crate::ops::empty_trash_job(&view).await {
                    view.submit(job);
                }
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
                    // Selected and given the keyboard once it appears: a folder is made to
                    // be used, and what follows -- opening it, renaming it, dragging into
                    // it -- starts from there.
                    view.submit_and_select(JobKind::CreateFolder { parent, name });
                }
            }
        ));
    }

    /// A document is named before it is made, as a folder is. The name starts as the
    /// template's own, or as the one an empty document is given, and what lands in the
    /// folder is selected.
    fn new_document(&self, template: Option<gio::File>) {
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

    /// The ways of opening the selection other than the plain one: a tab, a window, a
    /// terminal. Only the ones on offer are there, each an item of its own rather than a
    /// submenu, which a menu cannot be dismissed out of once it has been opened.
    fn sync_open_menu(&self) {
        let imp = self.imp();
        let section = &imp.open_section;
        if imp.open_items.get() == 0 {
            imp.open_items.set(section.n_items() as u32);
        }
        // Ours sit after the two the template starts with; what the template puts there
        // is counted once, so the entries below them can be moved without touching this.
        let ours = 2;
        while section.n_items() as u32 > imp.open_items.get() {
            section.remove(ours);
        }
        let ways = [
            (gettext("Open in New _Tab"), "view.open-new-tab"),
            (gettext("Open in New _Window"), "view.open-new-window"),
            (gettext("Open in _Terminal"), "view.open-terminal"),
        ];
        let mut at = ours;
        for (label, action) in ways {
            let name = action.trim_start_matches("view.");
            if imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
                .is_some_and(|a| a.is_enabled())
            {
                section.insert(at, Some(&label), Some(action));
                at += 1;
            }
        }
    }

    /// The document entry of the background menu: an item on its own where the templates
    /// folder is empty or missing, and what is in that folder where it is not.
    fn sync_new_menu(&self) {
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

    fn rename_selected(&self) {
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
    fn rename_many(&self) {
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
                let Some(new_names) =
                    crate::dialogs::batch_rename_dialog(&view, names, others).await
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
                move || view.reload()
            ),
        );
    }

    fn open_with(&self) {
        let infos = self.model().selected_infos();
        let Some(first) = infos.first() else {
            return;
        };
        let content_type = file_utils::content_type_of(first).map(|s| s.to_string());
        let files: Vec<gio::File> = infos.iter().map(file_utils::file_of).collect();
        crate::dialogs::OpenWithDialog::new(&files, content_type).present(Some(self));
    }

    /// Model position of the item cell under (x, y) in `stack` coordinates.
    pub(crate) fn item_at(&self, x: f64, y: f64) -> Option<u32> {
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
        let on_item = match self.item_at(x, y) {
            Some(pos) => {
                if !selection.is_selected(pos) {
                    selection.select_item(pos, true);
                }
                true
            }
            None => false,
        };
        // The menus that are built rather than laid out follow what the actions say, so
        // they are made once the selection has been taken in.
        self.update_action_state();
        let model: &gio::MenuModel = if on_item {
            self.sync_open_menu();
            self.sync_tags_menu();
            &imp.item_menu
        } else {
            self.sync_new_menu();
            &imp.background_menu
        };
        let point = imp
            .stack
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32));
        let (px, py) = point
            .map(|p| (p.x() as f64, p.y() as f64))
            .unwrap_or((x, y));
        self.popup_model(model, px, py);
        if on_item {
            self.attach_tag_picker();
        }
    }

    /// Show `model` as a popover menu at a point in the view's own coordinates.
    pub(crate) fn popup_model(&self, model: &gio::MenuModel, x: f64, y: f64) {
        let imp = self.imp();
        let existing = imp.popover.borrow().clone();
        let popover = match existing {
            Some(p) => p,
            None => {
                let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
                // Submenus open beside their item instead of sliding into the menu: a
                // sliding one resizes the popover, which then has to move to stay on
                // screen, and the whole menu jumps out from under the pointer.
                p.set_flags(gtk::PopoverMenuFlags::NESTED);
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

    /// Select every name in the folder matching a shell pattern, the way Nautilus does:
    /// the pattern replaces the selection rather than adding to it.
    fn select_pattern(&self) {
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

    fn popup_menu_for_selection(&self) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        self.popup_menu_at(w / 2.0, h / 2.0);
    }
}
