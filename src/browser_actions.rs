//! `view.*` actions, context menus, rename and new-folder flows for `BrowserView`.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::application::SpiralApplication;
use crate::browser_view::BrowserView;
use crate::file_utils;
use crate::ops::{JobKind, JobManager};
use crate::{clipboard, gio, glib, gtk};

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
        add("new-folder", |v| v.new_folder());
        add("properties", |v| v.show_properties());
        add("open-with", |v| v.open_with());
        add("star", |v| v.set_selection_starred(true));
        add("unstar", |v| v.set_selection_starred(false));
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
            add("open-terminal", |v| v.open_terminal()),
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
            move |_| update()
        ));
        self.clipboard().connect_changed(move |_| update());
        self.update_action_state();

        // Right click: item menu or background menu.
        let click = gtk::GestureClick::builder().button(3).build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                view.popup_menu_at(x, y);
            }
        ));
        imp.stack.add_controller(click);
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
        let can_write = !virtual_dir
            && self.location().is_some_and(|l| {
                l.query_info(
                    "access::can-write",
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .map(|i| i.boolean("access::can-write"))
                .unwrap_or(true)
            });
        self.set_enabled("open", n > 0);
        self.set_enabled(
            "open-new-tab",
            n > 0 && infos.iter().all(file_utils::is_dir),
        );
        self.set_enabled("open-with", n > 0 && !infos.iter().any(file_utils::is_dir));
        self.set_enabled(
            "open-terminal",
            self.terminal_dir().is_some() && crate::terminal::chosen().is_some(),
        );
        self.set_enabled("cut", n > 0 && !in_trash);
        self.set_enabled("copy", n > 0);
        let has_clip = clipboard::has_files(&self.clipboard());
        self.set_enabled("paste", can_write && !in_trash && has_clip);
        self.set_enabled("paste-into", single_dir && !in_trash && has_clip);
        self.set_enabled("rename", n == 1 && !in_trash);
        self.set_enabled("trash", n > 0 && !in_trash);
        self.set_enabled("delete", n > 0);
        self.set_enabled("new-folder", can_write && !in_trash);
        self.set_enabled("new-file", can_write && !in_trash);
        self.set_enabled("empty-trash", in_trash && self.model().n_items() > 0);
        self.set_enabled("properties", n > 0 || self.location().is_some());
        let local = infos.iter().all(|i| file_utils::file_of(i).is_native());
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

    fn open_terminal(&self) {
        let Some(dir) = self.terminal_dir() else {
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
                if let Some(file_name) = crate::dialogs::compress_dialog(&view, &default).await {
                    view.submit(JobKind::Compress {
                        files,
                        dest,
                        file_name,
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
    }

    fn submit(&self, kind: JobKind) {
        if let Some(m) = self.manager() {
            m.submit(kind);
        }
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
                view.submit(JobKind::Transfer {
                    pairs,
                    is_move: cut,
                });
                if cut {
                    cb.set_content(gtk::gdk::ContentProvider::NONE).ok();
                }
            }
        ));
    }

    fn delete_selected(&self) {
        let files = self.selected();
        if files.is_empty() {
            return;
        }
        let heading = match files.len() {
            1 => gettext("Permanently Delete “%s”?").replace("%s", &crate::ops::name(&files[0])),
            n => gettext("Permanently Delete %d Items?").replace("%d", &n.to_string()),
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

    fn show_properties(&self) {
        let files = match self.selected() {
            v if v.is_empty() => self.location().into_iter().collect(),
            v => v,
        };
        if files.is_empty() {
            return;
        }
        let dialog = crate::dialogs::PropertiesDialog::new(&files);
        dialog.connect_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || view.model().reload()
        ));
        dialog.present(Some(self));
    }

    fn open_with(&self) {
        let files = self.selected();
        if files.is_empty() {
            return;
        }
        crate::dialogs::OpenWithDialog::new(&files).present(Some(self));
    }

    /// Model position of the item cell under (x, y) in `stack` coordinates.
    fn item_at(&self, x: f64, y: f64) -> Option<u32> {
        let stack = &self.imp().stack;
        let mut w = stack.pick(x, y, gtk::PickFlags::DEFAULT)?;
        loop {
            if let Some(pos) = crate::browser_view::cell_position(&w) {
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
            None => {
                selection.unselect_all();
                &imp.background_menu
            }
        };
        self.update_action_state();
        let existing = imp.popover.borrow().clone();
        let popover = match existing {
            Some(p) => p,
            None => {
                let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
                p.set_parent(self);
                p.set_has_arrow(false);
                p.set_halign(gtk::Align::Start);
                imp.popover.replace(Some(p.clone()));
                p
            }
        };
        popover.set_menu_model(Some(model));
        let p = imp
            .stack
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32));
        let (px, py) = p
            .map(|p| (p.x() as i32, p.y() as i32))
            .unwrap_or((x as i32, y as i32));
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(px, py, 1, 1)));
        popover.popup();
    }

    fn popup_menu_for_selection(&self) {
        let (w, h) = (self.width() as f64, self.height() as f64);
        self.popup_menu_at(w / 2.0, h / 2.0);
    }
}
