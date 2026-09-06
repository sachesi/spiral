//! Dialogs a running job may need: name collisions (Nautilus-style) and I/O errors.

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::ops::job::{Resolution, name};
use crate::{adw, file_utils, gio, glib, gtk};

async fn describe(file: &gio::File, heading: &str) -> (gio::Icon, String) {
    let info = file
        .query_info_future(
            "standard::icon,standard::size,standard::type,time::modified",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .unwrap_or_default();
    let icon = info
        .icon()
        .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast());
    let mut text = format!("<b>{}</b>", glib::markup_escape_text(heading));
    if info.file_type() != gio::FileType::Directory && info.has_attribute("standard::size") {
        text.push_str(&format!(
            "\n{}",
            gettext("Size: %s").replace("%s", &crate::prefs::size(info.size() as u64))
        ));
    }
    if info.has_attribute("time::modified") {
        text.push_str(&format!(
            "\n{}",
            gettext("Last modified: %s").replace("%s", &file_utils::modified_string(&info))
        ));
    }
    (icon, text)
}

/// Ask how to handle `dest` already existing while transferring `src`.
/// Returns the resolution and whether to apply it to the rest of the job.
pub async fn ask_conflict(
    parent: &impl IsA<gtk::Widget>,
    src: &gio::File,
    dest: &gio::File,
    is_dir: bool,
) -> (Resolution, bool) {
    let dest_name = name(dest);
    let dest_dir = dest.parent().map(|p| name(&p)).unwrap_or_default();
    let primary = if is_dir {
        gettext("Merge Folder “%s”?")
    } else {
        gettext("Replace File “%s”?")
    }
    .replace("%s", &dest_name);
    let secondary = if is_dir {
        gettext(
            "Merging will ask for confirmation before replacing any files in the folder that conflict with the files being copied.",
        )
    } else {
        gettext("Another file with the same name already exists in “%s”. Replacing it will overwrite its content.").replace("%s", &dest_dir)
    };

    let cancel = gtk::Button::builder()
        .label(gettext("_Cancel"))
        .use_underline(true)
        .build();
    let skip = gtk::Button::builder()
        .label(gettext("_Skip"))
        .use_underline(true)
        .build();
    let rename = gtk::Button::builder()
        .label(gettext("_Rename"))
        .use_underline(true)
        .visible(false)
        .css_classes(["suggested-action"])
        .build();
    let replace = gtk::Button::builder()
        .label(if is_dir {
            gettext("_Merge")
        } else {
            gettext("_Replace")
        })
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&replace);
    header.pack_end(&rename);
    header.pack_end(&skip);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(primary)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .max_width_chars(50)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["title-2"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(secondary)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .max_width_chars(50)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build(),
    );
    let files = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Start)
        .build();
    for (file, heading) in [
        (
            dest,
            if is_dir {
                gettext("Original folder")
            } else {
                gettext("Original file")
            },
        ),
        (
            src,
            if is_dir {
                gettext("Merge with")
            } else {
                gettext("Replace with")
            },
        ),
    ] {
        let (icon, text) = describe(file, &heading).await;
        let row = gtk::Box::builder().spacing(12).build();
        row.append(
            &gtk::Image::builder()
                .gicon(&icon)
                .pixel_size(48)
                .valign(gtk::Align::Center)
                .build(),
        );
        row.append(
            &gtk::Label::builder()
                .use_markup(true)
                .label(text)
                .xalign(0.0)
                .build(),
        );
        files.append(&row);
    }
    let name_entry = gtk::Entry::builder().text(&dest_name).hexpand(true).build();
    let reset = gtk::Button::builder()
        .label(gettext("Re_set"))
        .use_underline(true)
        .build();
    let name_box = gtk::Box::builder().spacing(6).margin_top(6).build();
    name_box.append(&name_entry);
    name_box.append(&reset);
    let expander = gtk::Expander::builder()
        .label(gettext("Select a _new name for the destination"))
        .use_underline(true)
        .child(&name_box)
        .build();
    files.append(&expander);
    content.append(&files);
    let apply_all =
        gtk::CheckButton::with_label(&gettext("Apply this action to all files and folders"));
    content.append(&apply_all);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .child(&toolbar)
        .content_width(500)
        .build();

    // Rename replaces Replace once the user typed a different name.
    let update = glib::clone!(
        #[weak]
        name_entry,
        #[weak]
        expander,
        #[weak]
        rename,
        #[weak]
        replace,
        #[weak]
        apply_all,
        #[strong]
        dest_name,
        move || {
            let differs = expander.is_expanded()
                && name_entry.text().trim() != dest_name
                && !name_entry.text().trim().is_empty();
            rename.set_visible(differs);
            replace.set_visible(!differs);
            apply_all.set_sensitive(!differs);
        }
    );
    name_entry.connect_changed(glib::clone!(
        #[strong]
        update,
        move |_| update()
    ));
    expander.connect_expanded_notify(glib::clone!(
        #[strong]
        update,
        #[weak]
        name_entry,
        move |e| {
            if e.is_expanded() {
                name_entry.grab_focus();
            }
            update()
        }
    ));
    reset.connect_clicked(glib::clone!(
        #[weak]
        name_entry,
        #[strong]
        dest_name,
        move |_| name_entry.set_text(&dest_name)
    ));

    let (tx, rx) = oneshot::channel::<(Resolution, bool)>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let finish = |button: &gtk::Button, make: std::rc::Rc<dyn Fn() -> Resolution>| {
        button.connect_clicked(glib::clone!(
            #[strong]
            tx,
            #[weak]
            dialog,
            #[weak]
            apply_all,
            move |_| {
                if let Some(tx) = tx.borrow_mut().take() {
                    let all = apply_all.is_active() && apply_all.is_sensitive();
                    let _ = tx.send((make(), all));
                }
                dialog.close();
            }
        ));
    };
    finish(&cancel, std::rc::Rc::new(|| Resolution::Cancel));
    finish(&skip, std::rc::Rc::new(|| Resolution::Skip));
    finish(&replace, std::rc::Rc::new(|| Resolution::Replace));
    let entry_for_rename = name_entry.clone();
    finish(
        &rename,
        std::rc::Rc::new(move || Resolution::Rename(entry_for_rename.text().trim().to_string())),
    );
    name_entry.connect_activate(glib::clone!(
        #[weak]
        rename,
        move |_| {
            if rename.is_visible() {
                rename.emit_clicked();
            }
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        tx,
        move |_| {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send((Resolution::Cancel, false));
            }
        }
    ));
    dialog.set_default_widget(Some(&replace));
    dialog.present(Some(parent));
    rx.await.unwrap_or((Resolution::Cancel, false))
}

#[derive(Debug, PartialEq)]
pub enum ErrorChoice {
    Skip,
    Retry,
    Cancel,
}

/// Report an I/O error on `file` and ask whether to skip it, retry or stop the job.
pub async fn ask_error(
    parent: &impl IsA<gtk::Widget>,
    verb: &str,
    file: &gio::File,
    error: &glib::Error,
) -> ErrorChoice {
    let dialog = adw::AlertDialog::builder()
        .heading(
            gettext("Error While %v “%s”")
                .replace("%v", verb)
                .replace("%s", &name(file)),
        )
        .body(error.message())
        .close_response("cancel")
        .default_response("skip")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("retry", &gettext("_Retry")),
        ("skip", &gettext("_Skip")),
    ]);
    match dialog.choose_future(Some(parent)).await.as_str() {
        "skip" => ErrorChoice::Skip,
        "retry" => ErrorChoice::Retry,
        _ => ErrorChoice::Cancel,
    }
}
