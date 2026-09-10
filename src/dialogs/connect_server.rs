//! "Connect to Server": the address of a share, and the servers connected to before.

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, gio, glib, gtk};

/// Ask for a server address and mount it. Resolves to the location mounted, or None if the
/// dialog was dismissed. The dialog stays up while the address is being reached, so a name
/// that does not answer can be corrected where it was typed.
pub async fn connect_server_dialog(parent: &impl IsA<gtk::Widget>) -> Option<gio::File> {
    let schemes = crate::network::address_schemes();
    let entry = adw::EntryRow::builder()
        .title(gettext("Server Address"))
        .activates_default(true)
        .sensitive(!schemes.is_empty())
        .input_hints(gtk::InputHints::NO_SPELLCHECK)
        .build();
    let hint = gtk::Label::builder()
        .label(if schemes.is_empty() {
            gettext("gvfs is not installed, so no server can be reached.")
        } else {
            gettext("Addresses this system can open: %s")
                .replace("%s", &examples(&schemes).join(", "))
        })
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .margin_start(12)
        .margin_end(12)
        .build();
    let failure = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["error", "caption"])
        .margin_start(12)
        .margin_end(12)
        .build();

    let address_group = adw::PreferencesGroup::new();
    address_group.add(&entry);
    let recent = adw::PreferencesGroup::builder()
        .title(gettext("Recent Servers"))
        .visible(false)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    content.append(&address_group);
    content.append(&hint);
    content.append(&failure);
    content.append(&recent);

    let cancel = gtk::Button::builder()
        .label(gettext("_Cancel"))
        .use_underline(true)
        .build();
    let connect_label = gettext("C_onnect");
    let connect = gtk::Button::builder()
        .label(&connect_label)
        .use_underline(true)
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&connect);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&content));

    let dialog = adw::Dialog::builder()
        .title(gettext("Connect to Server"))
        .content_width(460)
        .child(&view)
        .build();
    dialog.set_default_widget(Some(&connect));

    // The address has to be one this system can reach before it is worth trying.
    entry.connect_changed(glib::clone!(
        #[weak]
        connect,
        #[weak]
        failure,
        move |entry| {
            connect.set_sensitive(crate::network::address(&entry.text()).is_some());
            failure.set_visible(false);
        }
    ));

    let (tx, rx) = oneshot::channel::<Option<gio::File>>();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let start = Rc::new(glib::clone!(
        #[weak]
        entry,
        #[weak]
        connect,
        #[weak]
        recent,
        #[weak]
        failure,
        #[weak]
        dialog,
        #[strong]
        connect_label,
        #[strong]
        tx,
        move |address: String| {
            let Some(file) = crate::network::address(&address) else {
                return;
            };
            // Reaching a server takes as long as it takes; the dialog says so and takes
            // no second address, from the button or from the list, until this one has
            // answered.
            let label = connect_label.clone();
            connect.set_child(Some(&adw::Spinner::new()));
            connect.set_sensitive(false);
            entry.set_sensitive(false);
            recent.set_sensitive(false);
            failure.set_visible(false);
            glib::spawn_future_local(glib::clone!(
                #[strong]
                tx,
                async move {
                    let result = crate::network::mount(&file, &dialog).await;
                    connect.set_label(&label);
                    connect.set_sensitive(true);
                    entry.set_sensitive(true);
                    recent.set_sensitive(true);
                    match result {
                        Ok(()) => {
                            crate::network::remember(&file.uri());
                            if let Some(tx) = tx.borrow_mut().take() {
                                let _ = tx.send(Some(file));
                            }
                            dialog.close();
                        }
                        // The password dialog was dismissed: nothing to report, the
                        // address is still there to try again.
                        Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                        Err(e) => {
                            failure.set_label(e.message());
                            failure.set_visible(true);
                            // Focus alone would select the whole address; an address that
                            // did not answer is corrected at its end more often than not.
                            entry.grab_focus();
                            entry.set_position(-1);
                        }
                    }
                }
            ));
        }
    ));

    connect.connect_clicked(glib::clone!(
        #[weak]
        entry,
        #[strong]
        start,
        move |_| start(entry.text().to_string())
    ));
    fill_recent(&recent, &entry, &start);

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        tx,
        move |_| {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(None);
            }
        }
    ));

    dialog.present(Some(parent));
    entry.grab_focus();
    rx.await.ok().flatten()
}

/// What an address looks like, one per protocol this system can use. The secure variant
/// of a protocol is typed the way the plain one is, `davs://` like `dav://`, and is not
/// named twice.
fn examples(schemes: &[&str]) -> Vec<&'static str> {
    const SHAPES: [(&[&str], &str); 7] = [
        (&["smb"], "smb://server/share"),
        (&["sftp", "ssh"], "sftp://user@host"),
        (&["ftp", "ftps", "ftpis"], "ftp://host"),
        (&["nfs"], "nfs://host/export"),
        (&["dav", "davs"], "dav://host/path"),
        (&["afp"], "afp://server/volume"),
        (&["http", "https"], "https://host/path"),
    ];
    SHAPES
        .iter()
        .filter(|(family, _)| family.iter().any(|s| schemes.contains(s)))
        .map(|(_, shape)| *shape)
        .collect()
}

/// The servers connected to before, each a row that connects again and a button that

/// takes it off the list. The group is not there while the list is empty.
fn fill_recent(
    group: &adw::PreferencesGroup,
    entry: &adw::EntryRow,
    start: &Rc<impl Fn(String) + 'static>,
) {
    let servers = crate::network::servers();
    group.set_visible(!servers.is_empty());
    for uri in servers {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&uri))
            .activatable(true)
            .build();
        row.connect_activated(glib::clone!(
            #[weak]
            entry,
            #[strong]
            start,
            #[strong]
            uri,
            move |_| {
                entry.set_text(&uri);
                start(uri.clone());
            }
        ));
        let forget = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text(gettext("Forget This Server"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        forget.connect_clicked(glib::clone!(
            #[weak]
            group,
            #[weak]
            row,
            #[strong]
            uri,
            move |_| {
                crate::network::forget(&uri);
                group.remove(&row);
                group.set_visible(!crate::network::servers().is_empty());
            }
        ));
        row.add_suffix(&forget);
        group.add(&row);
    }
}
