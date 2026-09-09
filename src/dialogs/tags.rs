//! The dialog a new tag is made in: a name and a colour.

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::browser_view::tag_dot;
use crate::tags::Tag;
use crate::{adw, gdk, glib, gtk};

/// Ask for a name and a colour. Resolves to the tag, or None if the dialog was dismissed.
/// A name is refused while it is empty, has a comma in it or is a tag already.
pub async fn new_tag_dialog(parent: &impl IsA<gtk::Widget>) -> Option<Tag> {
    let entry = gtk::Entry::builder()
        .placeholder_text(gettext("Name"))
        .activates_default(true)
        .input_hints(gtk::InputHints::NO_SPELLCHECK)
        .build();
    let on_pick: PickHook = Rc::new(RefCell::new(None));
    let (colors, chosen) = color_picker(parent, "", on_pick.clone());
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
    // A tag is a name and a colour: the button waits for both.
    let ready = glib::clone!(
        #[weak]
        dialog,
        #[weak]
        entry,
        #[strong]
        chosen,
        move || {
            let name = entry.text();
            dialog.set_response_enabled(
                "add",
                crate::tags::valid_name(&name)
                    && !crate::tags::exists(name.trim())
                    && !chosen.borrow().is_empty(),
            );
        }
    );
    entry.connect_changed(glib::clone!(
        #[strong]
        ready,
        move |_| ready()
    ));
    on_pick.replace(Some(Rc::new(ready)));
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

/// Told each time the pick changes.
type PickHook = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The colours as a row of dots to pick from, `selected` picked, then a dot for a colour
/// of the user's own that opens the colour dialog. The cell follows the pick.
fn color_picker(
    parent: &impl IsA<gtk::Widget>,
    selected: &str,
    on_pick: PickHook,
) -> (gtk::Box, Rc<RefCell<String>>) {
    let chosen = Rc::new(RefCell::new(selected.to_string()));
    let picked: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(f) = on_pick.borrow().as_ref() {
            f();
        }
    });
    let row = gtk::Box::builder()
        .spacing(2)
        .halign(gtk::Align::Center)
        .css_classes(["spiral-tag-picker"])
        .build();
    let mut group: Option<gtk::ToggleButton> = None;
    let custom = crate::tags::is_custom(selected);
    for color in crate::tags::COLORS {
        let dot = tag_dot(color);
        let button = gtk::ToggleButton::builder()
            .child(&dot)
            .tooltip_text(crate::tags::color_name(color))
            .active(color == selected)
            .css_classes(["flat", "circular"])
            .build();
        unsafe { button.set_data("color", color.to_string()) };
        button.set_group(group.as_ref());
        button.connect_toggled(glib::clone!(
            #[strong]
            chosen,
            #[strong]
            picked,
            move |b| {
                dot.set_icon_name(b.is_active().then_some("object-select-symbolic"));
                if b.is_active() {
                    chosen.replace(color.to_string());
                    picked();
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
    // The custom dot is painted by a rule of its own, since its colour is not in the
    // stylesheet until the tag is saved; a colour from the dialog rewrites the rule.
    let css = gtk::CssProvider::new();
    let paint = glib::clone!(
        #[weak]
        css,
        move |color: &str| css.load_from_string(&crate::tags::css_for(color))
    );
    let dot = tag_dot(if custom { selected } else { "" });
    if custom {
        paint(selected);
        dot.set_icon_name(Some("object-select-symbolic"));
    } else {
        dot.set_css_classes(&["spiral-tag-dot", "spiral-tag-custom"]);
    }
    let button = gtk::ToggleButton::builder()
        .child(&dot)
        .tooltip_text(gettext("Custom…"))
        .active(custom)
        .css_classes(["flat", "circular"])
        .build();
    button.set_group(group.as_ref());
    button.connect_clicked(glib::clone!(
        #[strong]
        chosen,
        #[strong]
        picked,
        #[weak(rename_to = parent)]
        parent.as_ref(),
        #[weak]
        row,
        move |_| {
            let start = chosen.borrow().clone();
            let start = if crate::tags::is_custom(&start) {
                gdk::RGBA::parse(&start).ok()
            } else {
                None
            };
            let dialog = gtk::ColorDialog::builder()
                .with_alpha(false)
                .title(gettext("Tag Colour"))
                .build();
            let (dot, paint, chosen, picked) =
                (dot.clone(), paint.clone(), chosen.clone(), picked.clone());
            glib::spawn_future_local(async move {
                let window = parent.root().and_downcast::<gtk::Window>();
                if let Ok(rgba) = dialog
                    .choose_rgba_future(window.as_ref(), start.as_ref())
                    .await
                {
                    let color = crate::tags::hex(&rgba);
                    paint(&color);
                    dot.set_css_classes(&["spiral-tag-dot", &crate::tags::dot_class(&color)]);
                    dot.set_icon_name(Some("object-select-symbolic"));
                    chosen.replace(color);
                    picked();
                } else if !crate::tags::is_custom(&chosen.borrow()) {
                    // Nothing was picked: back to the dot that was picked before.
                    let before = chosen.borrow().clone();
                    let mut child = row.first_child();
                    while let Some(b) = child {
                        child = b.next_sibling();
                        let color =
                            unsafe { b.data::<String>("color").map(|p| p.as_ref().clone()) };
                        if color.as_deref() == Some(before.as_str())
                            && let Some(b) = b.downcast_ref::<gtk::ToggleButton>()
                        {
                            b.set_active(true);
                        }
                    }
                }
            });
        }
    ));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        row.connect_destroy(move |_| {
            gtk::style_context_remove_provider_for_display(&display, &css);
        });
    }
    row.append(&button);
    (row, chosen)
}
