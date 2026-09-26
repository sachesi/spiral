//! The rows of the sidebar: how each kind is built and what it carries.

use super::*;

pub(super) fn row_file(row: &gtk::ListBoxRow) -> Option<gio::File> {
    ROW_FILE.get(row)
}

/// The file of a row from the GTK bookmarks file (not the fixed XDG folders).
pub(super) fn row_bookmark(row: &gtk::ListBoxRow) -> Option<gio::File> {
    let is_bookmark = BOOKMARK.has(row);
    is_bookmark.then(|| row_file(row)).flatten()
}

pub(super) fn row_eject(row: &gtk::ListBoxRow) -> Option<EjectTarget> {
    EJECT.get(row)
}

pub(super) fn row_volume(row: &gtk::ListBoxRow) -> Option<gio::Volume> {
    VOLUME.get(row)
}

pub(super) const SECTION_PLACES: u8 = 0;

pub(super) const SECTION_BOOKMARKS: u8 = 1;

pub(super) const SECTION_DEVICES: u8 = 2;

pub(super) const SECTION_NETWORK: u8 = 3;

pub(super) const SECTION_TAGS: u8 = 4;

pub(super) fn make_row(icon: &gio::Icon, title: &str, section: u8) -> (gtk::ListBoxRow, gtk::Box) {
    let image = gtk::Image::builder().gicon(icon).build();
    row_with(&image, title, section)
}

/// A row led by `leading`: an icon as a rule, a coloured dot for a tag.
pub(super) fn row_with(
    leading: &impl IsA<gtk::Widget>,
    title: &str,
    section: u8,
) -> (gtk::ListBoxRow, gtk::Box) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    leading.set_margin_end(8);
    content.append(leading);
    content.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .margin_end(2)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build(),
    );
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .focus_on_click(false)
        .build();
    SECTION.set(&row, section);
    (row, content)
}

pub(super) fn place_row(icon: &str, title: &str, file: &gio::File, section: u8) -> gtk::ListBoxRow {
    let (row, _) = make_row(&gio::ThemedIcon::new(icon).upcast(), title, section);
    ROW_FILE.set(&row, file.clone());
    add_drop_target(&row, file);
    row
}

/// A tag: its dot and its name, opening the list of what carries it. Files dropped on
/// it are given the tag.
pub(super) fn tag_row(tag: &crate::tags::Tag) -> gtk::ListBoxRow {
    // The dot in a box as wide as the icons of the rows above, so the names line up and
    // the dot stays round.
    let holder = gtk::Box::builder()
        .width_request(16)
        .halign(gtk::Align::Center)
        .build();
    holder.append(&crate::browser_view::tag_dot(&tag.color));
    let (row, _) = row_with(&holder, &tag.name, SECTION_TAGS);
    ROW_FILE.set(&row, crate::tags::location(&tag.name));
    ROW_TAG.set(&row, tag.name.clone());
    let anchor = tag.name.clone();
    add_reorder_dnd(&row, TagDrag(tag.name.clone()), move |drag, after| {
        crate::tags::move_to(&drag.0, Some((&anchor, after)));
    });
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let name = tag.name.clone();
    target.connect_drop(glib::clone!(
        #[weak]
        row,
        #[upgrade_or]
        false,
        move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let files = list.files();
            let name = name.clone();
            let row = row.downgrade();
            glib::spawn_future_local(async move {
                let (done, failed) = crate::tags::set(&files, &name, true).await;
                let Some(win) = row
                    .upgrade()
                    .and_then(|row| row.root())
                    .and_downcast::<crate::window::SpiralWindow>()
                else {
                    return;
                };
                if let Some(e) = failed {
                    let file = &files[done.len()];
                    glib::g_debug!("spiral", "cannot tag {}: {e}", file.uri());
                    win.show_toast(
                        &gettext("Could not tag “%s”").replace("%s", &crate::ops::name(file)),
                        false,
                    );
                }
                // The dots on the files are read as their cells are bound; make them look.
                if let Some(view) = win.current_view() {
                    view.reload();
                }
            });
            true
        }
    ));
    row.add_controller(target);
    row
}

/// The tag a row stands for, by name.
pub(super) fn row_tag(row: &gtk::ListBoxRow) -> Option<String> {
    ROW_TAG.get(row)
}

pub(super) fn row_section(row: &gtk::ListBoxRow) -> u8 {
    SECTION.get(row).unwrap_or(0)
}
