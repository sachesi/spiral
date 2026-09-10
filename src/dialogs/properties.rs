//! Properties dialog for one or more files: general page, and for a single local file a
//! permissions page that edits `unix::mode` in place.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gettextrs::{gettext, ngettext};

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::{adw, file_utils, gio, glib, gtk, prefs};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PropertiesDialog {
        pub cancellable: gio::Cancellable,
        pub changed: RefCell<Option<Box<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PropertiesDialog {
        const NAME: &'static str = "SpiralPropertiesDialog";
        type Type = super::PropertiesDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for PropertiesDialog {}
    impl WidgetImpl for PropertiesDialog {}
    impl AdwDialogImpl for PropertiesDialog {
        fn closed(&self) {
            self.cancellable.cancel();
            self.parent_closed();
        }
    }
}

glib::wrapper! {
    pub struct PropertiesDialog(ObjectSubclass<imp::PropertiesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

const ATTRS: &str = "standard::*,time::modified,time::access,time::created,owner::user,\
owner::group,unix::mode,unix::uid,access::*,selinux::context,metadata::custom-icon,\
metadata::custom-icon-name,trash::orig-path,trash::deletion-date";

/// What the disk page is told by the filesystem.
const FS_ATTRS: &str = "filesystem::size,filesystem::free,filesystem::used,filesystem::type";

/// Show a folder in the window the dialog came from, with some of what is in it selected.
type Reveal = Rc<dyn Fn(&gio::File, Vec<gio::File>)>;

impl PropertiesDialog {
    /// Query the files in the background, then build and present the dialog. Nothing is
    /// shown for files that cannot be read.
    pub fn open(
        files: Vec<gio::File>,
        parent: &impl IsA<gtk::Widget>,
        on_changed: impl Fn() + 'static,
    ) {
        let parent = parent.clone();
        glib::spawn_future_local(async move {
            let mut infos = Vec::new();
            for f in files {
                if let Ok(i) = f
                    .query_info_future(
                        ATTRS,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    infos.push((f, i));
                }
            }
            // The folders the dialog names can be opened where there is a window of folders
            // to open them in; a file chooser has none.
            let reveal = parent
                .root()
                .and_downcast::<crate::window::SpiralWindow>()
                .map(|win| {
                    let win = win.downgrade();
                    Rc::new(move |folder: &gio::File, select: Vec<gio::File>| {
                        if let Some(win) = win.upgrade() {
                            win.reveal(folder, select);
                        }
                    }) as Reveal
                });
            // A folder has a page about the disk it is on, where the filesystem says how big
            // that is.
            let disk = match infos.as_slice() {
                [(file, info)] if file_utils::is_dir(info) => file
                    .query_filesystem_info_future(FS_ATTRS, glib::Priority::DEFAULT)
                    .await
                    .ok()
                    .filter(|fs| fs.attribute_uint64("filesystem::size") > 0),
                _ => None,
            };
            if !infos.is_empty() && parent.root().is_some() {
                let dialog = Self::new(&infos, reveal, disk);
                dialog.connect_changed(on_changed);
                dialog.present(Some(&parent));
            }
        });
    }

    fn new(
        infos: &[(gio::File, gio::FileInfo)],
        reveal: Option<Reveal>,
        disk: Option<gio::FileInfo>,
    ) -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Properties"))
            .property("content-width", 460)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        dialog.set_child(Some(&toolbar));
        let general = dialog.general_page(infos, reveal);
        let mut pages = Vec::new();
        if let [(file, info)] = infos {
            if let Some(fs) = &disk {
                pages.push((
                    disk_page(file, fs),
                    "disk",
                    gettext("Disk"),
                    "drive-harddisk-symbolic",
                ));
            }
            if info.has_attribute("unix::mode") {
                pages.push((
                    permissions_page(file, info),
                    "permissions",
                    gettext("Permissions"),
                    "system-lock-screen-symbolic",
                ));
            }
        }
        if pages.is_empty() {
            toolbar.set_content(Some(&general));
            return dialog;
        }
        let stack = adw::ViewStack::new();
        stack
            .add_titled(&general, Some("general"), &gettext("General"))
            .set_icon_name(Some("document-properties-symbolic"));
        for (page, name, title, icon) in pages {
            stack
                .add_titled(&page, Some(name), &title)
                .set_icon_name(Some(icon));
        }
        // Words alone: with an icon beside each, three of them do not fit and the longest
        // loses its end.
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&stack)
            .display_mode(adw::InlineViewSwitcherDisplayMode::Labels)
            .build();
        header.set_title_widget(Some(&switcher));
        toolbar.set_content(Some(&stack));
        dialog
    }

    /// Called after something about the files changed (icon, permissions).
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.imp().changed.replace(Some(Box::new(f)));
    }

    fn emit_changed(&self) {
        if let Some(f) = self.imp().changed.borrow().as_ref() {
            f();
        }
    }

    /// A button at the end of `row` that shows `folder`, with `select` selected in it, and
    /// closes the dialog; nothing where there is no window to show it in.
    fn add_reveal(
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

    fn general_page(
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
            icon.set_from_gicon(&file_utils::icon_of(info));
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

    async fn pick_icon(&self, file: &gio::File, icon: &gtk::Image, reset: &gtk::Button) {
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
                icon.set_from_gicon(&gio::FileIcon::new(&image));
                reset.set_visible(true);
                self.emit_changed();
            }
            Err(e) => self.show_error(&gettext("Could Not Change Icon"), &e),
        }
    }

    fn show_error(&self, heading: &str, error: &glib::Error) {
        let alert = adw::AlertDialog::builder()
            .heading(heading)
            .body(error.message())
            .build();
        alert.add_response("ok", &gettext("_OK"));
        alert.present(Some(self));
    }
}

fn row(label: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(label)
        .subtitle(value)
        .subtitle_selectable(true)
        .build();
    r.add_css_class("property");
    r
}

/// The disk `dir` is on: how full it is, from `fs`, the filesystem's answer; the volume,
/// from the mount it is under and UDisks; and the drive that holds it, from UDisks, with
/// a way into Disks where that is installed. What UDisks has is filled in when it answers.
fn disk_page(dir: &gio::File, fs: &gio::FileInfo) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let usage = adw::PreferencesGroup::builder()
        .title(gettext("Usage"))
        .build();
    let size = fs.attribute_uint64("filesystem::size");
    let free = fs.attribute_uint64("filesystem::free");
    let used = if fs.has_attribute("filesystem::used") {
        fs.attribute_uint64("filesystem::used")
    } else {
        size.saturating_sub(free)
    };
    usage.add(&row(&gettext("Used"), &prefs::size(used)));
    usage.add(&row(&gettext("Free"), &prefs::size(free)));
    let capacity = row(&gettext("Capacity"), &prefs::size(size));
    let bar = gtk::LevelBar::builder()
        .value(used as f64 / size as f64)
        .width_request(120)
        .valign(gtk::Align::Center)
        .build();
    capacity.add_suffix(&bar);
    usage.add(&capacity);
    page.add(&usage);

    let mount = dir.path().and_then(|p| crate::disks::mount_of(&p));
    let fs_type = fs
        .attribute_string("filesystem::type")
        .map(|t| t.to_string());
    glib::spawn_future_local(glib::clone!(
        #[weak]
        page,
        async move {
            let device = mount
                .as_ref()
                .map(|m| m.source.clone())
                .filter(|s| s.starts_with("/dev/"));
            let volume = match &device {
                Some(device) => crate::disks::volume_of(device).await,
                None => None,
            }
            .unwrap_or_default();

            let mut fields = Vec::new();
            let format = volume
                .format
                .clone()
                .or_else(|| mount.as_ref().map(|m| m.fstype.clone()))
                .or(fs_type);
            if let Some(format) = format {
                fields.push((gettext("Format"), format));
            }
            if let Some(label) = &volume.label {
                fields.push((gettext("Label"), label.clone()));
            }
            if let Some(mount) = &mount {
                fields.push((
                    gettext("Mounted At"),
                    mount.point.to_string_lossy().into_owned(),
                ));
                if let Some(device) = &device {
                    fields.push((gettext("Device"), device.clone()));
                }
                if let Some(subvolume) = mount.option("subvol") {
                    fields.push((gettext("Subvolume"), subvolume.to_string()));
                }
                if let Some(compression) = mount
                    .option("compress")
                    .or_else(|| mount.option("compress-force"))
                {
                    fields.push((gettext("Compression"), compression.to_string()));
                }
                if mount.read_only() {
                    fields.push((gettext("Access"), gettext("Read-only")));
                }
            }
            if let Some(encryption) = &volume.encryption {
                fields.push((gettext("Encryption"), encryption.clone()));
            }
            if !fields.is_empty() {
                let group = adw::PreferencesGroup::builder()
                    .title(gettext("Volume"))
                    .build();
                group.add(&fields_card(&fields));
                page.add(&group);
            }

            let Some(drive) = &volume.drive else { return };
            let mut fields = Vec::new();
            if !drive.model.is_empty() {
                fields.push((gettext("Model"), drive.model.clone()));
            }
            fields.push((gettext("Type"), drive.kind.clone()));
            if drive.size > 0 {
                fields.push((gettext("Size"), prefs::size(drive.size)));
            }
            if let Some(table) = &volume.table {
                fields.push((gettext("Partition Table"), table.clone()));
            }
            if let Some((number, name)) = &volume.partition {
                let text = if name.is_empty() {
                    number.to_string()
                } else {
                    format!("{number} ({name})")
                };
                fields.push((gettext("Partition"), text));
            }
            let group = adw::PreferencesGroup::builder()
                .title(gettext("Drive"))
                .build();
            group.add(&fields_card(&fields));
            if let Some(device) = device
                && glib::find_program_in_path("gnome-disks").is_some()
            {
                let open = gtk::Button::builder()
                    .label(gettext("Open in Disks"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                open.connect_clicked(move |_| {
                    let argv = [
                        std::ffi::OsStr::new("gnome-disks"),
                        std::ffi::OsStr::new("--block-device"),
                        std::ffi::OsStr::new(&device),
                    ];
                    if let Err(e) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
                        glib::g_warning!("spiral", "cannot start Disks: {e}");
                    }
                });
                group.set_header_suffix(Some(&open));
            }
            page.add(&group);
        }
    ));
    page
}

/// Short facts two to a line, each a caption over its value, in a card as wide as a list:
/// half as tall as a list of rows with one fact each.
fn fields_card(fields: &[(String, String)]) -> gtk::Widget {
    let grid = gtk::Grid::builder()
        .hexpand(true)
        .column_homogeneous(true)
        .column_spacing(12)
        .row_spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    for (i, (title, value)) in fields.iter().enumerate() {
        let cell = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        cell.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
        cell.append(
            &gtk::Label::builder()
                .label(value)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .tooltip_text(value)
                .selectable(true)
                .build(),
        );
        grid.attach(&cell, (i % 2) as i32, (i / 2) as i32, 1, 1);
    }
    let card = gtk::Box::builder().css_classes(["card"]).build();
    card.append(&grid);
    card.upcast()
}

fn location_text(dir: &gio::File) -> String {
    dir.path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.uri().to_string())
}

// ---- permissions -------------------------------------------------------------------------

/// Bits of one class (owner, group, others) for a file: None, Read-only, Read and write.
const FILE_LEVELS: [u32; 3] = [0, 4, 6];
/// For a folder: None, List files only, Access files, Create and delete files.
const DIR_LEVELS: [u32; 4] = [0, 4, 5, 7];

fn file_level(bits: u32) -> u32 {
    match bits & 6 {
        0 => 0,
        4 => 1,
        _ => 2,
    }
}

fn dir_level(bits: u32) -> u32 {
    match (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0) {
        (false, _, _) => 0,
        (true, false, false) => 1,
        (true, false, true) => 2,
        (true, true, _) => 3,
    }
}

fn permissions_page(file: &gio::File, info: &gio::FileInfo) -> adw::PreferencesPage {
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
fn enclosed_dialog(parent: &impl IsA<gtk::Widget>, folder: &gio::File, done: Rc<dyn Fn()>) {
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

// ---- size ----------------------------------------------------------------------------------

fn size_text(total: u64, count: u64) -> String {
    if count <= 1 {
        prefs::size(total)
    } else {
        ngettext("%s (%d item)", "%s (%d items)", count as u32)
            .replace("%s", &prefs::size(total))
            .replace("%d", &count.to_string())
    }
}

/// Recursive size, updating the row as it goes.
async fn du(
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

fn full_date(dt: Option<glib::DateTime>) -> String {
    dt.and_then(|d| d.to_local().ok())
        .and_then(|d| d.format("%c").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| gettext("Unknown"))
}
