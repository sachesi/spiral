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
/// How far from an end of the strip a drag starts pushing it along, and how fast a drag
/// held right at the end pushes, in pixels a frame.
const DRAG_EDGE: f64 = 48.0;
const DRAG_SPEED: f64 = 20.0;
/// What a touchpad's own units are worth, as `GtkScrolledWindow` weighs them.
const SURFACE_SCROLL_FACTOR: f64 = 2.5;

/// A column that is not the folder being viewed: its own listing of `dir`.
pub struct SideColumn {
    dir: gio::File,
    model: FolderModel,
    list: gtk::ListView,
    root: gtk::ScrolledWindow,
}

/// The column widget at a point in `root`'s own coordinates.
fn column_at(root: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Option<gtk::Widget> {
    let mut widget = root.as_ref().pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(w) = widget {
        if w.has_css_class("spiral-miller-column") {
            return Some(w);
        }
        widget = w.parent();
    }
    None
}

/// Every column of the strip beside the one being viewed.
fn side_roots(view: &BrowserView) -> Vec<gtk::ScrolledWindow> {
    let imp = view.imp();
    let columns = imp.side_columns.borrow();
    columns
        .iter()
        .chain(imp.preview_column.borrow().iter())
        .map(|column| column.root.clone())
        .collect()
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
        self.setup_strip_input();
    }

    /// Shift and the wheel move the strip sideways, and a drag held near either end
    /// pushes it along, so a column that is off screen can still be reached.
    fn setup_strip_input(&self) {
        // GTK swaps the axes for a shifted scroll on its own, but the column under the
        // pointer takes the event and answers "handled" for something it did nothing
        // with: it scrolls up and down, and only the strip scrolls sideways.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::SHIFT_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                let adjustment = view.imp().columns_scroll.hadjustment();
                // The step GTK gives a wheel detent, so the strip moves like the rest.
                let step = match controller.unit() {
                    gdk::ScrollUnit::Wheel => adjustment.page_size().powf(2.0 / 3.0),
                    _ => SURFACE_SCROLL_FACTOR,
                };
                adjustment.set_value(adjustment.value() + dy * step);
                glib::Propagation::Stop
            }
        ));
        self.imp().columns_scroll.add_controller(scroll);

        let drag = gtk::DropControllerMotion::new();
        let at = glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_: &gtk::DropControllerMotion, x: f64, y: f64| view.push_strip(Some((x, y)))
        );
        drag.connect_enter(at.clone());
        drag.connect_motion(at);
        drag.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.push_strip(None)
        ));
        self.imp().columns_scroll.add_controller(drag);
    }

    /// Follow a drag over the strip: mark the column it would land in, and push the
    /// strip along while it hovers near either end.
    fn push_strip(&self, at: Option<(f64, f64)>) {
        let imp = self.imp();
        let Some((x, y)) = at else {
            self.end_strip_drag();
            return;
        };
        imp.drag_at.set((x, y));
        self.mark_drop_column(column_at(&*imp.columns_scroll, x, y).as_ref());
        if imp.drag_tick.borrow().is_some() || self.strip_push(x) == 0.0 {
            return;
        }
        let tick = imp.columns_scroll.add_tick_callback(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |scroll, _| {
                let (x, y) = view.imp().drag_at.get();
                let adjustment = scroll.hadjustment();
                let was = adjustment.value();
                adjustment.set_value(was + view.strip_push(x));
                // Nothing left to push. A drag that ends over the strip says nothing
                // about it, so this is also what stops the pushing after a drop.
                if adjustment.value() == was {
                    view.imp().drag_tick.take();
                    return glib::ControlFlow::Break;
                }
                // The column under the pointer changes as the strip slides past it, and
                // GTK only looks again when the pointer itself moves.
                view.mark_drop_column(column_at(scroll, x, y).as_ref());
                glib::ControlFlow::Continue
            }
        ));
        imp.drag_tick.replace(Some(tick));
    }

    /// How fast a drag hovering at `x` pushes the strip, in pixels a frame.
    fn strip_push(&self, x: f64) -> f64 {
        let width = self.imp().columns_scroll.width() as f64;
        let past_start = (DRAG_EDGE - x).clamp(0.0, DRAG_EDGE);
        let past_end = (x - (width - DRAG_EDGE)).clamp(0.0, DRAG_EDGE);
        (past_end - past_start) / DRAG_EDGE * DRAG_SPEED
    }

    /// The drag is over: stop pushing and take the mark off.
    pub(crate) fn end_strip_drag(&self) {
        if let Some(tick) = self.imp().drag_tick.take() {
            tick.remove();
        }
        self.mark_drop_column(None);
    }

    /// Show which column a drop would land in. GTK's own `:drop(active)` is a frame or
    /// more behind while the strip is moving, and it would point at the wrong folder.
    fn mark_drop_column(&self, target: Option<&gtk::Widget>) {
        for root in side_roots(self) {
            if Some(root.upcast_ref::<gtk::Widget>()) == target {
                root.add_css_class("spiral-drop-column");
            } else {
                root.remove_css_class("spiral-drop-column");
            }
        }
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
        if imp.view_mode.get() != ViewMode::Columns {
            self.clear_side_columns();
            return;
        }
        let Some(location) = self.location() else {
            self.clear_side_columns();
            return;
        };
        // The chain the path bar draws: from home, or from the root of the filesystem.
        // A mount point is where the crumbs start but not the columns, since finding it
        // means asking gvfs and the strip is drawn before an answer could arrive.
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
        // Walking one folder along leaves most of the path where it was, so the columns
        // whose folder has not changed are kept, listing and all; only the mark moves.
        let mut old: std::collections::VecDeque<SideColumn> = imp.side_columns.take().into();
        let mut columns: Vec<SideColumn> = Vec::new();
        let mut reusing = true;
        for (dir, child) in chain.into_iter().rev() {
            reusing = reusing && old.front().is_some_and(|column| column.dir.equal(&dir));
            match reusing.then(|| old.pop_front()).flatten() {
                Some(column) => {
                    mark_when_loaded(&column.model, &column.list, child);
                    columns.push(column);
                }
                None => columns.push(self.side_column(&dir, Some(child))),
            }
        }
        for column in old {
            imp.columns_box.remove(&column.root);
        }
        let mut previous: Option<gtk::Widget> = None;
        for column in &columns {
            if column.root.parent().is_none() {
                imp.columns_box
                    .insert_child_after(&column.root, previous.as_ref());
            }
            previous = Some(column.root.clone().upcast());
        }
        imp.side_columns.replace(columns);
        self.update_preview_column();
        self.scroll_columns_to_end();
    }

    /// Draw the columns from scratch, for the settings that change how a folder is
    /// listed: the kept ones would otherwise hold the old order.
    pub(crate) fn refresh_columns(&self) {
        self.clear_side_columns();
        self.rebuild_columns();
    }

    /// Whether the folder being viewed has a path to draw beside it.
    pub(crate) fn shows_chain(&self) -> bool {
        self.location()
            .is_some_and(|location| !location.equal(&crate::path_bar::chain_root(&location, None)))
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
            .flags(gio::SettingsBindFlags::GET)
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
        SideColumn {
            dir: dir.clone(),
            model,
            list,
            root,
        }
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
    /// measured over the frames that follow, and the focus and the fresh listings pull it
    /// about while that happens, so the ask is re-made every frame for a moment.
    fn scroll_columns_to_end(&self) {
        let deadline = glib::monotonic_time() + SCROLL_WINDOW.as_micros() as i64;
        self.imp()
            .columns_scroll
            .add_tick_callback(move |scroll, _| {
                let adjustment = scroll.hadjustment();
                adjustment.set_value(adjustment.upper());
                if glib::monotonic_time() < deadline {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
    }

    /// Whether a point in the stack falls in a column other than the current folder's.
    /// The gestures the views share act on `model()`, which those columns do not show.
    pub(crate) fn in_side_column(&self, x: f64, y: f64) -> bool {
        self.imp().view_mode.get() == ViewMode::Columns
            && column_at(&*self.imp().stack, x, y)
                .is_some_and(|w| !w.has_css_class("spiral-miller-current"))
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
            view.bind_icon(&image, &emblem, &info, item.position());
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
