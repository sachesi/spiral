//! "Select Items Matching" dialog: a shell pattern, matched against the names in the view.

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, glib, gtk};

/// Ask for a pattern. Resolves to it, or None if the dialog was dismissed.
pub async fn select_pattern_dialog(parent: &impl IsA<gtk::Widget>) -> Option<String> {
    let label = gtk::Label::builder()
        .label(gettext("Pattern"))
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let entry = gtk::Entry::builder()
        .hexpand(true)
        .activates_default(true)
        .input_hints(gtk::InputHints::NO_SPELLCHECK)
        .build();
    entry.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
    let example = gtk::Label::builder()
        .xalign(0.0)
        .use_markup(true)
        // Translators: the examples are patterns, and are not translated.
        .label(format!(
            "{} <i>*.png, file??.txt, pict*.???</i>",
            gettext("Examples: ")
        ))
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&label);
    content.append(&entry);
    content.append(&example);

    let cancel = gtk::Button::builder()
        .label(gettext("_Cancel"))
        .use_underline(true)
        .can_shrink(true)
        .build();
    let select = gtk::Button::builder()
        .label(gettext("_Select"))
        .use_underline(true)
        .can_shrink(true)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&select);
    let toolbar = adw::ToolbarView::builder().content(&content).build();
    toolbar.add_top_bar(&header);
    let dialog = adw::Dialog::builder()
        .title(gettext("Select Items Matching"))
        .content_width(400)
        .child(&toolbar)
        .focus_widget(&entry)
        .build();
    dialog.set_default_widget(Some(&select));

    let (tx, rx) = oneshot::channel::<Option<String>>();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let accept = glib::clone!(
        #[weak]
        entry,
        #[weak]
        dialog,
        #[strong]
        tx,
        move || {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(Some(entry.text().to_string()));
            }
            dialog.close();
        }
    );
    select.connect_clicked(glib::clone!(
        #[strong]
        accept,
        move |_| accept()
    ));
    entry.connect_activate(move |_| accept());
    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.connect_closed(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(None);
        }
    });
    dialog.present(Some(parent));
    rx.await.ok().flatten()
}
