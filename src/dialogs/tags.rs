//! The tags of a selection by name, with a box each, and the dialog a new tag is made in.

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::browser_view::tag_dot;
use crate::tags::Tag;
use crate::{adw, glib, gtk};

/// What puts a tag on the selection or takes it off.
type Apply = Rc<dyn Fn(&str, bool)>;

/// Where a selection stands with a tag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TagState {
    /// On every selected file.
    All,
    /// On some of them.
    Some,
    None,
}

/// Every tag on offer, and `carried` besides: tags the selection has that are not on
/// offer, which is what a file tagged by another program may bring. `state` says where
/// the selection stands with a tag, `apply` puts it on or takes it off.
pub fn tags_dialog(
    parent: &impl IsA<gtk::Widget>,
    carried: Vec<String>,
    state: impl Fn(&str) -> TagState + 'static,
    apply: impl Fn(&str, bool) + 'static,
) {
    let state = Rc::new(state);
    let apply: Apply = Rc::new(apply);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let fill = glib::clone!(
        #[weak]
        list,
        #[strong]
        state,
        #[strong]
        apply,
        move || {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let known = crate::tags::all();
            let extra = carried
                .iter()
                .filter(|n| !known.iter().any(|t| t.name == **n))
                .map(|n| Tag {
                    name: n.clone(),
                    color: String::new(),
                });
            for tag in known.iter().cloned().chain(extra) {
                list.append(&tag_row(&tag, state(&tag.name), apply.clone()));
            }
        }
    );
    fill();

    let new_tag = gtk::Button::builder()
        .label(gettext("New _Tag…"))
        .use_underline(true)
        .halign(gtk::Align::Start)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&list);
    content.append(&new_tag);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    let toolbar = adw::ToolbarView::builder().content(&scroll).build();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let dialog = adw::Dialog::builder()
        .title(gettext("Tags"))
        .content_width(360)
        .content_height(420)
        .child(&toolbar)
        .build();
    new_tag.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[strong]
        fill,
        move |_| {
            glib::spawn_future_local(glib::clone!(
                #[weak]
                dialog,
                #[strong]
                apply,
                #[strong]
                fill,
                async move {
                    if let Some(tag) = new_tag_dialog(&dialog).await {
                        apply(&tag.name, true);
                        crate::tags::add(tag);
                        fill();
                    }
                }
            ));
        }
    ));
    dialog.present(Some(parent));
}

fn tag_row(tag: &Tag, state: TagState, apply: Apply) -> adw::ActionRow {
    let check = gtk::CheckButton::builder()
        .valign(gtk::Align::Center)
        .active(state == TagState::All)
        .inconsistent(state == TagState::Some)
        .build();
    let row = adw::ActionRow::builder()
        .title(tag.name.as_str())
        .use_markup(false)
        .activatable_widget(&check)
        .build();
    row.add_prefix(&tag_dot(&tag.color));
    row.add_suffix(&check);
    let name = tag.name.clone();
    check.connect_toggled(move |check| {
        check.set_inconsistent(false);
        apply(&name, check.is_active());
    });
    row
}

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
