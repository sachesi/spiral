//! "Open With…" dialog: pick an application for the selected files, optionally as the new default.

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::{adw, gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OpenWithDialog {
        pub files: std::cell::RefCell<Vec<gio::File>>,
        pub content_type: std::cell::RefCell<Option<String>>,
        pub list: gtk::ListBox,
        pub show_all: gtk::ToggleButton,
        pub set_default: gtk::CheckButton,
        pub open_button: gtk::Button,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OpenWithDialog {
        const NAME: &'static str = "SpiralOpenWithDialog";
        type Type = super::OpenWithDialog;
        type ParentType = adw::Dialog;
    }

    impl ObjectImpl for OpenWithDialog {}
    impl WidgetImpl for OpenWithDialog {}
    impl AdwDialogImpl for OpenWithDialog {}
}

glib::wrapper! {
    pub struct OpenWithDialog(ObjectSubclass<imp::OpenWithDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn app_of(row: &gtk::ListBoxRow) -> Option<gio::AppInfo> {
    unsafe { row.data::<gio::AppInfo>("app").map(|p| p.as_ref().clone()) }
}

impl OpenWithDialog {
    pub fn new(files: &[gio::File], content_type: Option<String>) -> Self {
        let dialog: Self = glib::Object::builder()
            .property("title", gettext("Open With"))
            .property("content-width", 420)
            .property("content-height", 520)
            .build();
        let imp = dialog.imp();
        imp.files.replace(files.to_vec());
        imp.content_type.replace(content_type);

        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .build();
        let cancel = gtk::Button::with_label(&gettext("_Cancel"));
        cancel.set_use_underline(true);
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        header.pack_start(&cancel);
        imp.open_button.set_label(&gettext("_Open"));
        imp.open_button.set_use_underline(true);
        imp.open_button.add_css_class("suggested-action");
        imp.open_button.set_sensitive(false);
        imp.open_button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                if let Some(row) = dialog.imp().list.selected_row() {
                    dialog.launch(&row);
                }
            }
        ));
        header.pack_end(&imp.open_button);

        imp.list.set_selection_mode(gtk::SelectionMode::Single);
        imp.list.add_css_class("boxed-list");
        imp.list.connect_row_selected(glib::clone!(
            #[weak]
            dialog,
            move |_, row| dialog.imp().open_button.set_sensitive(row.is_some())
        ));
        imp.list.connect_row_activated(glib::clone!(
            #[weak]
            dialog,
            move |_, row| dialog.launch(row)
        ));

        imp.show_all.set_label(&gettext("Show All Applications"));
        imp.show_all.add_css_class("flat");
        imp.show_all.connect_toggled(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.fill()
        ));
        imp.set_default
            .set_label(Some(&gettext("Always use for this file type")));
        imp.set_default
            .set_sensitive(imp.content_type.borrow().is_some());

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&imp.list)
            .build();
        content.append(&scroller);
        content.append(&imp.set_default);
        content.append(&imp.show_all);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        dialog.set_child(Some(&toolbar));
        dialog.fill();
        dialog
    }

    fn fill(&self) {
        let imp = self.imp();
        while let Some(c) = imp.list.first_child() {
            imp.list.remove(&c);
        }
        let ct = imp.content_type.borrow().clone();
        let mut apps = match (&ct, imp.show_all.is_active()) {
            (Some(ct), false) => gio::AppInfo::all_for_type(ct),
            _ => gio::AppInfo::all()
                .into_iter()
                .filter(|a| a.should_show())
                .collect(),
        };
        apps.sort_by_key(|a| a.display_name().to_lowercase());
        let default = ct
            .as_deref()
            .and_then(|c| gio::AppInfo::default_for_type(c, false));
        for app in apps {
            let bx = gtk::Box::builder()
                .spacing(12)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(6)
                .margin_end(6)
                .build();
            let image = gtk::Image::builder().pixel_size(32).build();
            if let Some(icon) = app.icon() {
                image.set_from_gicon(&icon);
            } else {
                image.set_icon_name(Some("application-x-executable"));
            }
            bx.append(&image);
            let name = gtk::Label::builder()
                .label(app.display_name())
                .xalign(0.0)
                .hexpand(true)
                .build();
            bx.append(&name);
            if default.as_ref().is_some_and(|d| d.equal(&app)) {
                bx.append(
                    &gtk::Label::builder()
                        .label(gettext("Default"))
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );
            }
            let row = gtk::ListBoxRow::builder().child(&bx).build();
            unsafe { row.set_data("app", app) };
            imp.list.append(&row);
        }
        if let Some(first) = imp.list.row_at_index(0) {
            imp.list.select_row(Some(&first));
        }
    }

    fn launch(&self, row: &gtk::ListBoxRow) {
        let imp = self.imp();
        let Some(app) = app_of(row) else { return };
        if imp.set_default.is_active()
            && let Some(ct) = imp.content_type.borrow().as_deref()
        {
            let _ = app.set_as_default_for_type(ct);
        }
        let files = imp.files.borrow().clone();
        let ctx = self.display().app_launch_context();
        // An application that asks for a terminal has to be given one; GIO refuses to
        // start those itself rather than go looking for a terminal emulator.
        let outcome = crate::terminal::launch_if_wanted(&app, &files)
            .unwrap_or_else(|| app.launch(&files, Some(&ctx)));
        if let Err(e) = outcome {
            let d = adw::AlertDialog::builder()
                .heading(gettext("Could Not Open"))
                .body(e.message())
                .build();
            d.add_response("ok", &gettext("_OK"));
            d.present(self.parent().as_ref());
        }
        self.close();
    }
}
