//! Preferences dialog: every row writes its GSettings key immediately.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, gio, glib, gtk, prefs};

pub fn preferences_dialog() -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::builder()
        .title(gettext("Preferences"))
        .search_enabled(false)
        .build();
    let settings = prefs::settings();
    let page = adw::PreferencesPage::builder()
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();

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
        "date-format",
        &gettext("Date Format"),
        None,
        &[gettext("Relative"), gettext("Full Date and Time")],
    ));
    general.add(&terminal_row(&settings));
    page.add(&general);

    let behaviour = adw::PreferencesGroup::builder()
        .title(gettext("Behaviour"))
        .build();
    behaviour.add(&choice_row(
        &settings,
        "click-policy",
        &gettext("Open Items With"),
        None,
        &[gettext("Double Click"), gettext("Single Click")],
    ));
    let ask_on_drop = adw::SwitchRow::builder()
        .title(gettext("Ask What to Do With Dropped Files"))
        .subtitle(gettext(
            "Off, Ctrl copies and dragging inside Spiral moves without asking",
        ))
        .build();
    settings.bind("ask-on-drop", &ask_on_drop, "active").build();
    behaviour.add(&ask_on_drop);
    page.add(&behaviour);

    let network = adw::PreferencesGroup::builder()
        .title(gettext("Network"))
        .description(gettext(
            "Shares on other machines are reached through gvfs, and nothing here works without it.",
        ))
        .build();
    let use_network = adw::SwitchRow::builder()
        .title(gettext("Network Locations"))
        .subtitle(gettext(
            "“Connect to Server…” in the main menu, the network in the sidebar, and shares opened by address",
        ))
        .build();
    settings.bind("use-network", &use_network, "active").build();
    network.add(&use_network);
    page.add(&network);
    dialog.add(&page);

    let page = adw::PreferencesPage::builder()
        .title(gettext("Views"))
        .icon_name("view-grid-symbolic")
        .build();

    let views = adw::PreferencesGroup::builder()
        .title(gettext("Views"))
        .build();
    let folders_first = adw::SwitchRow::builder()
        .title(gettext("Sort Folders Before Files"))
        .build();
    settings
        .bind("folders-first", &folders_first, "active")
        .build();
    views.add(&folders_first);
    let columns = adw::SwitchRow::builder()
        .title(gettext("Column View"))
        .subtitle(gettext(
            "A third view drawing the path as a strip of lists, one folder per column",
        ))
        .build();
    settings.bind("use-column-view", &columns, "active").build();
    views.add(&columns);
    let guess = adw::SwitchRow::builder()
        .title(gettext("Grid View for Media Folders"))
        .subtitle(gettext(
            "Folders that are mostly images and videos open in grid view",
        ))
        .build();
    settings.bind("guess-view", &guess, "active").build();
    views.add(&guess);
    let remember = adw::SwitchRow::builder()
        .title(gettext("Remember View per Folder"))
        .subtitle(gettext(
            "Switching the view or the sort order applies to the current folder only",
        ))
        .build();
    settings.bind("remember-view", &remember, "active").build();
    remember.set_sensitive(crate::prefs::per_folder_available());
    views.add(&remember);
    let tree = adw::SwitchRow::builder()
        .title(gettext("Expandable Folders in List View"))
        .build();
    settings.bind("use-tree-view", &tree, "active").build();
    views.add(&tree);
    views.add(&sort_row(&settings));
    page.add(&views);

    let sidebar = adw::PreferencesGroup::builder()
        .title(gettext("Sidebar"))
        .build();
    let root = adw::SwitchRow::builder()
        .title(gettext("Root"))
        .subtitle(gettext("The top of the filesystem, above the bookmarks"))
        .build();
    settings.bind("show-root", &root, "active").build();
    sidebar.add(&root);
    let favorites = adw::SwitchRow::builder()
        .title(gettext("Favorites"))
        .subtitle(gettext("The starred files and folders"))
        .build();
    settings
        .bind("show-favorites", &favorites, "active")
        .build();
    sidebar.add(&favorites);
    page.add(&sidebar);

    let tags = adw::PreferencesGroup::builder()
        .title(gettext("Tags"))
        .description(gettext(
            "Tags are written to the files themselves, so they follow a file wherever it goes and other programs can read them.",
        ))
        .build();
    let use_tags = adw::SwitchRow::builder()
        .title(gettext("Colour Tags"))
        .subtitle(gettext(
            "A row of colours in the context menu, a dot on each tagged file and the tags listed in the sidebar",
        ))
        .build();
    settings.bind("use-tags", &use_tags, "active").build();
    tags.add(&use_tags);
    page.add(&tags);
    dialog.add(&page);

    let page = adw::PreferencesPage::builder()
        .title(gettext("Menus"))
        .icon_name("open-menu-symbolic")
        .build();

    let optional = adw::PreferencesGroup::builder()
        .title(gettext("Optional Context Menu Actions"))
        .description(gettext(
            "A middle click still opens a folder in a new tab, Shift+Delete still deletes permanently, and links can always be pasted.",
        ))
        .build();
    for (key, title) in [
        ("show-open-new-tab", gettext("Open in New Tab")),
        ("show-open-new-window", gettext("Open in New Window")),
        ("show-copy-to", gettext("Copy to…")),
        ("show-move-to", gettext("Move to…")),
    ] {
        let row = adw::SwitchRow::builder().title(title).build();
        settings.bind(key, &row, "active").build();
        optional.add(&row);
    }
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
    dialog.add(&page);

    let page = adw::PreferencesPage::builder()
        .title(gettext("Speed"))
        .icon_name("power-profile-performance-symbolic")
        .build();

    let performance = adw::PreferencesGroup::builder()
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

/// The order folders open in, the same six the view menu offers. It is the default only:
/// a folder with an order of its own keeps it, and with per-folder memory off, sorting from
/// the menu writes this very setting.
fn sort_row(settings: &gio::Settings) -> adw::ComboRow {
    const ORDERS: [(&str, bool); 6] = [
        ("name", false),
        ("name", true),
        ("modified", true),
        ("modified", false),
        ("size", true),
        ("type", false),
    ];
    let labels = [
        gettext("A-Z"),
        gettext("Z-A"),
        gettext("Last Modified"),
        gettext("First Modified"),
        gettext("Size"),
        gettext("Type"),
    ];
    let current = |s: &gio::Settings| {
        let key = s.string("sort-key");
        let reversed = s.boolean("sort-reversed");
        ORDERS
            .iter()
            .position(|(k, r)| *k == key.as_str() && *r == reversed)
            .unwrap_or(0) as u32
    };
    let row = adw::ComboRow::builder()
        .title(gettext("Sort Order"))
        .subtitle(gettext(
            "How folders without an order of their own are sorted",
        ))
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .selected(current(settings))
        .build();
    row.connect_selected_notify(glib::clone!(
        #[strong]
        settings,
        move |row| {
            let (key, reversed) = ORDERS[row.selected() as usize];
            let _ = settings.set_string("sort-key", key);
            let _ = settings.set_boolean("sort-reversed", reversed);
        }
    ));
    // Sorting from the menu writes the same keys when views are not remembered per folder.
    for key in ["sort-key", "sort-reversed"] {
        settings.connect_changed(
            Some(key),
            glib::clone!(
                #[weak]
                row,
                move |s, _| row.set_selected(current(s))
            ),
        );
    }
    row
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
