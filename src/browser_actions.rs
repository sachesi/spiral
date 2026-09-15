//! The `view.*` actions of `BrowserView` and when each is enabled; what they do is in the
//! files beside this one.

use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::file_utils;
use crate::object_data::Key;
use crate::ops::{JobKind, JobManager, JobStatus};
use crate::{clipboard, gio, glib, gtk};

mod create;
mod menus;
mod open;
mod tagging;
mod transfer;

use menus::*;
use tagging::*;

static TAG: Key<String> = Key::new("tag");

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
            // A window each.
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
        add("open-item-location-new-tab", |v| {
            v.open_item_location_in_new_tab()
        });
        add("bookmark", |v| {
            if let Some(dir) = v.location() {
                crate::bookmarks::add(&dir);
                v.update_action_state();
            }
        });

        let destructive = [
            add("cut", |v| v.copy_to_clipboard(true)),
            add("copy", |v| v.copy_to_clipboard(false)),
            add("copy-network-address", |v| {
                if let [info] = v.model().selected_infos().as_slice()
                    && let Some(target) = file_utils::target_of(info)
                {
                    v.clipboard().set_text(&target.uri());
                }
            }),
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
            add("create-link", |v| v.link_selection()),
            add("link", |v| v.link_selection()),
            add("paste-link", |v| v.paste_link()),
            add("copy-to", |v| v.transfer_to(false)),
            add("move-to", |v| v.transfer_to(true)),
            add("copy-to-other-pane", |v| v.transfer_to_other_pane(false)),
            add("move-to-other-pane", |v| v.transfer_to_other_pane(true)),
            add("run", |v| v.run_selected()),
            add("rename", |v| v.rename_selected()),
            add("new-file", |v| v.new_document(None)),
            add("empty-trash", |v| v.empty_trash()),
            add("extract", |v| {
                let archives = v.selected();
                if let Some(dest) = v.location()
                    && !archives.is_empty()
                {
                    v.submit_and_select(JobKind::Extract { archives, dest });
                }
            }),
            add("extract-to", |v| v.extract_to()),
            add("compress", |v| v.compress()),
            add("new-folder-with-selection", |v| {
                v.new_folder_with_selection()
            }),
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
                // Writability is looked up once per folder, off the main loop's back, and
                // its answer also says that gvfs has reached the folder.
                view.imp().can_write.set(true);
                view.imp()
                    .reached
                    .set(view.location().is_none_or(|l| l.is_native()));
                update();
                view.update_other_pane();
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
                            view.imp().reached.set(true);
                            update();
                            view.update_other_pane();
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
        // A folder that fails to open takes no files, and the other pane stops offering it.
        self.model().connect_error_message_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_other_pane()
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
                    let generation = clipboard::refresh_cut(&cb).await;
                    if view.imp().cut_gen.replace(generation) != generation {
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
        // The preview holds one file, and its arrows would trade a wider selection for it.
        self.set_enabled("preview", n == 1);
        let all_folders = n > 0 && infos.iter().all(file_utils::is_dir);
        let shown = |key: &str| self.imp().settings.boolean(key);
        // Whether the menu offers them is up to `sync_open_menu`; the keys work either way.
        self.set_enabled("open-new-tab", all_folders);
        self.set_enabled("open-new-window", all_folders);
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
        // An entry that only stands for somewhere else, a server or a drive, has nothing to
        // copy; the address of a server does, and Ctrl+C copies that instead.
        let pointers = infos.iter().any(|i| file_utils::target_of(i).is_some());
        self.set_enabled("cut", n > 0 && !in_trash && can_delete && !pointers);
        self.set_enabled("copy", n > 0 && !pointers);
        let address = match infos.as_slice() {
            [info] => file_utils::target_of(info).is_some_and(|t| !t.is_native()),
            _ => false,
        };
        self.set_enabled("copy-network-address", address);
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
        let item_location =
            n == 1 && in_virtual && file_utils::file_of(&infos[0]).parent().is_some();
        self.set_enabled("open-item-location", item_location);
        self.set_enabled("open-item-location-new-tab", item_location);
        self.set_enabled("copy-to", n > 0 && !in_trash && shown("show-copy-to"));
        self.set_enabled(
            "move-to",
            n > 0 && !in_trash && can_delete && shown("show-move-to"),
        );
        // Only with a second pane, and one showing a folder that takes files.
        let other_dir = self.other_pane().map(|(_, dir)| dir);
        let copy_other = n > 0 && !in_trash && !pointers && other_dir.is_some();
        self.set_enabled("copy-to-other-pane", copy_other);
        let same_dir = match (&other_dir, self.location()) {
            (Some(dir), Some(here)) => dir.equal(&here),
            _ => false,
        };
        self.set_enabled("move-to-other-pane", copy_other && can_delete && !same_dir);
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
        let can_link = n > 0 && local && local_dir && can_write;
        self.set_enabled("link", can_link);
        self.set_enabled("create-link", can_link && shown("show-create-link"));
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
        self.set_enabled(
            "new-folder-with-selection",
            n > 1 && can_write && can_delete && !self.model().searching(),
        );
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
}
