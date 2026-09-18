//! The general page: the file's name, type, size, place and times, its icon, and the folders
//! it can reveal.

use super::*;

pub(super) fn size_text(total: u64, count: u64) -> String {
    if count <= 1 {
        prefs::size(total)
    } else {
        ngettext("%s (%d item)", "%s (%d items)", count as u32)
            .replace("%s", &prefs::size(total))
            .replace("%d", &count.to_string())
    }
}

/// Recursive size, updating the row as it goes.
pub(super) async fn du(
    file: &gio::File,
    total: &mut u64,
    count: &mut u64,
    row: &adw::ActionRow,
    cancel: &gio::Cancellable,
) {
    let Ok(info) = file
        .query_info_future(
            "standard::type,standard::size",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::LOW,
        )
        .await
    else {
        return;
    };
    *count += 1;
    if info.file_type() != gio::FileType::Directory {
        *total += file_utils::size_of(&info);
        return;
    }
    let Ok(en) = file
        .enumerate_children_future(
            "standard::type,standard::size,standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::LOW,
        )
        .await
    else {
        return;
    };
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let Ok(infos) = en.next_files_future(128, glib::Priority::LOW).await else {
            break;
        };
        if infos.is_empty() {
            break;
        }
        for i in infos {
            if i.file_type() == gio::FileType::Directory {
                Box::pin(du(&en.child(&i), total, count, row, cancel)).await;
            } else {
                *count += 1;
                *total += file_utils::size_of(&i);
            }
        }
        row.set_subtitle(&size_text(*total, *count));
    }
}

pub(super) fn full_date(dt: Option<glib::DateTime>) -> String {
    dt.and_then(|d| d.to_local().ok())
        .and_then(|d| d.format("%c").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| gettext("Unknown"))
}

impl PropertiesDialog {
    /// A button at the end of `row` that shows `folder`, with `select` selected in it, and
    /// closes the dialog; nothing where there is no window to show it in.
    pub(super) fn add_reveal(
        &self,
        row: &adw::ActionRow,
        tooltip: &str,
        reveal: &Option<Reveal>,
        folder: gio::File,
        select: Vec<gio::File>,
    ) {
        let Some(reveal) = reveal.clone() else { return };
        let button = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text(tooltip)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                reveal(&folder, select.clone());
                dialog.close();
            }
        ));
        row.add_suffix(&button);
    }

    pub(super) fn general_page(
        &self,
        infos: &[(gio::File, gio::FileInfo)],
        reveal: Option<Reveal>,
    ) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::new();

        // Header: icon + name. A folder's icon is a button that picks a custom image.
        let header = adw::PreferencesGroup::new();
        let icon = gtk::Image::builder().pixel_size(96).build();
        let title = gtk::Label::builder()
            .css_classes(["title-2"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let head_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        if let [(file, info)] = infos {
            file_utils::set_icon(&icon, info);
            title.set_text(&info.display_name());
            if file_utils::is_dir(info) {
                let button = gtk::Button::builder()
                    .child(&icon)
                    .halign(gtk::Align::Center)
                    .tooltip_text(gettext("Change Icon…"))
                    .css_classes(["flat"])
                    .build();
                let reset = gtk::Button::builder()
                    .label(gettext("Reset Icon"))
                    .halign(gtk::Align::Center)
                    .visible(info.has_attribute("metadata::custom-icon"))
                    .css_classes(["flat"])
                    .build();
                button.connect_clicked(glib::clone!(
                    #[weak(rename_to = dialog)]
                    self,
                    #[strong]
                    file,
                    #[weak]
                    icon,
                    #[weak]
                    reset,
                    move |_| {
                        glib::spawn_future_local(glib::clone!(
                            #[strong]
                            file,
                            async move { dialog.pick_icon(&file, &icon, &reset).await }
                        ));
                    }
                ));
                reset.connect_clicked(glib::clone!(
                    #[weak(rename_to = dialog)]
                    self,
                    #[strong]
                    file,
                    #[weak]
                    icon,
                    move |reset| {
                        let info = gio::FileInfo::new();
                        // Setting the attribute with an invalid type removes it.
                        unsafe {
                            gio::ffi::g_file_info_set_attribute(
                                info.as_ptr(),
                                c"metadata::custom-icon".as_ptr(),
                                gio::ffi::G_FILE_ATTRIBUTE_TYPE_INVALID,
                                std::ptr::null_mut(),
                            );
                        }
                        glib::spawn_future_local(glib::clone!(
                            #[strong]
                            file,
                            #[weak]
                            dialog,
                            #[weak]
                            icon,
                            #[weak]
                            reset,
                            async move {
                                match file
                                    .set_attributes_future(
                                        &info,
                                        gio::FileQueryInfoFlags::NONE,
                                        glib::Priority::DEFAULT,
                                    )
                                    .await
                                {
                                    Ok(_) => {
                                        icon.set_icon_name(Some("folder"));
                                        reset.set_visible(false);
                                        dialog.emit_changed();
                                    }
                                    Err(e) => {
                                        dialog.show_error(&gettext("Could Not Reset Icon"), &e)
                                    }
                                }
                            }
                        ));
                    }
                ));
                head_box.append(&button);
                head_box.append(&title);
                head_box.append(&reset);
            } else {
                head_box.append(&icon);
                head_box.append(&title);
            }
        } else {
            icon.set_icon_name(Some("folder-documents-symbolic"));
            title.set_text(
                &ngettext("%d Item", "%d Items", infos.len() as u32)
                    .replace("%d", &infos.len().to_string()),
            );
            head_box.append(&icon);
            head_box.append(&title);
        }
        header.add(&head_box);
        page.add(&header);

        let group = adw::PreferencesGroup::new();
        if let [(_, info)] = infos {
            group.add(&row(&gettext("Type"), &file_utils::type_string(info)));
            if let Some(ct) = file_utils::content_type_of(info)
                && !file_utils::is_dir(info)
            {
                group.add(&row(&gettext("MIME Type"), &ct));
            }
        }

        let size_row = row(&gettext("Size"), &gettext("Calculating…"));
        group.add(&size_row);

        if let [(file, info)] = infos {
            if let Some(parent) = file.parent() {
                let location = row(&gettext("Location"), &location_text(&parent));
                self.add_reveal(
                    &location,
                    &gettext("Open Parent Folder"),
                    &reveal,
                    parent.clone(),
                    vec![file.clone()],
                );
                group.add(&location);
                if info.is_symlink()
                    && let Some(target) = info.symlink_target()
                {
                    let link = row(&gettext("Link Target"), &target.to_string_lossy());
                    let target = parent.resolve_relative_path(&target);
                    if let Some(folder) = target.parent() {
                        self.add_reveal(
                            &link,
                            &gettext("Open Link Target Location"),
                            &reveal,
                            folder,
                            vec![target],
                        );
                    }
                    group.add(&link);
                }
            }
            // An item in the trash: where it was, and since when it has been in the trash.
            if let Some(orig) = info.attribute_byte_string("trash::orig-path")
                && let Some(folder) = gio::File::for_path(orig.as_str()).parent()
            {
                let original = row(&gettext("Original Folder"), &location_text(&folder));
                self.add_reveal(
                    &original,
                    &gettext("Open Original Folder"),
                    &reveal,
                    folder,
                    Vec::new(),
                );
                group.add(&original);
                group.add(&row(
                    &gettext("Trashed On"),
                    &full_date(file_utils::trashed_on(info)),
                ));
            }
            group.add(&row(
                &gettext("Modified"),
                &full_date(info.modification_date_time()),
            ));
            group.add(&row(
                &gettext("Accessed"),
                &full_date(info.access_date_time()),
            ));
            if let Some(created) = info.creation_date_time() {
                group.add(&row(&gettext("Created"), &full_date(Some(created))));
            }
            if !file_utils::is_dir(info)
                && let Some(ct) = file_utils::content_type_of(info)
                && let Some(app) = gio::AppInfo::default_for_type(&ct, false)
            {
                group.add(&row(&gettext("Default Application"), &app.display_name()));
            }
        } else if let Some(parent) = infos[0].0.parent() {
            group.add(&row(&gettext("Location"), &location_text(&parent)));
        }
        page.add(&group);

        // Size is summed in the background; the row updates as folders are walked.
        let files: Vec<gio::File> = infos.iter().map(|(f, _)| f.clone()).collect();
        let cancellable = self.imp().cancellable.clone();
        glib::spawn_future_local(async move {
            let mut total = 0u64;
            let mut count = 0u64;
            for f in &files {
                if cancellable.is_cancelled() {
                    return;
                }
                du(f, &mut total, &mut count, &size_row, &cancellable).await;
            }
            size_row.set_subtitle(&size_text(total, count));
        });
        page
    }

    pub(super) async fn pick_icon(&self, file: &gio::File, icon: &gtk::Image, reset: &gtk::Button) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&gettext("Images")));
        filter.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title(gettext("Select Custom Icon"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let parent = self.root().and_downcast::<gtk::Window>();
        let Ok(image) = chooser.open_future(parent.as_ref()).await else {
            return;
        };
        let info = gio::FileInfo::new();
        info.set_attribute_string("metadata::custom-icon", &image.uri());
        match file
            .set_attributes_future(
                &info,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
        {
            Ok(_) => {
                file_utils::show_custom_icon(icon, image);
                reset.set_visible(true);
                self.emit_changed();
            }
            Err(e) => self.show_error(&gettext("Could Not Change Icon"), &e),
        }
    }
}
