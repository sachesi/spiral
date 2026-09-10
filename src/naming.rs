//! File name validation, the rename popover and the new-folder dialog.

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

/// Everything that can be judged from the name alone, and `max`, the longest name in bytes
/// the folder takes, where it is known; whether it is taken is checked separately, since
/// that means asking the filesystem.
pub fn validate(
    name: &str,
    original: Option<&str>,
    is_folder: bool,
    max: Option<usize>,
) -> Verdict {
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
    if max.is_some_and(|max| name.len() > max) {
        return Verdict::Error(if is_folder {
            gettext("Folder name is too long.")
        } else {
            gettext("File name is too long.")
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

/// Where the name ends and the extension begins, counted in characters: a selection in an
/// entry is counted in those, not in bytes, and a Cyrillic name is longer in bytes than it
/// looks. -1 selects to the end, for a name with no extension to leave out. A leading dot
/// belongs to the name.
pub(crate) fn stem_end(name: &str, is_folder: bool) -> i32 {
    match name.rfind('.').filter(|&i| i > 0 && !is_folder) {
        Some(dot) => name[..dot].chars().count() as i32,
        None => -1,
    }
}

/// The longest name in bytes `dir` takes, as its filesystem says; `None` for a folder
/// that is not local, or a filesystem that will not say. Asked off the main loop, since a
/// network mount may take its time to answer.
pub async fn name_max(dir: &gio::File) -> Option<usize> {
    let path = std::ffi::CString::new(dir.path()?.into_os_string().into_encoded_bytes()).ok()?;
    gio::spawn_blocking(move || {
        // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
        let max = unsafe { libc::pathconf(path.as_ptr(), libc::_PC_NAME_MAX) };
        usize::try_from(max).ok()
    })
    .await
    .ok()
    .flatten()
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
    let dir = parent.clone();
    let max = std::rc::Rc::new(std::cell::Cell::new(None));
    let check = glib::clone!(
        #[strong]
        max,
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
            let (ok, msg) = match validate(&name, original.as_deref(), is_folder, max.get()) {
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
    glib::spawn_future_local(async move {
        max.set(name_max(&dir).await);
        check();
    });
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
    entry.grab_focus();
    entry.select_region(0, stem_end(old, is_folder));
    rx.await.ok().flatten()
}

/// "New Folder" dialog. Resolves to the folder name or None.
/// Ask for the name of a new folder in `dir`, starting from `suggested`.
pub async fn new_folder_dialog(
    parent: &impl IsA<gtk::Widget>,
    dir: &gio::File,
    suggested: &str,
) -> Option<String> {
    name_dialog(
        parent,
        dir,
        &gettext("New Folder"),
        &gettext("_Folder Name"),
        suggested,
        true,
    )
    .await
}

/// What `names` start with in common, as the name for a folder to hold them: their
/// extensions left out, a word or number the names only share the start of dropped, and
/// what trails it -- spaces, dashes, dots, an opening bracket -- trimmed off; nothing when
/// they share less than three characters.
pub fn common_name(names: &[String]) -> String {
    let stems: Vec<Vec<char>> = names
        .iter()
        .map(|n| match n.rfind('.').filter(|&i| i > 0) {
            Some(dot) => n[..dot].chars().collect(),
            None => n.chars().collect(),
        })
        .collect();
    let Some(first) = stems.first() else {
        return String::new();
    };
    let len = stems.iter().fold(first.len(), |len, s| {
        first[..len]
            .iter()
            .zip(s)
            .take_while(|(a, b)| a == b)
            .count()
    });
    let mut len = len;
    let cut_short = |len: usize| {
        stems
            .iter()
            .any(|s| s.get(len).is_some_and(|c| c.is_alphanumeric()))
    };
    if len > 0 && first[len - 1].is_alphanumeric() && cut_short(len) {
        len = first[..len]
            .iter()
            .rposition(|c| !c.is_alphanumeric())
            .unwrap_or(0);
    }
    let common: String = first[..len].iter().collect();
    let common = common.trim_end_matches(|c: char| c.is_whitespace() || "-_.([{".contains(c));
    if common.chars().count() < 3 {
        return String::new();
    }
    common.to_string()
}

/// "New Document" dialog, starting from `suggested`: the name of the template the document
/// comes from, or the one an empty document is given. Resolves to the file name or None.
pub async fn new_file_dialog(
    parent: &impl IsA<gtk::Widget>,
    dir: &gio::File,
    suggested: &str,
) -> Option<String> {
    name_dialog(
        parent,
        dir,
        &gettext("New Document"),
        &gettext("_File Name"),
        suggested,
        false,
    )
    .await
}

/// Ask for a name for something about to be made in `dir`, and check it as it is typed.
async fn name_dialog(
    parent: &impl IsA<gtk::Widget>,
    dir: &gio::File,
    title: &str,
    entry_title: &str,
    suggested: &str,
    is_folder: bool,
) -> Option<String> {
    let entry = adw::EntryRow::builder()
        .title(entry_title)
        .use_underline(true)
        .text(suggested)
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
        .title(title)
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
        is_folder,
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
    // A suggested name is there to be replaced, all but the extension: the name is what
    // the document is about, the extension what it is.
    let stem = stem_end(suggested, is_folder);
    // The row selects all of its text as it takes the keyboard, and takes it again when
    // the window itself is given focus, so the narrower selection is put back after each
    // of those -- until the name has been typed into, when the selection is the user's.
    let suggested = suggested.to_string();
    let reselect = std::rc::Rc::new(move |entry: &adw::EntryRow| {
        if entry.text() != suggested {
            return;
        }
        // An idle later, because the selection it makes for itself comes after this.
        let entry = entry.clone();
        glib::idle_add_local_once(move || entry.select_region(0, stem));
    });
    entry.connect_map(glib::clone!(
        #[strong]
        reselect,
        move |entry| reselect(entry)
    ));
    if let Some(text) = entry.delegate().and_downcast::<gtk::Text>() {
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak]
            entry,
            move |_| reselect(&entry)
        ));
        text.add_controller(focus);
    }
    rx.await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_longer_than_the_folder_takes_is_refused() {
        let long = "x".repeat(256);
        assert!(matches!(
            validate(&long, None, false, Some(255)),
            Verdict::Error(_)
        ));
        assert!(matches!(
            validate(&long[1..], None, false, Some(255)),
            Verdict::Ok
        ));
        // Counted in bytes, as the filesystem counts them.
        let wide = "ї".repeat(128);
        assert!(matches!(
            validate(&wide, None, true, Some(255)),
            Verdict::Error(_)
        ));
        assert!(matches!(validate(&long, None, false, None), Verdict::Ok));
    }

    #[test]
    fn a_folder_for_several_files_is_named_after_what_they_share() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            common_name(&names(&["Trip 2024 - 01.jpg", "Trip 2024 - 02.jpg"])),
            "Trip 2024"
        );
        assert_eq!(common_name(&names(&["report.pdf", "report.odt"])), "report");
        assert_eq!(
            common_name(&names(&["IMG_1234.jpg", "IMG_1256.jpg"])),
            "IMG"
        );
        assert_eq!(common_name(&names(&["ab1.txt", "ab2.txt"])), "");
        assert_eq!(common_name(&names(&["one", "two"])), "");
    }
}
