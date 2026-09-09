//! Where a location is typed: inline completion as a path is typed, so the rest of the
//! first matching folder name is appended and selected and typing on replaces it, and the
//! path bar that gives way to an entry in the headers the choosers build in code.

use std::cell::Cell;
use std::rc::Rc;

use crate::gtk::prelude::*;
use crate::{gio, glib, gtk};

/// Set the entry's text without triggering completion.
pub fn set_text_quiet(entry: &gtk::Entry, text: &str) {
    unsafe { entry.set_data("quiet", true) };
    entry.set_text(text);
    unsafe { entry.steal_data::<bool>("quiet") };
}

pub fn attach(entry: &gtk::Entry) {
    // Complete after insertions only, so deleting never fights the completion.
    let inserted = Rc::new(Cell::new(false));
    if let Some(delegate) = entry.delegate() {
        delegate.connect_insert_text(glib::clone!(
            #[strong]
            inserted,
            move |_, _, _| inserted.set(true)
        ));
    }
    let generation = Rc::new(Cell::new(0u64));
    entry.connect_changed(move |entry| {
        let text = entry.text();
        let grew = inserted.replace(false);
        let quiet = unsafe { entry.data::<bool>("quiet").is_some() };
        generation.set(generation.get() + 1);
        if quiet || !grew || text.contains("://") || text.ends_with('/') {
            return;
        }
        // Only complete when typing at the end.
        if entry.position() != text.chars().count() as i32 {
            return;
        }
        let Some((dir, prefix)) = text.rsplit_once('/') else {
            return;
        };
        let dir = if let Some(rest) = dir.strip_prefix('~') {
            gio::File::for_path(glib::home_dir())
                .resolve_relative_path(rest.trim_start_matches('/'))
        } else {
            gio::File::for_commandline_arg(if dir.is_empty() { "/" } else { dir })
        };
        let typed = text.to_string();
        let prefix = prefix.to_string();
        let show_hidden = prefix.starts_with('.');
        let generation = generation.clone();
        let this = generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak]
            entry,
            async move {
                let folded = prefix.to_lowercase();
                let names: Vec<String> = crate::ops::children(
                    &dir,
                    "standard::name,standard::display-name,standard::type,standard::is-hidden",
                )
                .await
                .into_iter()
                .filter(|(_, i)| i.file_type() == gio::FileType::Directory)
                .filter(|(_, i)| show_hidden || !i.is_hidden())
                .map(|(_, i)| i.display_name().to_string())
                .filter(|n| n.to_lowercase().starts_with(&folded))
                .collect();
                if generation.get() != this || names.is_empty() {
                    return;
                }
                // Longest case-insensitive common prefix of all matches, in the first match's casing.
                let first = &names[0];
                let mut common = first.len();
                for n in &names[1..] {
                    let shared = first
                        .char_indices()
                        .zip(n.chars())
                        .take_while(|((_, a), b)| a.to_lowercase().eq(b.to_lowercase()))
                        .map(|((i, a), _)| i + a.len_utf8())
                        .last()
                        .unwrap_or(0);
                    common = common.min(shared);
                }
                let suffix = &first[prefix.len().min(common)..common];
                let suffix = if names.len() == 1 {
                    format!("{suffix}/")
                } else {
                    suffix.to_string()
                };
                if suffix.is_empty() {
                    return;
                }
                let start = typed.chars().count() as i32;
                set_text_quiet(&entry, &format!("{typed}{suffix}"));
                entry.select_region(start, -1);
            }
        ));
    });
}

/// The folder a typed location means: a URI, a path from the home folder, or a path.
pub fn resolve(text: &str) -> gio::File {
    if text.contains("://") {
        gio::File::for_uri(text)
    } else if let Some(rest) = text.strip_prefix('~') {
        gio::File::for_path(glib::home_dir()).resolve_relative_path(rest.trim_start_matches('/'))
    } else {
        gio::File::for_commandline_arg(text)
    }
}

/// A path bar that gives way to an entry, the way the window's header does it in
/// Blueprint: a click on the current folder, or `edit`, swaps the breadcrumbs for a place
/// to type; a location typed, Esc, or the focus leaving brings them back. The choosers
/// build their headers in code, so they take theirs from here.
pub struct LocationBar {
    stack: gtk::Stack,
    entry: gtk::Entry,
    view: crate::browser_view::BrowserView,
}

/// Show the entry, filled with where the view is, and give it the keyboard.
fn show_entry(stack: &gtk::Stack, entry: &gtk::Entry, view: &crate::browser_view::BrowserView) {
    let text = view
        .location()
        .map(|f| match f.path() {
            Some(p) => p.to_string_lossy().into_owned(),
            None => f.uri().to_string(),
        })
        .unwrap_or_default();
    set_text_quiet(entry, &text);
    stack.set_visible_child_name("location");
    entry.grab_focus();
    entry.set_position(-1);
}

impl LocationBar {
    pub fn new(view: &crate::browser_view::BrowserView) -> Self {
        let path_bar: crate::path_bar::PathBar = glib::Object::new();
        path_bar.set_location(view.location().as_ref());
        let entry = gtk::Entry::builder()
            .primary_icon_name("folder-symbolic")
            .hexpand(true)
            .build();
        attach(&entry);
        let cancel = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettextrs::gettext("Cancel"))
            .build();
        let entry_box = gtk::Box::builder().css_classes(["linked"]).build();
        entry_box.append(&entry);
        entry_box.append(&cancel);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .build();
        stack.add_named(&path_bar, Some("pathbar"));
        stack.add_named(&entry_box, Some("location"));

        // Back to the breadcrumbs, with the keyboard where it was before the typing.
        let done = glib::clone!(
            #[weak]
            stack,
            #[weak]
            view,
            move || {
                stack.set_visible_child_name("pathbar");
                view.grab_view_focus();
            }
        );
        view.connect_location_notify(glib::clone!(
            #[weak]
            path_bar,
            move |v| path_bar.set_location(v.location().as_ref())
        ));
        path_bar.connect_navigate(glib::clone!(
            #[weak]
            view,
            move |_, f| {
                view.go_to(f);
                view.grab_view_focus();
            }
        ));
        path_bar.connect_edit_location(glib::clone!(
            #[weak]
            stack,
            #[weak]
            entry,
            #[weak]
            view,
            move |_| show_entry(&stack, &entry, &view)
        ));
        entry.connect_activate(glib::clone!(
            #[strong]
            done,
            #[weak]
            view,
            move |entry| {
                let text = entry.text();
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                view.go_to(&resolve(text));
                done();
            }
        ));
        cancel.connect_clicked(glib::clone!(
            #[strong]
            done,
            move |_| done()
        ));
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(glib::clone!(
            #[strong]
            done,
            move |controller, k, _, _| match k {
                gtk::gdk::Key::Escape => {
                    done();
                    glib::Propagation::Stop
                }
                // Accept the inline completion. Tab never moves the focus on: leaving the
                // entry puts the crumbs back, which loses the path half typed whenever the
                // completion had nothing to add yet.
                gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab => {
                    if let Some(entry) = controller.widget().and_downcast::<gtk::Entry>() {
                        entry.set_position(-1);
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        ));
        entry.add_controller(key);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak]
            stack,
            move |_| stack.set_visible_child_name("pathbar")
        ));
        entry.add_controller(focus);

        Self {
            stack,
            entry,
            view: view.clone(),
        }
    }

    /// The widget for the header bar's title.
    pub fn widget(&self) -> &gtk::Stack {
        &self.stack
    }

    /// Swap the breadcrumbs for the entry, as Ctrl+L does in the window.
    pub fn edit(&self) {
        show_entry(&self.stack, &self.entry, &self.view);
    }
}
