//! Drag and drop: dragging files out, dropping them on folders, and folders that open under
//! a drag held over them.

use super::*;

/// One offered action means the modifier already chose: Ctrl copies, Shift moves,
/// Ctrl+Shift links. Otherwise move for drags started in this process, copy for drags from
/// other applications.
///
/// While the drop menu is doing the deciding this says copy, whatever the drag suggests:
/// the source is told the action as the files land, before the menu has been answered, and
/// a source that deletes what it moved must not act on a drop the reader may still cancel.
pub fn preferred_action(target: &gtk::DropTarget) -> gtk::gdk::DragAction {
    let Some(drop) = target.current_drop() else {
        return gtk::gdk::DragAction::COPY;
    };
    if crate::prefs::settings().boolean("ask-on-drop") {
        return gtk::gdk::DragAction::COPY;
    }
    let actions = drop.actions();
    if actions == gtk::gdk::DragAction::COPY {
        gtk::gdk::DragAction::COPY
    } else if actions == gtk::gdk::DragAction::LINK {
        gtk::gdk::DragAction::LINK
    } else if actions.contains(gtk::gdk::DragAction::MOVE) && drop.drag().is_some() {
        // Same-process drag: default to move for local files.
        gtk::gdk::DragAction::MOVE
    } else {
        gtk::gdk::DragAction::COPY
    }
}

/// How long a drag has to rest on a folder before it springs open.
pub(super) const HOVER_OPEN_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// Open what `widget` points at when a drag rests on it: a breadcrumb, a sidebar entry.
/// Folders in the view spring open too, but from the drop target that covers the whole of
/// it, since a row's cells only cover their own text.
///
/// The pointer has to come to rest: the timer only starts once the drag has moved into the
/// widget, and starts again from every larger movement, so a drag passing over a row on
/// its way somewhere else leaves it alone.
pub fn open_on_hover(widget: &impl IsA<gtk::Widget>, open: impl Fn() + 'static) {
    let widget: gtk::Widget = widget.clone().upcast();
    let motion = gtk::DropControllerMotion::new();
    let start = std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0)));
    let timer: std::rc::Rc<RefCell<Option<glib::SourceId>>> = Default::default();
    let open = std::rc::Rc::new(open);
    motion.connect_enter(glib::clone!(
        #[strong]
        start,
        move |_, x, y| start.set((x, y))
    ));
    motion.connect_motion(glib::clone!(
        #[strong]
        start,
        #[strong]
        timer,
        #[strong]
        open,
        #[weak(rename_to = widget)]
        widget,
        move |_, x, y| {
            let (from_x, from_y) = start.get();
            if !widget.drag_check_threshold(from_x as i32, from_y as i32, x as i32, y as i32) {
                return;
            }
            start.set((x, y));
            if let Some(id) = timer.borrow_mut().take() {
                id.remove();
            }
            timer.replace(Some(glib::timeout_add_local_once(
                HOVER_OPEN_AFTER,
                glib::clone!(
                    #[strong]
                    timer,
                    #[strong]
                    open,
                    move || {
                        timer.replace(None);
                        open();
                    }
                ),
            )));
        }
    ));
    motion.connect_leave(glib::clone!(
        #[strong]
        timer,
        move |_| {
            if let Some(id) = timer.borrow_mut().take() {
                id.remove();
            }
        }
    ));
    widget.add_controller(motion);
}

impl BrowserView {
    /// Drag the selection out of the view. The source sits above the list and captures
    /// the press, because the list's own rubberband gesture claims it otherwise; a press
    /// that misses every row is left alone so rubberband selection still works.
    pub(super) fn setup_drag_source(&self) {
        if self.chooser_mode() {
            return;
        }
        let source = gtk::DragSource::builder()
            .actions(
                gtk::gdk::DragAction::COPY
                    | gtk::gdk::DragAction::MOVE
                    | gtk::gdk::DragAction::LINK,
            )
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        source.connect_prepare(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            None,
            move |source, x, y| {
                let stack = view.imp().stack.clone().upcast::<gtk::Widget>();
                let cell = cell_at(&stack, x, y)?;
                let pos = cell_position(&cell)?;
                let sel = view.model().selection();
                if !sel.is_selected(pos) {
                    sel.select_item(pos, true);
                }
                let files = view.model().selected_files();
                if files.is_empty() {
                    return None;
                }
                // Drag the whole row, not the cell the gesture started on.
                if let Some(row) = row_widget(&cell) {
                    let paintable = gtk::WidgetPaintable::new(Some(&row));
                    source.set_icon(Some(&paintable), row.width() / 2, row.height() / 2);
                }
                Some(gtk::gdk::ContentProvider::for_value(
                    &gtk::gdk::FileList::from_array(&files).to_value(),
                ))
            }
        ));
        self.imp().stack.add_controller(source);
        // The list's rubberband gesture outruns any drag source, so it is switched off
        // for presses that land on a row and back on for presses on empty space.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, x, y| {
                let imp = view.imp();
                let stack = imp.stack.clone().upcast::<gtk::Widget>();
                let empty = cell_at(&stack, x, y).is_none();
                imp.grid_view.set_enable_rubberband(empty);
                imp.column_view.set_enable_rubberband(empty);
            }
        ));
        self.imp().stack.add_controller(click);
    }

    /// Each cell is a drop target when it shows a folder; every cell of a row carries
    /// one, so the whole row accepts a drop. Dragging is handled view-wide instead,
    /// because the list's rubberband gesture outruns any drag source on a cell.
    pub(crate) fn setup_cell_dnd(&self, cell: &impl IsA<gtk::Widget>) {
        if self.chooser_mode() {
            return;
        }
        let cell = cell.clone().upcast::<gtk::Widget>();
        let target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::LINK,
        );
        let over = glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            gtk::gdk::DragAction::empty(),
            move |t: &gtk::DropTarget| match view.cell_folder(&cell) {
                Some(_) => preferred_action(t),
                None => gtk::gdk::DragAction::empty(),
            }
        );
        target.connect_enter(glib::clone!(
            #[strong]
            over,
            move |t, _, _| over(t)
        ));
        target.connect_motion(move |t, _, _| over(t));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            false,
            move |t, value, x, y| {
                // What the cell shows can change under a resting drag: a spring-loaded
                // folder opens and the cell is bound to whatever the new folder has in
                // that place. The pointer has not moved, so this target is still the one
                // the drop reaches, and refusing it would lose the files; they go into the
                // folder on screen instead, which is where the drop landed.
                let Some(folder) = view.cell_folder(&cell).or_else(|| view.location()) else {
                    return false;
                };
                view.drop_files(t, value, &folder, x, y)
            }
        ));
        cell.add_controller(target);
    }

    /// A drag resting on a folder opens it, so files can be carried into a folder that is
    /// not on screen when the drag starts. The wait starts again whenever the drag reaches
    /// a different folder, and is dropped when it leaves the view or lands.
    pub(super) fn hover_folder(&self, x: f64, y: f64) {
        let imp = self.imp();
        let over = self.item_at(x, y).filter(|&pos| {
            self.model()
                .info_at(pos)
                .as_ref()
                .is_some_and(file_utils::is_dir)
        });
        if over == imp.hover_pos.get() {
            return;
        }
        // GTK's own drop outline is off in the views whose cells are the columns of a row,
        // where it would draw around one column; those get the row marked instead. The
        // grid keeps the outline: a cell there is the whole tile.
        self.set_drop_row(over.and_then(|_| {
            imp.stack
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .as_ref()
                .and_then(row_widget)
        }));
        imp.hover_pos.set(over);
        if let Some(id) = imp.hover_timer.borrow_mut().take() {
            id.remove();
        }
        let Some(pos) = over else {
            return;
        };
        let id = glib::timeout_add_local_once(
            HOVER_OPEN_AFTER,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    let imp = view.imp();
                    imp.hover_timer.replace(None);
                    if imp.hover_pos.get() != Some(pos) {
                        return;
                    }
                    imp.hover_pos.set(None);
                    if let Some(info) = view.model().info_at(pos).filter(file_utils::is_dir) {
                        view.go_to(&file_utils::file_of(&info));
                    }
                }
            ),
        );
        imp.hover_timer.replace(Some(id));
    }

    /// The folder shown at a point of the view, if there is one there.
    pub(super) fn folder_at(&self, x: f64, y: f64) -> Option<gio::File> {
        let info = self
            .item_at(x, y)
            .and_then(|pos| self.model().info_at(pos))?;
        file_utils::is_dir(&info).then(|| file_utils::file_of(&info))
    }

    /// Mark the row a drag is over, unmarking the one it left.
    pub(super) fn set_drop_row(&self, row: Option<gtk::Widget>) {
        let row = row.filter(|_| self.view_mode() != ViewMode::Grid);
        if let Some(old) = self.imp().drop_row.replace(row.clone()) {
            old.remove_css_class("spiral-drop-row");
        }
        if let Some(row) = row {
            row.add_css_class("spiral-drop-row");
        }
    }

    /// Stop waiting for a folder to spring open.
    pub(super) fn end_hover(&self) {
        let imp = self.imp();
        self.set_drop_row(None);
        imp.hover_pos.set(None);
        if let Some(id) = imp.hover_timer.borrow_mut().take() {
            id.remove();
        }
    }

    /// The folder a cell currently shows, if it is one.
    pub(super) fn cell_folder(&self, cell: &gtk::Widget) -> Option<gio::File> {
        let pos = cell_position(cell)?;
        let info = self.model().info_at(pos)?;
        file_utils::is_dir(&info).then(|| file_utils::file_of(&info))
    }

    /// Handle a `FileList` drop into `folder`; returns whether it was accepted.
    pub fn drop_files(
        &self,
        target: &gtk::DropTarget,
        value: &glib::Value,
        folder: &gio::File,
        x: f64,
        y: f64,
    ) -> bool {
        // The drag is over, whatever comes of it.
        self.end_strip_drag();
        self.end_hover();
        let Ok(list) = value.get::<gtk::gdk::FileList>() else {
            return false;
        };
        let files: Vec<gio::File> = list
            .files()
            .into_iter()
            .filter(|f| !f.equal(folder) && !folder.has_prefix(f))
            .collect();
        if files.is_empty() {
            return false;
        }
        // Always ask, since nothing here can tell a held modifier from a plain drop.
        // Wayland has the compositor pick the action before the drop reaches us, and the
        // pointer carries no keyboard modifiers while the drag grab is on.
        if self.imp().settings.boolean("ask-on-drop") {
            // Moving into the folder the files already sit in would do nothing, so do not
            // offer it; copying there still makes sense, and makes copies.
            let settled = files
                .iter()
                .all(|f| f.parent().is_some_and(|p| p.equal(folder)));
            if let Some(move_action) = self
                .imp()
                .actions
                .lookup_action("drop-move")
                .and_downcast::<gio::SimpleAction>()
            {
                move_action.set_enabled(!settled);
            }
            self.imp()
                .pending_drop
                .replace(Some((files, folder.clone())));
            self.ask_drop_action(target, x, y);
            return true;
        }
        self.run_drop(files, folder, preferred_action(target))
    }

    /// Put the drop menu where the files landed. Closing it without a choice, by Escape or
    /// a click outside, drops the files it was asking about.
    pub(super) fn ask_drop_action(&self, target: &gtk::DropTarget, x: f64, y: f64) {
        let point = target
            .widget()
            .and_then(|w| w.compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32)));
        let (px, py) = match point {
            // Drops on the sidebar or a breadcrumb land outside the view: use its middle.
            Some(p)
                if (0.0..self.width() as f32).contains(&p.x())
                    && (0.0..self.height() as f32).contains(&p.y()) =>
            {
                (p.x() as f64, p.y() as f64)
            }
            _ => (self.width() as f64 / 2.0, self.height() as f64 / 2.0),
        };
        self.popup_model(&self.imp().drop_menu, px, py);
    }

    /// Carry out a drop, once it is clear what it should do.
    pub(super) fn run_drop(
        &self,
        files: Vec<gio::File>,
        folder: &gio::File,
        action: gtk::gdk::DragAction,
    ) -> bool {
        if action == gtk::gdk::DragAction::LINK {
            self.submit_kind(crate::ops::JobKind::Link {
                files,
                dest: folder.clone(),
            });
            return true;
        }
        let is_move = action == gtk::gdk::DragAction::MOVE;
        // Moving onto the folder the files already live in is a no-op.
        if is_move
            && files
                .iter()
                .all(|f| f.parent().is_some_and(|p| p.equal(folder)))
        {
            return false;
        }
        let pairs = files.into_iter().map(|f| (f, folder.clone())).collect();
        self.submit_kind(crate::ops::JobKind::Transfer { pairs, is_move });
        true
    }

    /// A drop menu entry was picked. Taking the files means a second click does nothing.
    pub(crate) fn finish_drop(&self, action: gtk::gdk::DragAction) {
        if let Some((files, folder)) = self.imp().pending_drop.take() {
            self.run_drop(files, &folder, action);
        }
    }
}
