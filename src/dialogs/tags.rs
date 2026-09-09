//! The dialog a new tag is made in: a name and a colour.

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::browser_view::tag_dot;
use crate::tags::Tag;
use crate::{adw, glib, gtk};

/// Ask for a name and a colour. Resolves to the tag, or None if the dialog was dismissed.
/// A name is refused while it is empty, has a comma in it or is a tag already.
pub async fn new_tag_dialog(parent: &impl IsA<gtk::Widget>) -> Option<Tag> {
    let entry = gtk::Entry::builder()
        .placeholder_text(gettext("Name"))
        .activates_default(true)
        .input_hints(gtk::InputHints::NO_SPELLCHECK)
        .build();
    let (colors, chosen) = color_picker("");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&entry);
    content.append(&colors);
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("New Tag"))
        .extra_child(&content)
        .close_response("cancel")
        .default_response("add")
        .build();
    dialog.add_responses(&[("cancel", &gettext("_Cancel")), ("add", &gettext("_Add"))]);
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("add", false);
    entry.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |entry| {
            let name = entry.text();
            dialog.set_response_enabled(
                "add",
                crate::tags::valid_name(&name) && !crate::tags::exists(name.trim()),
            );
        }
    ));
    let (tx, rx) = oneshot::channel();
    let tx = RefCell::new(Some(tx));
    dialog.connect_response(None, move |_, response| {
        if let Some(tx) = tx.take() {
            let _ = tx.send(response.to_string());
        }
    });
    dialog.present(Some(parent));
    entry.grab_focus();
    if rx.await.ok()? != "add" {
        return None;
    }
    Some(Tag {
        name: entry.text().trim().to_string(),
        color: chosen.borrow().clone(),
    })
}

/// The colours as a row of dots to pick from, `selected` picked; the cell follows the pick.
fn color_picker(selected: &str) -> (gtk::Box, Rc<RefCell<String>>) {
    let chosen = Rc::new(RefCell::new(selected.to_string()));
    let row = gtk::Box::builder()
        .spacing(2)
        .halign(gtk::Align::Center)
        .css_classes(["spiral-tag-picker"])
        .build();
    let mut group: Option<gtk::ToggleButton> = None;
    for color in crate::tags::COLORS.iter().copied().chain([""]) {
        let dot = tag_dot(color);
        let button = gtk::ToggleButton::builder()
            .child(&dot)
            .tooltip_text(crate::tags::color_name(color))
            .active(color == selected)
            .css_classes(["flat", "circular"])
            .build();
        button.set_group(group.as_ref());
        button.connect_toggled(glib::clone!(
            #[strong]
            chosen,
            move |b| {
                dot.set_icon_name(b.is_active().then_some("object-select-symbolic"));
                if b.is_active() {
                    chosen.replace(color.to_string());
                }
            }
        ));
        if color == selected {
            button
                .child()
                .and_downcast::<gtk::Image>()
                .unwrap()
                .set_icon_name(Some("object-select-symbolic"));
        }
        group.get_or_insert(button.clone());
        row.append(&button);
    }
    (row, chosen)
}
