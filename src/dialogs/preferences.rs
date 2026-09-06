//! Preferences dialog: every row writes its GSettings key immediately.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, gio, gtk, prefs};

pub fn preferences_dialog() -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Preferences"))
        .search_enabled(false)
        .build();
    let settings = prefs::settings();
    let page = adw::PreferencesPage::new();

    let general = adw::PreferencesGroup::builder()
        .title(gettext("General"))
        .build();
    general.add(&choice_row(
        &settings,
        "size-units",
        &gettext("Size Units"),
        Some(&gettext(
            "Decimal uses 1000 bytes per kB, binary 1024 per KiB",
        )),
        &[gettext("Decimal (kB, MB)"), gettext("Binary (KiB, MiB)")],
    ));
    general.add(&choice_row(
        &settings,
        "click-policy",
        &gettext("Open Items With"),
        None,
        &[gettext("Double Click"), gettext("Single Click")],
    ));
    let folders_first = adw::SwitchRow::builder()
        .title(gettext("Sort Folders Before Files"))
        .build();
    settings
        .bind("folders-first", &folders_first, "active")
        .build();
    general.add(&folders_first);
    general.add(&choice_row(
        &settings,
        "date-format",
        &gettext("Date Format"),
        None,
        &[gettext("Relative"), gettext("Full Date and Time")],
    ));
    let ask_on_drop = adw::SwitchRow::builder()
        .title(gettext("Ask What to Do With Dropped Files"))
        .subtitle(gettext(
            "Off, Ctrl copies and dragging inside Spiral moves without asking",
        ))
        .build();
    settings.bind("ask-on-drop", &ask_on_drop, "active").build();
    general.add(&ask_on_drop);
    general.add(&terminal_row(&settings));
    page.add(&general);

    let views = adw::PreferencesGroup::builder()
        .title(gettext("Views"))
        .build();
    let remember = adw::SwitchRow::builder()
        .title(gettext("Remember View per Folder"))
        .subtitle(gettext(
            "Switching the view or the sort order applies to the current folder only",
        ))
        .build();
    settings.bind("remember-view", &remember, "active").build();
    views.add(&remember);
    let guess = adw::SwitchRow::builder()
        .title(gettext("Grid View for Media Folders"))
        .subtitle(gettext(
            "Folders that are mostly images and videos open in grid view",
        ))
        .build();
    settings.bind("guess-view", &guess, "active").build();
    views.add(&guess);
    let tree = adw::SwitchRow::builder()
        .title(gettext("Expandable Folders in List View"))
        .build();
    settings.bind("use-tree-view", &tree, "active").build();
    views.add(&tree);
    page.add(&views);

    let optional = adw::PreferencesGroup::builder()
        .title(gettext("Optional Context Menu Actions"))
        .description(gettext(
            "Shift+Delete deletes permanently either way, and links can always be pasted.",
        ))
        .build();
    let create_link = adw::SwitchRow::builder()
        .title(gettext("Create Link"))
        .build();
    settings
        .bind("show-create-link", &create_link, "active")
        .build();
    optional.add(&create_link);
    let delete_permanently = adw::SwitchRow::builder()
        .title(gettext("Delete Permanently"))
        .build();
    settings
        .bind("show-delete-permanently", &delete_permanently, "active")
        .build();
    optional.add(&delete_permanently);
    page.add(&optional);

    let performance = adw::PreferencesGroup::builder()
        .title(gettext("Performance"))
        .description(gettext(
            "Reading every file on a network share or a slow disk can take a while.",
        ))
        .build();
    let scopes = [
        gettext("Local Files Only"),
        gettext("All Files"),
        gettext("Never"),
    ];
    performance.add(&choice_row(
        &settings,
        "thumbnails",
        &gettext("Show Thumbnails"),
        None,
        &scopes,
    ));
    performance.add(&choice_row(
        &settings,
        "item-counts",
        &gettext("Count Items in Folders"),
        None,
        &scopes,
    ));
    performance.add(&choice_row(
        &settings,
        "recursive-search",
        &gettext("Search in Subfolders"),
        None,
        &scopes,
    ));
    page.add(&performance);

    dialog.add(&page);
    dialog
}

fn choice_row(
    settings: &gio::Settings,
    key: &str,
    title: &str,
    subtitle: Option<&str>,
    choices: &[String],
) -> adw::ComboRow {
    let model = gtk::StringList::new(&choices.iter().map(String::as_str).collect::<Vec<_>>());
    let row = adw::ComboRow::builder()
        .title(title)
        .subtitle(subtitle.unwrap_or_default())
        .model(&model)
        .selected(settings.enum_(key) as u32)
        .build();
    let settings = settings.clone();
    let key = key.to_string();
    row.connect_selected_notify(move |row| {
        let _ = settings.set_enum(&key, row.selected() as i32);
    });
    row
}

/// "Automatic" plus every terminal found in PATH; the key stores the executable name.
fn terminal_row(settings: &gio::Settings) -> adw::ComboRow {
    let found = crate::terminal::installed();
    let mut labels = vec![gettext("Automatic")];
    labels.extend(found.iter().map(|t| t.label.to_string()));
    let current = settings.string("terminal");
    let selected = found
        .iter()
        .position(|t| t.exec == current.as_str())
        .map_or(0, |i| i as u32 + 1);
    let subtitle = match found.first() {
        Some(t) => gettext("Automatic uses %s").replace("%s", t.label),
        None => gettext("No terminal emulator found in PATH"),
    };
    let row = adw::ComboRow::builder()
        .title(gettext("Terminal"))
        .subtitle(subtitle)
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .selected(selected)
        .build();
    let settings = settings.clone();
    row.connect_selected_notify(move |row| {
        let exec = match row.selected() {
            0 => "",
            i => found[i as usize - 1].exec,
        };
        let _ = settings.set_string("terminal", exec);
    });
    row
}
