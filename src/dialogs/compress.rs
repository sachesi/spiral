//! "Create Archive" dialog: name plus one of the formats the installed tools can produce.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::ops::archive::{Format, creatable_formats};
use crate::{adw, glib, gtk};

/// Resolves to the archive file name (with extension), or None if cancelled.
pub async fn compress_dialog(parent: &impl IsA<gtk::Widget>, default_name: &str) -> Option<String> {
    let formats: Vec<Format> = creatable_formats();
    if formats.is_empty() {
        return None;
    }
    let name = adw::EntryRow::builder()
        .title(gettext("Archive _Name"))
        .use_underline(true)
        .text(default_name)
        .build();
    let labels: Vec<&str> = formats.iter().map(|f| f.extension).collect();
    let format = adw::ComboRow::builder()
        .title(gettext("_Format"))
        .use_underline(true)
        .model(&gtk::StringList::new(&labels))
        .subtitle(&formats[0].description)
        .build();
    format.connect_selected_notify({
        let formats = formats.clone();
        move |row| {
            if let Some(f) = formats.get(row.selected() as usize) {
                row.set_subtitle(&f.description);
            }
        }
    });
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&name);
    list.append(&format);

    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Create Archive"))
        .extra_child(&list)
        .close_response("cancel")
        .default_response("create")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("create", &gettext("C_reate")),
    ]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    let valid = |n: &str| !n.trim().is_empty() && !n.contains('/');
    name.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |n| dialog.set_response_enabled("create", valid(&n.text()))
    ));
    name.connect_entry_activated(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            if dialog.is_response_enabled("create") {
                dialog.emit_by_name::<()>("response", &[&"create"]);
                dialog.close();
            }
        }
    ));
    name.select_region(0, -1);
    if dialog.choose_future(Some(parent)).await != "create" {
        return None;
    }
    let ext = formats[format.selected() as usize].extension;
    let text = name.text();
    let text = text.trim();
    // A name typed with the extension already on it is left alone.
    Some(if text.ends_with(ext) {
        text.to_string()
    } else {
        format!("{text}{ext}")
    })
}
