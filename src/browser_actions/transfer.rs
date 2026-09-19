//! Moving files about: the clipboard, copying and moving to a folder or the other pane,
//! links, the trash and deleting, and archives in and out.

use super::*;

impl BrowserView {
    /// Start `kind`. What it makes, moves or renames into the folder on screen is
    /// selected once it is done, whichever way it was asked for: a menu, a key, a drop.
    pub(super) fn submit(&self, kind: JobKind) {
        self.submit_and_select(kind);
    }

    /// Submit and select what the job leaves in the folder, the way pasting should end:
    /// with the pasted files picked out, ready for the next thing done to them. Only if
    /// the view is still in that folder when the job ends: one that has moved on to
    /// another keeps the selection it has there.
    pub(super) fn submit_and_select(&self, kind: JobKind) -> Option<crate::ops::Job> {
        let job = self.manager().map(|m| m.submit(kind))?;
        let location = self.location();
        let before = self.selection_snapshot();
        job.connect_status_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |job| {
                let here = match (view.location(), &location) {
                    (Some(now), Some(then)) => now.equal(then),
                    _ => false,
                };
                if job.status() == JobStatus::Done && here {
                    view.select_files_since(job.landed(), before.clone());
                }
            }
        ));
        Some(job)
    }

    pub fn submit_kind(&self, kind: JobKind) {
        self.submit(kind);
    }

    pub(super) fn submit_on_selection(&self, make: impl Fn(Vec<gio::File>) -> JobKind) {
        let files = self.selected();
        if !files.is_empty() {
            self.submit(make(files));
        }
    }

    pub(super) fn copy_to_clipboard(&self, cut: bool) {
        let files = self.selected();
        if !files.is_empty() {
            clipboard::set(&self.clipboard(), &files, cut);
        }
    }

    pub(super) fn paste(&self, into: Option<gio::File>) {
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
                let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
                let pairs = files.into_iter().map(|f| (f, dest.clone())).collect();
                let job = view.submit_and_select(JobKind::Transfer {
                    pairs,
                    is_move: cut,
                });
                // The cut is used up once the files have moved. Until then it stays, so a
                // move that is cancelled or fails can be pasted again; and it is only
                // cleared if the clipboard still holds it, not what was copied meanwhile.
                if let (true, Some(job)) = (cut, job) {
                    job.connect_status_notify(move |job| {
                        if job.status() != JobStatus::Done {
                            return;
                        }
                        let (cb, uris) = (cb.clone(), uris.clone());
                        glib::spawn_future_local(async move {
                            if let Some((files, true)) = clipboard::read(&cb).await
                                && files
                                    .iter()
                                    .map(|f| f.uri().to_string())
                                    .eq(uris.iter().cloned())
                            {
                                cb.set_content(gtk::gdk::ContentProvider::NONE).ok();
                            }
                        });
                    });
                }
            }
        ));
    }

    pub(super) fn paste_image(&self, dest: gio::File) {
        let cb = self.clipboard();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                match cb.read_texture_future().await {
                    Ok(Some(image)) => {
                        view.submit_and_select(JobKind::SaveImage {
                            parent: dest,
                            image,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => view.show_error(&gettext("Could Not Paste Image"), e.message()),
                }
            }
        ));
    }

    /// Choose a folder, then copy or move the selection there.
    pub(super) fn transfer_to(&self, is_move: bool) {
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

    /// The other pane of a split tab.
    pub(super) fn sibling(&self) -> Option<BrowserView> {
        let paned = self.parent().and_downcast::<gtk::Paned>()?;
        [paned.start_child(), paned.end_child()]
            .into_iter()
            .flatten()
            .find(|c| c != self.upcast_ref::<gtk::Widget>())
            .and_downcast()
    }

    /// Whether the other pane can put files here follows what this one shows.
    pub(super) fn update_other_pane(&self) {
        if let Some(other) = self.sibling() {
            other.update_action_state();
        }
    }

    /// The other pane of a split tab and its folder, when that folder takes files.
    pub(super) fn other_pane(&self) -> Option<(BrowserView, gio::File)> {
        let other = self.sibling()?;
        let dir = other.location()?;
        let virtual_dir = dir.uri().starts_with("trash:")
            || crate::starred::is_starred_location(&dir)
            || crate::tags::is_tag_location(&dir);
        let takes_files = other.imp().can_write.get() && other.model().error_message().is_none();
        (!virtual_dir && takes_files).then_some((other, dir))
    }

    /// Copy or move the selection into the folder the other pane shows, and select it
    /// there once it has landed.
    pub(super) fn transfer_to_other_pane(&self, is_move: bool) {
        let files = self.selected();
        let Some((other, dest)) = self.other_pane() else {
            return;
        };
        // Moving onto the folder the files already live in is a no-op.
        let home = |f: &gio::File| f.parent().is_some_and(|p| p.equal(&dest));
        if files.is_empty() || is_move && files.iter().all(home) {
            return;
        }
        let pairs = files.into_iter().map(|f| (f, dest.clone())).collect();
        other.submit_and_select(JobKind::Transfer { pairs, is_move });
    }

    pub(super) fn paste_link(&self) {
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

    /// A link to each selected file, in the folder being viewed.
    pub(super) fn link_selection(&self) {
        if let Some(dest) = self.location() {
            self.submit_on_selection(|files| JobKind::Link {
                files,
                dest: dest.clone(),
            });
        }
    }

    /// Move trashed items back to where they came from.
    pub(super) fn restore_selected(&self) {
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

    pub(super) fn delete_selected(&self) {
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

    pub(super) fn empty_trash(&self) {
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

    pub(super) fn extract_to(&self) {
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

    pub(super) fn compress(&self) {
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
}
