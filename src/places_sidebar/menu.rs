//! The menu of a row: renaming bookmarks and tags, their colours, and removing them.

use super::*;

impl PlacesSidebar {
    pub(super) fn popup_row_menu(&self, row: &gtk::ListBoxRow, x: f64, y: f64) {
        let imp = self.imp();
        imp.menu_row.replace(Some(row.clone()));
        let enable = |name: &str, on: bool| {
            if let Some(a) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_enabled(on);
            }
        };
        let bookmark = row_bookmark(row).is_some();
        let tag = row_tag(row);
        let window = !imp.in_dialog.get();
        enable("open-new-tab", window && row_file(row).is_some());
        enable("rename", bookmark || tag.is_some());
        enable("remove", bookmark || tag.is_some());
        enable("new-tag", crate::tags::all().len() < crate::tags::MAX);
        let leaving = row_eject(row);
        enable("eject", leaving.as_ref().is_some_and(|t| !t.is_network()));
        enable(
            "disconnect",
            leaving.as_ref().is_some_and(EjectTarget::is_network),
        );
        enable(
            "empty-trash",
            window
                && !imp.trash_empty.get()
                && row_file(row).is_some_and(|f| f.uri().starts_with("trash:")),
        );
        let existing = imp.popover.borrow().clone();
        let popover = existing.unwrap_or_else(|| {
            let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
            p.set_parent(self);
            p.set_has_arrow(false);
            p.set_halign(gtk::Align::Start);
            imp.popover.replace(Some(p.clone()));
            p
        });
        let model: &gio::MenuModel = if tag.is_some() {
            &imp.tag_menu
        } else {
            &imp.row_menu
        };
        popover.set_menu_model(Some(model));
        if let Some(tag) = &tag {
            self.attach_color_picker(&popover, tag);
        }
        let p = imp
            .list
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
        popover.popup();
    }

    /// The colours a tag can have, as a row of dots with the one it carries marked, and
    /// a dot of its own for a colour from the colour dialog. The popover holds on to the
    /// row it was given once, so the dots are made again for the tag whose menu is on the
    /// way up.
    pub(super) fn attach_color_picker(&self, popover: &gtk::PopoverMenu, tag: &str) {
        let imp = self.imp();
        let selected = crate::tags::color_of(tag).unwrap_or_default();
        let existing = imp.color_picker.borrow().clone();
        let row = existing.unwrap_or_else(|| {
            let bx = gtk::Box::builder()
                .spacing(2)
                .css_classes(["spiral-tag-picker"])
                .build();
            imp.color_picker.replace(Some(bx.clone()));
            bx
        });
        while let Some(dot) = row.first_child() {
            row.remove(&dot);
        }
        let button = |dot: &gtk::Image, name: &str| {
            gtk::Button::builder()
                .child(dot)
                .tooltip_text(name)
                .css_classes(["flat", "circular"])
                .build()
        };
        for color in crate::tags::COLORS {
            let dot = crate::browser_view::tag_dot(color);
            if color == selected {
                dot.set_icon_name(Some("object-select-symbolic"));
            }
            let dot = button(&dot, &crate::tags::color_name(color));
            dot.connect_clicked(glib::clone!(
                #[weak(rename_to = sidebar)]
                self,
                #[weak]
                popover,
                move |_| {
                    // The menu goes first: the rows are rewritten under it otherwise.
                    popover.popdown();
                    let _ = sidebar.activate_action("sidebar.tag-color", Some(&color.to_variant()));
                }
            ));
            row.append(&dot);
        }
        let custom = crate::tags::is_custom(&selected);
        let dot = crate::browser_view::tag_dot(if custom { &selected } else { "" });
        if custom {
            dot.set_icon_name(Some("object-select-symbolic"));
        } else {
            dot.set_css_classes(&["spiral-tag-dot", "spiral-tag-custom"]);
        }
        let dot = button(&dot, &gettext("Custom…"));
        dot.connect_clicked(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            #[weak]
            popover,
            move |_| {
                popover.popdown();
                let _ = sidebar.activate_action("sidebar.tag-custom-color", None);
            }
        ));
        row.append(&dot);
        if row.parent().is_none() {
            popover.add_child(&row, "colors");
        }
    }

    /// Entry popover over `row` for the bookmark's label; empty restores the folder name.
    pub(super) fn rename_bookmark(&self, row: &gtk::ListBoxRow, file: &gio::File) {
        let label = crate::bookmarks::load()
            .into_iter()
            .find(|(f, _)| f.equal(file))
            .and_then(|(_, l)| l)
            .unwrap_or_else(|| crate::file_utils::location_name(file));
        let file = file.clone();
        self.rename_row(
            row,
            &label,
            |_| true,
            move |name| crate::bookmarks::rename(&file, name),
        );
    }

    /// The same popover for a tag. The name has to be one a tag can have and not one
    /// another tag has; the tag's own name is fine, and changes nothing.
    pub(super) fn rename_tag(&self, row: &gtk::ListBoxRow, tag: &str) {
        let old = tag.to_string();
        self.rename_row(
            row,
            tag,
            glib::clone!(
                #[strong]
                old,
                move |name| {
                    crate::tags::valid_name(name)
                        && (name.trim() == old || !crate::tags::exists(name.trim()))
                }
            ),
            move |name| {
                let name = name.trim();
                if name != old {
                    crate::tags::rename(&old, name);
                }
            },
        );
    }

    /// A tag is taken off everything that carries it when it goes, so ask first.
    pub(super) fn remove_tag(&self, tag: String) {
        let n = crate::tags::carriers(&tag) as u32;
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Remove Tag “%s”?").replace("%s", &tag))
            .body(if n == 0 {
                gettext("No file carries it.")
            } else {
                ngettext(
                    "It will be taken off the %d file that carries it.",
                    "It will be taken off the %d files that carry it.",
                    n,
                )
                .replace("%d", &n.to_string())
            })
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("remove", &gettext("_Remove")),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                if dialog.choose_future(Some(&sidebar)).await == "remove" {
                    crate::tags::remove(&tag);
                }
            }
        ));
    }

    /// Entry popover over `row` with `label` in it. `valid` says whether what is typed
    /// may be accepted, `accept` takes it.
    pub(super) fn rename_row(
        &self,
        row: &gtk::ListBoxRow,
        label: &str,
        valid: impl Fn(&str) -> bool + 'static,
        accept: impl Fn(&str) + 'static,
    ) {
        let entry = gtk::Entry::builder().text(label).build();
        let button = gtk::Button::builder()
            .label(gettext("_Rename"))
            .use_underline(true)
            .css_classes(["suggested-action"])
            .build();
        let bx = gtk::Box::builder()
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        bx.append(&entry);
        bx.append(&button);
        let bounds = row
            .compute_bounds(self)
            .map(|b| {
                gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    b.width() as i32,
                    b.height() as i32,
                )
            })
            .unwrap_or_else(|| gdk::Rectangle::new(0, 0, 1, 1));
        let popover = gtk::Popover::builder()
            .child(&bx)
            .pointing_to(&bounds)
            .build();
        popover.set_parent(self);
        let valid = std::rc::Rc::new(valid);
        entry.connect_changed(glib::clone!(
            #[weak]
            button,
            #[strong]
            valid,
            move |entry| button.set_sensitive(valid(&entry.text()))
        ));
        let accept = std::rc::Rc::new(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            #[weak]
            entry,
            #[weak]
            popover,
            move || {
                if !valid(&entry.text()) {
                    return;
                }
                accept(&entry.text());
                popover.popdown();
                sidebar.rebuild();
            }
        ));
        button.connect_clicked(glib::clone!(
            #[strong]
            accept,
            move |_| accept()
        ));
        entry.connect_activate(move |_| accept());
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
        entry.grab_focus();
    }
}
