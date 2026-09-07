//! Miller columns: the folder chain as a strip of lists, one column per level.
//!
//! The last column is the folder being viewed. It shows `model()`, like the grid and the
//! list do, and it is the one the `view.*` actions, the context menus and drag and drop
//! work on. The columns before it are the path that leads there, the one after is what
//! the selected folder holds. Clicking in any of those goes to that folder, which makes
//! it the last column, the way the Finder moves between columns.

use std::time::Duration;

use crate::adw::prelude::*;
use crate::adw::subclass::prelude::*;
use crate::browser_view::{
    BrowserView, emblem_image, preferred_action, remember_list_item, set_cut, unbind_icon,
};
use crate::enums::ViewMode;
use crate::file_utils;
use crate::folder_model::FolderModel;
use crate::{gdk, gio, glib, gtk};

/// Width of one column, as in the Finder these come from.
const COLUMN_WIDTH: i32 = 240;
/// How long a selection has to hold still before the folder it points at is listed, so
/// that running down a folder full of folders does not open a listing per keystroke.
const PREVIEW_DELAY: Duration = Duration::from_millis(60);
/// How long marking the folder on the path may wait for its column to load, in 50 ms steps.
const MARK_TRIES: u32 = 40;
/// How long the strip keeps putting itself back at the far right after a change.
const SCROLL_WINDOW: Duration = Duration::from_millis(250);

/// A column that is not the folder being viewed: its own listing of `dir`.
pub struct SideColumn {
    model: FolderModel,
    root: gtk::ScrolledWindow,
}

/// Select `file` once the column showing it has listed enough to hold it.
fn mark_when_loaded(model: &FolderModel, list: &gtk::ListView, file: gio::File) {
    let (model, list) = (model.downgrade(), list.downgrade());
    glib::spawn_future_local(async move {
        for _ in 0..MARK_TRIES {
            {
                let (Some(model), Some(list)) = (model.upgrade(), list.upgrade()) else {
                    return;
                };
                if let Some(pos) = model.position_of(&file) {
                    model.selection().select_item(pos, true);
                    list.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
                    return;
                }
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
    });
}

impl BrowserView {
    /// The strip is built once; only the columns beside the current folder come and go.
    pub(crate) fn setup_miller(&self) {
        let imp = self.imp();
        imp.miller_scroll.set_width_request(COLUMN_WIDTH);
        imp.miller_list.set_model(Some(&imp.model.selection()));
        imp.miller_list
            .set_factory(Some(&self.miller_factory(true)));
        self.connect_location_notify(|view| view.schedule_columns());
        imp.columns_scroll
            .hadjustment()
            .connect_changed(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |adjustment| {
                    if glib::monotonic_time() >= view.imp().scroll_until.get() {
                        return;
                    }
                    // The strip is measured inside a layout pass, and moving it from there
                    // leaves the frame that is already being drawn behind; an idle is late
                    // enough for the move to be drawn.
                    let adjustment = adjustment.clone();
                    glib::idle_add_local_once(move || adjustment.set_value(adjustment.upper()));
                }
            ));
        imp.model
            .selection()
            .connect_selection_changed(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _, _| view.schedule_preview()
            ));
        // The columns beside the current one sort the way it does.
        for prop in ["sort-key", "sort-reversed"] {
            imp.model.connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_, _| view.sync_column_sort()
                ),
            );
        }
        self.setup_miller_keys();
    }

    /// Rebuild after the current change has been dealt with: navigating from a column
    /// throws that column away, and doing so while it is handling a click is too early.
    pub(crate) fn schedule_columns(&self) {
        if self.imp().columns_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().columns_pending.set(false);
                view.rebuild_columns();
            }
        ));
    }

    /// Draw the path as columns: one per folder from the root of the chain, then the
    /// folder being viewed, then whatever the selection points at.
    pub(crate) fn rebuild_columns(&self) {
        let imp = self.imp();
        self.clear_side_columns();
        if imp.view_mode.get() != ViewMode::Columns {
            return;
        }
        let Some(location) = self.location() else {
            return;
        };
        // Same chain the path bar draws, so the crumbs and the columns agree.
        let root = crate::path_bar::chain_root(&location, None);
        let mut chain = Vec::new();
        let mut current = location;
        while !current.equal(&root) {
            let Some(parent) = current.parent() else {
                break;
            };
            chain.push((parent.clone(), current));
            current = parent;
        }
        let mut previous: Option<gtk::Widget> = None;
        for (dir, child) in chain.into_iter().rev() {
            let column = self.side_column(&dir, Some(child));
            imp.columns_box
                .insert_child_after(&column.root, previous.as_ref());
            previous = Some(column.root.clone().upcast());
            imp.side_columns.borrow_mut().push(column);
        }
        self.update_preview_column();
        self.scroll_columns_to_end();
    }

    fn clear_side_columns(&self) {
        let imp = self.imp();
        for column in imp.side_columns.take() {
            imp.columns_box.remove(&column.root);
        }
        if let Some(column) = imp.preview_column.take() {
            imp.columns_box.remove(&column.root);
        }
    }

    /// Wait out the keystrokes before listing what the selection points at.
    pub(crate) fn schedule_preview(&self) {
        let imp = self.imp();
        if imp.view_mode.get() != ViewMode::Columns {
            return;
        }
        let generation = imp.preview_gen.get() + 1;
        imp.preview_gen.set(generation);
        glib::timeout_add_local_once(
            PREVIEW_DELAY,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    if view.imp().preview_gen.get() == generation {
                        view.update_preview_column();
                    }
                }
            ),
        );
    }

    /// The column past the current one lists the single selected folder, and is gone as
    /// soon as the selection is anything else.
    fn update_preview_column(&self) {
        let imp = self.imp();
        if let Some(column) = imp.preview_column.take() {
            imp.columns_box.remove(&column.root);
        }
        if imp.view_mode.get() != ViewMode::Columns {
            return;
        }
        let infos = imp.model.selected_infos();
        let [info] = infos.as_slice() else { return };
        if !file_utils::is_dir(info) {
            return;
        }
        let column = self.side_column(&file_utils::file_of(info), None);
        imp.columns_box
            .insert_child_after(&column.root, Some(&*imp.miller_scroll));
        imp.preview_column.replace(Some(column));
        self.scroll_columns_to_end();
    }

    /// A listing of `dir` of its own, with `mark` picked out as the folder on the path.
    fn side_column(&self, dir: &gio::File, mark: Option<gio::File>) -> SideColumn {
        let model = FolderModel::new(dir);
        model.set_sort_key(self.model().sort_key());
        model.set_sort_reversed(self.model().sort_reversed());
        self.imp()
            .settings
            .bind("show-hidden", &model, "show-hidden")
            .build();
        let list = gtk::ListView::builder()
            .model(&model.selection())
            .factory(&self.miller_factory(false))
            // One click is how a column view is walked, whatever opening a file takes.
            .single_click_activate(true)
            .build();
        list.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            model,
            #[strong]
            dir,
            move |_, pos| {
                let Some(info) = model.info_at(pos) else {
                    return;
                };
                let file = file_utils::file_of(&info);
                if file_utils::is_dir(&info) {
                    view.go_to(&file);
                } else {
                    // A file makes its own folder the one being viewed, with it picked out.
                    view.go_to(&dir);
                    view.select_files_when_loaded(vec![file]);
                }
            }
        ));
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(COLUMN_WIDTH)
            .css_classes(["spiral-miller-column"])
            .child(&list)
            .build();
        // Dropping on a column puts the files in the folder it lists.
        if !self.chooser_mode() {
            let target = gtk::DropTarget::new(
                gdk::FileList::static_type(),
                gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK,
            );
            target.connect_enter(|t, _, _| preferred_action(t));
            target.connect_motion(|t, _, _| preferred_action(t));
            target.connect_drop(glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[strong]
                dir,
                #[upgrade_or]
                false,
                move |t, value, x, y| view.drop_files(t, value, &dir, x, y)
            ));
            root.add_controller(target);
        }
        if let Some(file) = mark {
            mark_when_loaded(&model, &list, file);
        }
        SideColumn { model, root }
    }

    fn sync_column_sort(&self) {
        let (key, reversed) = (self.model().sort_key(), self.model().sort_reversed());
        let imp = self.imp();
        for column in imp
            .side_columns
            .borrow()
            .iter()
            .chain(imp.preview_column.borrow().iter())
        {
            column.model.set_sort_key(key);
            column.model.set_sort_reversed(reversed);
        }
    }

    /// The folder being viewed sits at the far right; keep it in sight. The strip is
    /// measured a frame or two after the columns are in and can be measured more than
    /// once, so the ask stands for a moment rather than for a single answer.
    fn scroll_columns_to_end(&self) {
        let imp = self.imp();
        imp.scroll_until
            .set(glib::monotonic_time() + SCROLL_WINDOW.as_micros() as i64);
        let adjustment = imp.columns_scroll.hadjustment();
        adjustment.set_value(adjustment.upper());
    }

    /// Whether a point in the stack falls in a column other than the current folder's.
    /// The gestures the views share act on `model()`, which those columns do not show.
    pub(crate) fn in_side_column(&self, x: f64, y: f64) -> bool {
        if self.imp().view_mode.get() != ViewMode::Columns {
            return false;
        }
        let mut widget = self.imp().stack.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(w) = widget {
            if w.has_css_class("spiral-miller-column") {
                return !w.has_css_class("spiral-miller-current");
            }
            widget = w.parent();
        }
        false
    }

    /// Icon, name and lock emblem. Only the current folder's column carries the position
    /// and the drop targets the shared view code reads back.
    fn miller_factory(&self, current: bool) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let bx = gtk::Box::builder()
                .spacing(6)
                .css_classes(["spiral-view-cell"])
                .build();
            let image = gtk::Image::builder()
                .pixel_size(view.list_icon_size())
                .css_classes(["spiral-image"])
                .build();
            view.bind_property("list-icon-size", &image, "pixel-size")
                .sync_create()
                .build();
            bx.append(&image);
            bx.append(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    // Let the column width decide, not the longest name in it.
                    .max_width_chars(1)
                    .build(),
            );
            bx.append(&emblem_image());
            item.set_child(Some(&bx));
            if current {
                remember_list_item(&bx, item);
                view.setup_cell_dnd(&bx);
            }
        });
        let view = self.downgrade();
        factory.connect_bind(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let bx = item.child().unwrap();
            let image = bx.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = image.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let emblem = bx.last_child().and_downcast::<gtk::Image>().unwrap();
            view.bind_icon(&image, &emblem, &info);
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            label.set_tooltip_text(Some(&info.display_name()));
            item.set_accessible_label(&info.display_name());
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(image) = item
                .child()
                .and_then(|bx| bx.first_child())
                .and_downcast::<gtk::Image>()
            {
                unbind_icon(&image);
            }
        });
        factory
    }

    /// Left steps out to the parent folder, right into the selected one: the arrows of a
    /// column view, which a vertical list has nothing else to do with.
    fn setup_miller_keys(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                if state.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::SHIFT_MASK
                        | gdk::ModifierType::ALT_MASK,
                ) {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::Left => {
                        let child = view.location();
                        view.go_up();
                        // Step back out onto the folder that was being viewed.
                        if let Some(child) = child {
                            view.select_files_when_loaded(vec![child]);
                        }
                        glib::Propagation::Stop
                    }
                    gdk::Key::Right => {
                        let infos = view.model().selected_infos();
                        match infos.as_slice() {
                            [info] if file_utils::is_dir(info) => {
                                view.go_to(&file_utils::file_of(info));
                                glib::Propagation::Stop
                            }
                            _ => glib::Propagation::Proceed,
                        }
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.imp().miller_list.add_controller(keys);
    }
}
