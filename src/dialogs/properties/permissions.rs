//! The permissions page: owner, group and others, and changing everything inside a folder
//! at once.

use super::*;

/// Bits of one class (owner, group, others) for a file: None, Read-only, Read and write.
pub(super) const FILE_LEVELS: [u32; 3] = [0, 4, 6];

/// For a folder: None, List files only, Access files, Create and delete files.
pub(super) const DIR_LEVELS: [u32; 4] = [0, 4, 5, 7];

pub(super) fn file_level(bits: u32) -> u32 {
    match bits & 6 {
        0 => 0,
        4 => 1,
        _ => 2,
    }
}

pub(super) fn dir_level(bits: u32) -> u32 {
    match (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0) {
        (false, _, _) => 0,
        (true, false, false) => 1,
        (true, false, true) => 2,
        (true, true, _) => 3,
    }
}

pub(super) fn permissions_page(file: &gio::File, info: &gio::FileInfo) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let is_dir = file_utils::is_dir(info);
    let mode = Rc::new(Cell::new(info.attribute_uint32("unix::mode")));
    let owner = info.attribute_string("owner::user").unwrap_or_default();
    let group_name = info.attribute_string("owner::group").unwrap_or_default();
    let me = glib::user_name().to_string_lossy().into_owned();
    let editable = owner == me || me == "root";

    let group = adw::PreferencesGroup::new();
    if !editable {
        group.set_description(Some(&gettext(
            "You are not the owner, so you cannot change these permissions.",
        )));
    }
    let levels: Vec<String> = if is_dir {
        vec![
            gettext("None"),
            gettext("List Files Only"),
            gettext("Access Files"),
            gettext("Create and Delete Files"),
        ]
    } else {
        vec![
            gettext("None"),
            gettext("Read-Only"),
            gettext("Read and Write"),
        ]
    };
    let model = gtk::StringList::new(&levels.iter().map(String::as_str).collect::<Vec<_>>());

    let apply = {
        let file = file.clone();
        let mode = mode.clone();
        let page = page.clone();
        Rc::new(move |new_mode: u32| {
            if new_mode == mode.get() {
                return;
            }
            let info = gio::FileInfo::new();
            info.set_attribute_uint32("unix::mode", new_mode);
            glib::spawn_future_local(glib::clone!(
                #[strong]
                file,
                #[strong]
                mode,
                #[weak]
                page,
                async move {
                    match file
                        .set_attributes_future(
                            &info,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    {
                        Ok(_) => mode.set(new_mode),
                        Err(e) => {
                            let alert = adw::AlertDialog::builder()
                                .heading(gettext("Could Not Change Permissions"))
                                .body(e.message())
                                .build();
                            alert.add_response("ok", &gettext("_OK"));
                            alert.present(Some(&page));
                        }
                    }
                }
            ));
        })
    };

    let classes = [
        (gettext("Owner"), owner, 6),
        (gettext("Group"), group_name, 3),
        (gettext("Others"), glib::GString::new(), 0),
    ];
    // Set while the rows are being put back in step with the folder, which is not a choice
    // to apply.
    let syncing = Rc::new(Cell::new(false));
    let mut combos = Vec::new();
    for (title, subtitle, shift) in classes {
        let bits = (mode.get() >> shift) & 7;
        let combo = adw::ComboRow::builder()
            .title(&title)
            .subtitle(subtitle)
            .model(&model)
            .selected(if is_dir {
                dir_level(bits)
            } else {
                file_level(bits)
            })
            .sensitive(editable)
            .build();
        combo.connect_selected_notify(glib::clone!(
            #[strong]
            apply,
            #[strong]
            mode,
            #[strong]
            syncing,
            move |combo| {
                if syncing.get() {
                    return;
                }
                let i = combo.selected() as usize;
                let current = mode.get();
                let class = (current >> shift) & 7;
                let bits = if is_dir {
                    DIR_LEVELS[i]
                } else {
                    // Keep the execute bit as it is; the switch below owns it.
                    FILE_LEVELS[i] | (class & 1)
                };
                apply((current & !(7 << shift)) | (bits << shift));
            }
        ));
        group.add(&combo);
        combos.push((combo, shift));
    }

    if !is_dir {
        let exec = adw::SwitchRow::builder()
            .title(gettext("Executable"))
            .subtitle(gettext("Allow running as a program"))
            .active(mode.get() & 0o111 != 0)
            .sensitive(editable)
            .build();
        exec.connect_active_notify(glib::clone!(
            #[strong]
            apply,
            #[strong]
            mode,
            move |row| {
                let current = mode.get();
                let new_mode = if row.is_active() {
                    // Execute for every class that can read.
                    current | ((current & 0o444) >> 2)
                } else {
                    current & !0o111
                };
                apply(new_mode);
            }
        ));
        group.add(&exec);
    }
    page.add(&group);

    if is_dir && editable {
        let enclosed = adw::PreferencesGroup::new();
        let button = adw::ButtonRow::builder()
            .title(gettext("Change Permissions for Enclosed Files…"))
            .build();
        // The folder itself is among what changed; its rows follow once it is done.
        let resync = glib::clone!(
            #[strong]
            file,
            #[strong]
            mode,
            move || {
                glib::spawn_future_local(glib::clone!(
                    #[strong]
                    file,
                    #[strong]
                    mode,
                    #[strong]
                    combos,
                    #[strong]
                    syncing,
                    async move {
                        let Ok(info) = file
                            .query_info_future(
                                "unix::mode",
                                gio::FileQueryInfoFlags::NONE,
                                glib::Priority::DEFAULT,
                            )
                            .await
                        else {
                            return;
                        };
                        mode.set(info.attribute_uint32("unix::mode"));
                        syncing.set(true);
                        for (combo, shift) in &combos {
                            combo.set_selected(dir_level((mode.get() >> shift) & 7));
                        }
                        syncing.set(false);
                    }
                ));
            }
        );
        let resync = Rc::new(resync);
        button.connect_activated(glib::clone!(
            #[strong]
            file,
            move |button| enclosed_dialog(button, &file, resync.clone())
        ));
        enclosed.add(&button);
        page.add(&enclosed);
    }

    if let Some(context) = info.attribute_string("selinux::context") {
        let security = adw::PreferencesGroup::new();
        security.add(&row(&gettext("Security Context"), &context));
        page.add(&security);
    }
    page
}

/// Ask for the permissions of everything in `folder`, files and folders apart, each class
/// left unchanged unless a level is picked for it, and set them as an operation.
pub(super) fn enclosed_dialog(
    parent: &impl IsA<gtk::Widget>,
    folder: &gio::File,
    done: Rc<dyn Fn()>,
) {
    let unchanged = gettext("Unchanged");
    let file_levels = [
        unchanged.clone(),
        gettext("None"),
        gettext("Read-Only"),
        gettext("Read and Write"),
    ];
    let dir_levels = [
        unchanged,
        gettext("None"),
        gettext("List Files Only"),
        gettext("Access Files"),
        gettext("Create and Delete Files"),
    ];
    let classes = [gettext("Owner"), gettext("Group"), gettext("Others")];
    let combos = |title: String, levels: &[String]| {
        let group = adw::PreferencesGroup::builder().title(title).build();
        let model = gtk::StringList::new(&levels.iter().map(String::as_str).collect::<Vec<_>>());
        let rows: Vec<adw::ComboRow> = classes
            .iter()
            .map(|class| {
                let combo = adw::ComboRow::builder().title(class).model(&model).build();
                group.add(&combo);
                combo
            })
            .collect();
        (group, rows)
    };
    let (files_group, file_rows) = combos(gettext("Files"), &file_levels);
    let (folders_group, folder_rows) = combos(gettext("Folders"), &dir_levels);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    content.append(&files_group);
    content.append(&folders_group);
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Change Permissions for Enclosed Files"))
        .body(gettext("For “%s” and everything in it.").replace("%s", &crate::ops::name(folder)))
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", &gettext("_Cancel"));
    dialog.add_response("change", &gettext("_Change"));
    dialog.set_response_appearance("change", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("change"));
    let folder = folder.clone();
    dialog.connect_response(Some("change"), move |_, _| {
        // (value, mask) over the three classes, owner first.
        let bits = |rows: &[adw::ComboRow], levels: &[u32], mask: u32| {
            rows.iter()
                .zip([6, 3, 0])
                .filter(|(row, _)| row.selected() > 0)
                .fold((0, 0), |(value, all), (row, shift)| {
                    let level = levels[row.selected() as usize - 1];
                    (value | level << shift, all | mask << shift)
                })
        };
        // A file keeps its execute bits; the levels only speak of reading and writing.
        let files = bits(&file_rows, &FILE_LEVELS, 6);
        let folders = bits(&folder_rows, &DIR_LEVELS, 7);
        if files.1 == 0 && folders.1 == 0 {
            return;
        }
        if let Some(app) =
            gio::Application::default().and_downcast::<crate::application::SpiralApplication>()
        {
            let job = app
                .job_manager()
                .submit(crate::ops::JobKind::SetPermissions {
                    folder: folder.clone(),
                    files,
                    folders,
                });
            let done = done.clone();
            job.connect_status_notify(move |job| {
                if job.is_finished() {
                    done();
                }
            });
        }
    });
    dialog.present(Some(parent));
}
