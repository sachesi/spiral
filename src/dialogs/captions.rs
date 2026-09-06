//! "Captions" dialog for the grid view: three dropdowns for the lines under each name,
//! written straight to the `captions` key.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, file_utils, gtk, prefs};

pub fn captions_dialog() -> adw::Dialog {
    let settings = prefs::settings();
    let current: Vec<String> = settings
        .strv("captions")
        .iter()
        .map(|s| s.to_string())
        .collect();
    let kinds = file_utils::caption_kinds();
    let labels: Vec<&str> = kinds.iter().map(|(_, t)| t.as_str()).collect();
    let group = adw::PreferencesGroup::builder()
        .description(gettext(
            "Captions are shown under file names in the grid view.",
        ))
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let titles = [gettext("First"), gettext("Second"), gettext("Third")];
    let rows: Vec<adw::ComboRow> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let chosen = current.get(i).map(String::as_str).unwrap_or("none");
            let row = adw::ComboRow::builder()
                .title(title)
                .model(&gtk::StringList::new(&labels))
                .selected(kinds.iter().position(|(k, _)| *k == chosen).unwrap_or(0) as u32)
                .build();
            group.add(&row);
            row
        })
        .collect();
    let keys: Vec<&'static str> = kinds.iter().map(|(k, _)| *k).collect();
    for row in &rows {
        let rows = rows.clone();
        let keys = keys.clone();
        let settings = settings.clone();
        row.connect_selected_notify(move |_| {
            let value: Vec<&str> = rows.iter().map(|r| keys[r.selected() as usize]).collect();
            let _ = settings.set_strv("captions", value.as_slice());
        });
    }
    let view = adw::ToolbarView::builder().content(&group).build();
    view.add_top_bar(&adw::HeaderBar::new());
    adw::Dialog::builder()
        .title(gettext("Captions"))
        .content_width(360)
        .child(&view)
        .build()
}
