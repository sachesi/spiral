//! Favorites and colour tags on the selection, and the row of tag dots in the menu.

use super::*;

/// Where a selection stands with a tag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TagState {
    /// On every selected file.
    All,
    /// On some of them.
    Some,
    None,
}

/// A file name shown as a menu label: an underscore in it is a character, not a mnemonic.
pub(super) fn mnemonic_safe(name: &str) -> String {
    name.replace('_', "__")
}

impl BrowserView {
    pub(super) fn set_selection_starred(&self, starred: bool) {
        let infos: Vec<gio::FileInfo> = self
            .model()
            .selected_infos()
            .into_iter()
            .filter(|i| crate::starred::is_starred(&file_utils::file_of(i)) != starred)
            .collect();
        for info in &infos {
            crate::starred::set_starred(&file_utils::file_of(info), starred);
        }
        self.update_action_state();
        self.refresh_cells();
        if !starred {
            self.offer_to_star_again(&infos);
        }
    }

    /// Unstarred in Favorites, items leave the view on the spot; a toast can bring them
    /// back.
    pub(crate) fn offer_to_star_again(&self, infos: &[gio::FileInfo]) {
        let in_favorites = self
            .location()
            .is_some_and(|l| crate::starred::is_starred_location(&l));
        let Some(win) = self.root().and_downcast::<crate::window::SpiralWindow>() else {
            return;
        };
        if !in_favorites || infos.is_empty() {
            return;
        }
        let message = match infos {
            [info] => gettext("Removed “%s” from Favorites").replace("%s", &info.display_name()),
            _ => ngettext(
                "Removed %d item from Favorites",
                "Removed %d items from Favorites",
                infos.len() as u32,
            )
            .replace("%d", &infos.len().to_string()),
        };
        let files: Vec<gio::File> = infos.iter().map(file_utils::file_of).collect();
        win.show_undo_toast(&message, move || {
            for file in &files {
                crate::starred::set_starred(file, true);
            }
        });
    }

    /// How the selection stands with a tag: on every file, on some, or on none.
    pub(super) fn tag_state(&self, name: &str) -> TagState {
        let infos = self.model().selected_infos();
        let with = infos
            .iter()
            .filter(|i| crate::tags::of_info(i).iter().any(|t| t == name))
            .count();
        match with {
            0 => TagState::None,
            n if n == infos.len() => TagState::All,
            _ => TagState::Some,
        }
    }

    /// Whether the selection can be tagged: tags are on, and the files are local, since
    /// the extended attribute is the local filesystems' to keep, and not in the trash.
    pub(super) fn can_tag(&self) -> bool {
        let infos = self.model().selected_infos();
        crate::tags::enabled()
            && !self.chooser_mode()
            && !infos.is_empty()
            && infos.iter().all(|i| {
                let file = file_utils::file_of(i);
                file.is_native() && !file.uri().starts_with("trash:")
            })
    }

    /// Put a tag on the whole selection or take it off the whole selection. The infos
    /// on screen are told as well: the folder monitor will say the same a moment later,
    /// but the dots should not wait for it.
    pub(crate) fn set_selection_tag(&self, name: &str, on: bool) {
        let infos = self.model().selected_infos();
        let files: Vec<gio::File> = infos.iter().map(file_utils::file_of).collect();
        let name = name.to_string();
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            let (done, failed) = crate::tags::set(&files, &name, on).await;
            for (info, names) in infos.iter().zip(&done) {
                match crate::tags::attribute_value(names) {
                    Some(v) => info.set_attribute_string(crate::tags::ATTRIBUTE, &v),
                    None => info.remove_attribute(crate::tags::ATTRIBUTE),
                }
            }
            let Some(view) = view.upgrade() else { return };
            if let Some(e) = failed {
                // The reason is nearly always that the filesystem keeps no extended
                // attributes, which GIO says at length; the log gets its wording.
                let info = &infos[done.len()];
                glib::g_debug!("spiral", "cannot tag {}: {e}", files[done.len()].uri());
                if let Some(win) = view.root().and_downcast::<crate::window::SpiralWindow>() {
                    win.show_toast(
                        &gettext("Could not tag “%s”").replace("%s", &info.display_name()),
                        false,
                    );
                }
            }
            view.refresh_cells();
        });
    }

    /// The tags section of the item menu: the coloured tags as a row of dots to click,
    /// while tags are on and the selection can take one; nothing otherwise. It is only
    /// touched when that changes: the popover loses the slot the dots go in when the
    /// item is taken out and put back, and would not take them again.
    pub(super) fn sync_tags_menu(&self) {
        let section = &self.imp().tags_section;
        let picker = self.can_tag() && crate::tags::all().iter().any(|t| !t.color.is_empty());
        if section.n_items() == i32::from(picker) {
            return;
        }
        while section.n_items() > 0 {
            section.remove(0);
        }
        if picker {
            let item = gio::MenuItem::new(None, None);
            item.set_attribute_value("custom", Some(&"tags".to_variant()));
            section.append_item(&item);
        }
    }

    /// The row of dots for the menu on show: one per coloured tag, marked where the
    /// selection carries it. Built once and kept; the popover lets go of it whenever it
    /// builds a menu again, and it goes back in the slot the section leaves for it.
    pub(super) fn attach_tag_picker(&self) {
        let imp = self.imp();
        if !self.can_tag() {
            return;
        }
        let existing = imp.tag_picker.borrow().clone();
        let picker = existing.unwrap_or_else(|| {
            let bx = gtk::Box::builder()
                .spacing(2)
                .css_classes(["spiral-tag-picker"])
                .build();
            imp.tag_picker.replace(Some(bx.clone()));
            bx
        });
        while let Some(child) = picker.first_child() {
            picker.remove(&child);
        }
        for tag in crate::tags::all().iter().filter(|t| !t.color.is_empty()) {
            let dot = crate::browser_view::tag_dot(&tag.color);
            let button = gtk::Button::builder()
                .child(&dot)
                .tooltip_text(&tag.name)
                .css_classes(["flat", "circular"])
                .build();
            TAG.set(&button, tag.name.clone());
            let name = tag.name.clone();
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| {
                    // The menu goes first: the rows are rewritten under it otherwise.
                    if let Some(p) = view.imp().popover.borrow().as_ref() {
                        p.popdown();
                    }
                    let on = !matches!(view.tag_state(&name), TagState::All);
                    view.set_selection_tag(&name, on);
                }
            ));
            picker.append(&button);
        }
        self.sync_tag_picker();
        if picker.parent().is_none()
            && let Some(popover) = imp.popover.borrow().as_ref()
        {
            popover.add_child(&picker, "tags");
        }
    }

    /// Mark each dot of the picker as the selection stands with its tag.
    pub(super) fn sync_tag_picker(&self) {
        let Some(picker) = self.imp().tag_picker.borrow().clone() else {
            return;
        };
        let mut child = picker.first_child();
        while let Some(button) = child {
            child = button.next_sibling();
            let (Some(name), Some(dot)) = (TAG.get(&button), button.first_child()) else {
                continue;
            };
            let state = self.tag_state(&name);
            dot.set_css_classes(&[
                "spiral-tag-dot",
                &crate::tags::dot_class(&crate::tags::color_of(&name).unwrap_or_default()),
            ]);
            if state == TagState::Some {
                dot.add_css_class("spiral-tag-some");
            }
            let image = dot.downcast_ref::<gtk::Image>().unwrap();
            image.set_icon_name((state != TagState::None).then_some("object-select-symbolic"));
        }
    }
}
