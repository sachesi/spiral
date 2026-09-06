//! File name validation, the rename popover and the new-folder dialog (Nautilus-style).

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, gio, glib, gtk};

/// Outcome of validating a candidate name in `parent`.
pub enum Verdict {
    Ok,
    /// Allowed, but worth telling the user (e.g. leading dot hides the file).
    Warning(String),
    Error(String),
}

/// Everything that can be judged from the name alone; whether it is taken is checked
/// separately, since that means asking the filesystem.
pub fn validate(name: &str, original: Option<&str>, is_folder: bool) -> Verdict {
    if name.is_empty() {
        return Verdict::Error(String::new());
    }
    if name.contains('/') {
        return Verdict::Error(if is_folder {
            gettext("Folder names cannot contain “/”.")
        } else {
            gettext("File names cannot contain “/”.")
        });
    }
    if name == "." || name == ".." {
        return Verdict::Error(if is_folder {
            gettext("A folder cannot be called “%s”.").replace("%s", name)
        } else {
            gettext("A file cannot be called “%s”.").replace("%s", name)
        });
    }
    if name.starts_with('.') && original.is_none_or(|o| !o.starts_with('.')) {
        return Verdict::Warning(if is_folder {
            gettext("Folders with “.” at the beginning of their name are hidden.")
        } else {
            gettext("Files with “.” at the beginning of their name are hidden.")
        });
    }
    Verdict::Ok
}

fn taken_message(is_folder: bool) -> String {
    if is_folder {
        gettext("A folder with that name already exists.")
    } else {
        gettext("A file with that name already exists.")
    }
}

/// Wire an entry, feedback label and accept button to `validate`. Returns a closure that
/// re-validates; the accept button is only sensitive for acceptable names.
fn bind_validation(
    entry: &impl IsA<gtk::Editable>,
    feedback: &gtk::Label,
    revealer: &gtk::Revealer,
    accept: &gtk::Button,
    parent: gio::File,
    original: Option<String>,
    is_folder: bool,
) {
    let entry: gtk::Editable = entry.clone().upcast();
    let check = glib::clone!(
        #[weak]
        entry,
        #[weak]
        feedback,
        #[weak]
        revealer,
        #[weak]
        accept,
        move || {
            let name = entry.text().trim().to_string();
            let (ok, msg) = match validate(&name, original.as_deref(), is_folder) {
                Verdict::Ok => (true, String::new()),
                Verdict::Warning(m) => (true, m),
                Verdict::Error(m) => (false, m),
            };
            let unchanged = original.as_deref() == Some(name.as_str());
            accept.set_sensitive(ok && !unchanged);
            feedback.set_text(&msg);
            revealer.set_reveal_child(!msg.is_empty());
            if !ok || unchanged {
                return;
            }
            // Ask the folder whether the name is taken; the answer only counts if the
            // entry still says the same thing when it arrives.
            let candidate = parent.child(&name);
            glib::spawn_future_local(glib::clone!(
                #[weak]
                entry,
                #[weak]
                feedback,
                #[weak]
                revealer,
                #[weak]
                accept,
                async move {
                    let taken = candidate
                        .query_info_future(
                            "standard::type",
                            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                            glib::Priority::DEFAULT,
                        )
                        .await
                        .is_ok();
                    if taken && entry.text().trim() == name {
                        accept.set_sensitive(false);
                        feedback.set_text(&taken_message(is_folder));
                        revealer.set_reveal_child(true);
                    }
                }
            ));
        }
    );
    entry.connect_changed(glib::clone!(
        #[strong]
        check,
        move |_| check()
    ));
    check();
}

/// Inline rename popover anchored to `anchor` inside `parent`. Resolves to the new name or None.
pub async fn rename_popover(
    parent: &impl IsA<gtk::Widget>,
    anchor: &gtk::gdk::Rectangle,
    dir: &gio::File,
    old: &str,
    is_folder: bool,
) -> Option<String> {
    let title = gtk::Label::builder()
        .label(if is_folder {
            gettext("Rename Folder")
        } else {
            gettext("Rename File")
        })
        .margin_bottom(12)
        .css_classes(["title-2"])
        .build();
    let entry = gtk::Entry::builder().text(old).margin_bottom(12).build();
    entry.update_property(&[gtk::accessible::Property::Label(&gettext("New Filename"))]);
    let feedback = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(0)
        .margin_bottom(12)
        .build();
    let revealer = gtk::Revealer::builder().child(&feedback).build();
    let button = gtk::Button::builder()
        .label(gettext("_Rename"))
        .use_underline(true)
        .halign(gtk::Align::End)
        .css_classes(["suggested-action"])
        .build();
    let bx = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    bx.append(&title);
    bx.append(&entry);
    bx.append(&revealer);
    bx.append(&button);
    let popover = gtk::Popover::builder()
        .child(&bx)
        .pointing_to(anchor)
        .build();
    popover.set_parent(parent);
    bind_validation(
        &entry,
        &feedback,
        &revealer,
        &button,
        dir.clone(),
        Some(old.to_string()),
        is_folder,
    );

    let (tx, rx) = oneshot::channel::<Option<String>>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let accept = glib::clone!(
        #[weak]
        entry,
        #[weak]
        button,
        #[weak]
        popover,
        #[strong]
        tx,
        move || {
            if !button.is_sensitive() {
                return;
            }
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Some(entry.text().trim().to_string()));
            }
            popover.popdown();
        }
    );
    button.connect_clicked(glib::clone!(
        #[strong]
        accept,
        move |_| accept()
    ));
    entry.connect_activate(move |_| accept());
    popover.connect_closed(glib::clone!(
        #[strong]
        tx,
        move |p| {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(None);
            }
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        }
    ));
    popover.popup();
    let stem = old
        .rfind('.')
        .filter(|&i| i > 0 && !is_folder)
        .unwrap_or(old.len());
    entry.grab_focus();
    entry.select_region(0, stem as i32);
    rx.await.ok().flatten()
}

/// "New Folder" dialog. Resolves to the folder name or None.
pub async fn new_folder_dialog(parent: &impl IsA<gtk::Widget>, dir: &gio::File) -> Option<String> {
    let entry = adw::EntryRow::builder()
        .title(gettext("_Folder Name"))
        .use_underline(true)
        .build();
    let feedback = gtk::Label::builder()
        .xalign(0.0)
        .margin_top(6)
        .css_classes(["warning", "caption"])
        .build();
    let revealer = gtk::Revealer::builder()
        .child(&feedback)
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .build();
    let create = gtk::Button::builder()
        .label(gettext("_Create"))
        .use_underline(true)
        .margin_top(6)
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    group.add(&revealer);
    group.add(&create);
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(gettext("New Folder"))
        .content_width(450)
        .child(&toolbar)
        .focus_widget(&entry)
        .build();
    bind_validation(
        &entry,
        &feedback,
        &revealer,
        &create,
        dir.clone(),
        None,
        true,
    );

    let (tx, rx) = oneshot::channel::<Option<String>>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let accept = glib::clone!(
        #[weak]
        entry,
        #[weak]
        create,
        #[weak]
        dialog,
        #[strong]
        tx,
        move || {
            if !create.is_sensitive() {
                return;
            }
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Some(entry.text().trim().to_string()));
            }
            dialog.close();
        }
    );
    create.connect_clicked(glib::clone!(
        #[strong]
        accept,
        move |_| accept()
    ));
    entry.connect_entry_activated(move |_| accept());
    dialog.connect_closed(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(None);
        }
    });
    dialog.present(Some(parent));
    rx.await.ok().flatten()
}
