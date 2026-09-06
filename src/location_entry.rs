//! Inline completion for the location entry: as a path is typed, the rest of the first
//! matching folder name is appended and selected, so typing on replaces it and Tab or
//! Right accepts it.

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
