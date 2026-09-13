//! The disk page of a folder: how full the disk is, and what the volume and the drive are.

use super::*;

/// The disk `dir` is on: how full it is, from `fs`, the filesystem's answer; the volume,
/// from the mount it is under and UDisks; and the drive that holds it, from UDisks, with
/// a way into Disks where that is installed. What UDisks has is filled in when it answers.
pub(super) fn disk_page(dir: &gio::File, fs: &gio::FileInfo) -> adw::PreferencesPage {
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
        .width_request(120)
        .valign(gtk::Align::Center)
        .build();
    // The bar's own levels show a bar near its end as good news, which on a disk it is
    // not; without them it is the one colour, and the warning colour when nearly full.
    for offset in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(offset));
    }
    let fill = used as f64 / size as f64;
    bar.set_value(fill);
    if fill > 0.9 {
        bar.add_css_class("spiral-nearly-full");
    }
    capacity.add_suffix(&bar);
    usage.add(&capacity);
    page.add(&usage);

    // A share reached through gvfs has a local path too, into gvfs's own mount, which is
    // not the share's; what the filesystem answered says what it is instead.
    let mount = dir
        .path()
        .filter(|_| dir.is_native())
        .and_then(|p| crate::disks::mount_of(&p));
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
pub(super) fn fields_card(fields: &[(String, String)]) -> gtk::Widget {
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

pub(super) fn location_text(dir: &gio::File) -> String {
    dir.path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.uri().to_string())
}
