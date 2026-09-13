//! Drag and drop on the sidebar: files dropped on places, bookmarks and the trash, and rows
//! dragged into another order.

use super::*;

pub(super) async fn is_dir_future(file: &gio::File) -> bool {
    file.query_info_future(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .is_ok_and(|info| info.file_type() == gio::FileType::Directory)
}

/// Drag payload for reordering bookmarks.
#[derive(Clone, glib::Boxed)]
#[boxed_type(name = "SpiralBookmarkDrag")]
pub(super) struct BookmarkDrag(pub(super) String);

/// Drag payload for reordering tags: the tag's name.
#[derive(Clone, glib::Boxed)]
#[boxed_type(name = "SpiralTagDrag")]
pub(super) struct TagDrag(pub(super) String);

/// Bookmark rows can be dragged among themselves; dropping on one inserts before or after it.
pub(super) fn add_bookmark_dnd(row: &gtk::ListBoxRow, file: &gio::File) {
    let file = file.clone();
    add_reorder_dnd(row, BookmarkDrag(file.uri().into()), move |drag, after| {
        crate::bookmarks::move_to(&gio::File::for_uri(&drag.0), Some((&file, after)));
    });
}

/// Rows of one section can be dragged among themselves: a drag from `row` carries `payload`,
/// and `move_to` is given what was dropped on `row` and whether it goes after it.
pub(super) fn add_reorder_dnd<T: glib::value::ValueType>(
    row: &gtk::ListBoxRow,
    payload: T,
    move_to: impl Fn(&T, bool) + 'static,
) {
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();
    source
        .connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    source.connect_drag_begin(glib::clone!(
        #[weak]
        row,
        move |src, _| {
            let paintable = gtk::WidgetPaintable::new(Some(&row));
            src.set_icon(Some(&paintable), 0, row.height() / 2);
        }
    ));
    row.add_controller(source);

    let target = gtk::DropTarget::new(T::Type::static_type(), gdk::DragAction::MOVE);
    target.connect_drop(glib::clone!(
        #[weak]
        row,
        #[upgrade_or]
        false,
        move |_, value, _, y| {
            let Ok(drag) = value.get::<T>() else {
                return false;
            };
            move_to(&drag, y > f64::from(row.height()) / 2.0);
            if let Some(sidebar) = row.ancestor(PlacesSidebar::static_type()) {
                sidebar.downcast::<PlacesSidebar>().unwrap().rebuild();
            }
            true
        }
    ));
    row.add_controller(target);
}

/// Files dropped on a sidebar row are copied/moved into that location.
pub(super) fn add_drop_target(row: &gtk::ListBoxRow, file: &gio::File) {
    if crate::starred::is_starred_location(file) {
        return;
    }
    if file.uri().starts_with("trash:") {
        add_trash_drop_target(row);
        return;
    }
    // The network is a list of machines, not a place anything can be put.
    if file.uri().starts_with("network:") {
        return;
    }
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.connect_enter(|t, _, _| crate::browser_view::preferred_action(t));
    target.connect_motion(|t, _, _| crate::browser_view::preferred_action(t));
    let hovered = file.clone();
    let file = file.clone();
    target.connect_drop(glib::clone!(
        #[weak]
        row,
        #[upgrade_or]
        false,
        move |t, value, x, y| {
            let view = row
                .root()
                .and_downcast::<crate::window::SpiralWindow>()
                .and_then(|w| w.current_view());
            match view {
                Some(v) => v.drop_files(t, value, &file, x, y),
                None => false,
            }
        }
    ));
    row.add_controller(target);
    crate::browser_view::open_on_hover(
        row,
        glib::clone!(
            #[weak]
            row,
            move || {
                if let Some(sidebar) = row
                    .ancestor(PlacesSidebar::static_type())
                    .and_downcast::<PlacesSidebar>()
                {
                    sidebar.emit_by_name::<()>("open-location", &[&hovered, &false]);
                }
            }
        ),
    );
}

/// Files dropped on Trash are trashed. Nothing is asked first: unlike a folder, there is
/// only one thing a drop on the trash can mean, and undo puts them back.
///
/// A copy is accepted as readily as a move, and the action is answered from what the drag
/// offers: a target that only takes `MOVE` refuses every drag that has settled on a copy,
/// which is what a drag from another application arrives as.
pub(super) fn add_trash_drop_target(row: &gtk::ListBoxRow) {
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.connect_enter(|t, _, _| trash_drop_action(t));
    target.connect_motion(|t, _, _| trash_drop_action(t));
    target.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let files = list.files();
        let app =
            gio::Application::default().and_downcast::<crate::application::SpiralApplication>();
        let (Some(app), false) = (app, files.is_empty()) else {
            return false;
        };
        app.job_manager()
            .submit(crate::ops::JobKind::Trash { files });
        true
    });
    row.add_controller(target);
}

/// Move where the drag offers one, a copy otherwise: the files are trashed either way, and
/// the source is told what it can act on.
pub(super) fn trash_drop_action(target: &gtk::DropTarget) -> gdk::DragAction {
    let offered = target
        .current_drop()
        .map(|drop| drop.actions())
        .unwrap_or(gdk::DragAction::MOVE);
    if offered.contains(gdk::DragAction::MOVE) {
        gdk::DragAction::MOVE
    } else {
        gdk::DragAction::COPY
    }
}
