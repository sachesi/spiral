//! "Visible Columns" dialog for the list view: a switch per column, written straight to the
//! `visible-columns` key.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, file_utils, gtk, prefs};

pub fn columns_dialog() -> adw::Dialog {
    let settings = prefs::settings();
    let visible: Vec<String> = settings
        .strv("visible-columns")
        .iter()
        .map(|s| s.to_string())
        .collect();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let mut keys: Vec<&'static str> = Vec::new();
    for (key, title) in file_utils::optional_columns() {
        keys.push(key);
        list.append(&switch_row(&title, key, &visible, &keys, &settings));
    }
    keys.push("star");
    list.append(&switch_row(
        &gettext("Star"),
        "star",
        &visible,
        &keys,
        &settings,
    ));
    let view = adw::ToolbarView::builder()
        .content(
            &gtk::ScrolledWindow::builder()
                .propagate_natural_height(true)
                .child(&list)
                .build(),
        )
        .build();
    view.add_top_bar(&adw::HeaderBar::new());
    adw::Dialog::builder()
        .title(gettext("Visible Columns"))
        .content_width(360)
        .child(&view)
        .build()
}

/// Every switch rewrites the key from the rows' state, so the stored order stays canonical.
fn switch_row(
    title: &str,
    key: &'static str,
    visible: &[String],
    keys: &[&'static str],
    settings: &crate::gio::Settings,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(visible.iter().any(|v| v == key))
        .build();
    let settings = settings.clone();
    let keys: Vec<&'static str> = keys.to_vec();
    row.connect_active_notify(move |row| {
        let mut on: Vec<&str> = settings
            .strv("visible-columns")
            .iter()
            .map(|s| s.as_str())
            .filter(|k| *k != key)
            .map(|k| keys.iter().copied().find(|c| *c == k).unwrap_or(""))
            .filter(|k| !k.is_empty())
            .collect();
        if row.is_active() {
            on.push(key);
        }
        on.sort_by_key(|k| keys.iter().position(|c| c == k));
        let _ = settings.set_strv("visible-columns", on.as_slice());
    });
    row
}
