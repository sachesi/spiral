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

const ATTRS: &str = "standard::*,time::modified,time::access,owner::user,owner::group,unix::mode,\
unix::uid,access::*,selinux::context,metadata::custom-icon,metadata::custom-icon-name";

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
            if !infos.is_empty() && parent.root().is_some() {
                let dialog = Self::new(&infos);
                dialog.connect_changed(on_changed);
                dialog.present(Some(&parent));
            }
        });
    }

    fn new(infos: &[(gio::File, gio::FileInfo)]) -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Properties"))
            .property("content-width", 460)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        dialog.set_child(Some(&toolbar));
        let general = dialog.general_page(infos);
        match infos {
            [(file, info)] if info.has_attribute("unix::mode") => {
                let stack = adw::ViewStack::new();
                stack
                    .add_titled(&general, Some("general"), &gettext("General"))
                    .set_icon_name(Some("document-properties-symbolic"));
                stack
                    .add_titled(
                        &permissions_page(file, info),
                        Some("permissions"),
                        &gettext("Permissions"),
                    )
                    .set_icon_name(Some("system-lock-screen-symbolic"));
                let switcher = adw::ViewSwitcher::builder()
                    .stack(&stack)
                    .policy(adw::ViewSwitcherPolicy::Wide)
                    .build();
                header.set_title_widget(Some(&switcher));
                toolbar.set_content(Some(&stack));
            }
            _ => toolbar.set_content(Some(&general)),
        }
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

    fn general_page(&self, infos: &[(gio::File, gio::FileInfo)]) -> adw::PreferencesPage {
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
                group.add(&row(&gettext("Location"), &location_text(&parent)));
            }
            group.add(&row(
                &gettext("Modified"),
                &full_date(info.modification_date_time()),
            ));
            group.add(&row(
                &gettext("Accessed"),
                &full_date(info.access_date_time()),
            ));
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
            move |combo| {
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

    if let Some(context) = info.attribute_string("selinux::context") {
        let security = adw::PreferencesGroup::new();
        security.add(&row(&gettext("Security Context"), &context));
        page.add(&security);
    }
    page
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
