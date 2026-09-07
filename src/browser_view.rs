//! One folder view (a tab): grid or list over a shared `FolderModel`, with history.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::subclass::prelude::*;

use crate::enums::{SortKey, ViewMode};
use crate::file_utils;
use crate::folder_model::FolderModel;
use crate::{adw, gdk, gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/sachesi/spiral/ui/browser_view.ui")]
    #[properties(wrapper_type = super::BrowserView)]
    pub struct BrowserView {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub column_view: TemplateChild<gtk::ColumnView>,
        #[template_child]
        pub columns_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub columns_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub miller_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub miller_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub error_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub floating_bar: TemplateChild<gtk::Box>,
        #[template_child]
        pub floating_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub floating_primary: TemplateChild<gtk::Label>,
        #[template_child]
        pub floating_details: TemplateChild<gtk::Label>,
        #[template_child]
        pub item_menu: TemplateChild<gio::MenuModel>,
        #[template_child]
        pub background_menu: TemplateChild<gio::MenuModel>,
        #[template_child]
        pub drop_menu: TemplateChild<gio::MenuModel>,
        /// Files waiting for the drop menu to say what to do with them.
        pub pending_drop: RefCell<Option<(Vec<gio::File>, gio::File)>>,
        /// The columns of the Miller view beside the folder being viewed: the path
        /// that leads to it, then the folder the selection points at.
        pub side_columns: RefCell<Vec<crate::miller::SideColumn>>,
        pub preview_column: RefCell<Option<crate::miller::SideColumn>>,
        /// Bumped per selection so only the last of a run of them lists a folder.
        pub preview_gen: Cell<u64>,
        /// Set while a rebuild of the strip is waiting for the current change to end.
        pub columns_pending: Cell<bool>,
        /// Until when the strip puts itself back at the folder being viewed, on the
        /// monotonic clock.
        pub scroll_until: Cell<i64>,

        #[property(get)]
        pub model: FolderModel,
        #[property(get, set = Self::set_view_mode, builder(ViewMode::Grid))]
        pub view_mode: Cell<ViewMode>,
        #[property(get)]
        pub can_go_back: Cell<bool>,
        #[property(get)]
        pub can_go_forward: Cell<bool>,
        #[property(get, set)]
        icon_size: Cell<i32>,
        #[property(get, set)]
        list_icon_size: Cell<i32>,
        /// View mode remembered for the current folder, if any.
        pub folder_view: Cell<Option<ViewMode>>,
        /// Sort order remembered for the current folder, if any.
        pub folder_sort: Cell<Option<(SortKey, bool)>>,
        /// Whether the current folder accepts new files, looked up when it is entered.
        pub can_write: Cell<bool>,
        /// Bumped per navigation so late view lookups for an old folder are dropped.
        pub nav_gen: Cell<u64>,
        #[property(get, nullable)]
        pub location: RefCell<Option<gio::File>>,
        /// Portal chooser mode: no destructive actions, activation selects instead of launching.
        #[property(get, set, construct_only)]
        pub chooser_mode: Cell<bool>,

        pub actions: gio::SimpleActionGroup,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        /// Set while the column header is being updated from the model, not the user.
        pub syncing_header: Cell<bool>,
        /// Optional list columns by key, for the visibility setting.
        pub columns: RefCell<Vec<(&'static str, gtk::ColumnViewColumn)>>,

        pub history: RefCell<Vec<gio::File>>,
        pub history_pos: Cell<usize>,
        pub settings: gio::Settings,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowserView {
        const NAME: &'static str = "SpiralBrowserView";
        type Type = super::BrowserView;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            use gtk::gdk::{Key, ModifierType as M};
            klass.add_binding_action(Key::KP_Delete, M::empty(), "view.trash");
            klass.add_binding_action(Key::Delete, M::empty(), "view.trash");
            klass.add_binding_action(Key::Delete, M::SHIFT_MASK, "view.delete");
            klass.add_binding_action(Key::F2, M::empty(), "view.rename");
            klass.add_binding_action(Key::c, M::CONTROL_MASK, "view.copy");
            klass.add_binding_action(Key::x, M::CONTROL_MASK, "view.cut");
            klass.add_binding_action(Key::v, M::CONTROL_MASK, "view.paste");
            klass.add_binding_action(Key::a, M::CONTROL_MASK, "view.select-all");
            klass.add_binding_action(Key::n, M::CONTROL_MASK | M::SHIFT_MASK, "view.new-folder");
            klass.add_binding_action(Key::Return, M::ALT_MASK, "view.properties");
            klass.add_binding_action(Key::Menu, M::empty(), "view.context-menu");
            klass.add_binding_action(Key::F10, M::SHIFT_MASK, "view.context-menu");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }

        fn new() -> Self {
            Self {
                stack: Default::default(),
                grid_view: Default::default(),
                column_view: Default::default(),
                columns_scroll: Default::default(),
                columns_box: Default::default(),
                miller_scroll: Default::default(),
                miller_list: Default::default(),
                side_columns: Default::default(),
                preview_column: Default::default(),
                preview_gen: Default::default(),
                columns_pending: Default::default(),
                scroll_until: Default::default(),
                error_page: Default::default(),
                empty_page: Default::default(),
                floating_bar: Default::default(),
                floating_spinner: Default::default(),
                floating_primary: Default::default(),
                floating_details: Default::default(),
                item_menu: Default::default(),
                background_menu: Default::default(),
                drop_menu: Default::default(),
                pending_drop: Default::default(),
                chooser_mode: Default::default(),
                actions: gio::SimpleActionGroup::new(),
                popover: Default::default(),
                syncing_header: Default::default(),
                columns: Default::default(),
                model: FolderModel::default(),
                view_mode: Default::default(),
                can_go_back: Default::default(),
                can_go_forward: Default::default(),
                icon_size: Cell::new(96),
                list_icon_size: Cell::new(32),
                folder_view: Default::default(),
                folder_sort: Default::default(),
                can_write: Cell::new(true),
                nav_gen: Default::default(),
                location: Default::default(),
                history: Default::default(),
                history_pos: Default::default(),
                settings: gio::Settings::new(crate::config::APP_ID),
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for BrowserView {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("open-in-new-tab")
                        .param_types([gio::File::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("file-activated")
                        .param_types([gio::File::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_actions();
            obj.setup_drag_source();
            obj.setup_background_click();

            // Dropping on empty space copies/moves into the folder being viewed.
            if !obj.chooser_mode() {
                let target = gtk::DropTarget::new(
                    gtk::gdk::FileList::static_type(),
                    gtk::gdk::DragAction::COPY
                        | gtk::gdk::DragAction::MOVE
                        | gtk::gdk::DragAction::LINK,
                );
                let action = glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    gtk::gdk::DragAction::empty(),
                    move |t: &gtk::DropTarget, x: f64, y: f64| {
                        if obj.in_side_column(x, y) {
                            gtk::gdk::DragAction::empty()
                        } else {
                            preferred_action(t)
                        }
                    }
                );
                target.connect_enter(action.clone());
                target.connect_motion(action);
                target.connect_drop(glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    false,
                    move |t, value, x, y| match obj.location() {
                        Some(loc) if !obj.in_side_column(x, y) =>
                            obj.drop_files(t, value, &loc, x, y),
                        _ => false,
                    }
                ));
                self.stack.add_controller(target);
            }

            let key = if obj.chooser_mode() {
                "chooser-view-mode"
            } else {
                "view-mode"
            };
            obj.set_view_mode(global_view_mode(&self.settings, key));
            // The global default only drives folders without a remembered view.
            self.settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak]
                    obj,
                    move |s, _| {
                        if obj.imp().folder_view.get().is_none() {
                            obj.set_view_mode(global_view_mode(s, key));
                        }
                    }
                ),
            );
            for key in crate::prefs::VIEW_KEYS {
                self.settings.connect_changed(
                    Some(key),
                    glib::clone!(
                        #[weak]
                        obj,
                        move |_, _| {
                            obj.model().reload();
                            obj.rebuild_columns();
                        }
                    ),
                );
            }
            let apply_click = glib::clone!(
                #[weak(rename_to = imp)]
                self,
                move |_: &gio::Settings, _: &str| {
                    let single = crate::prefs::single_click();
                    imp.grid_view.set_single_click_activate(single);
                    imp.column_view.set_single_click_activate(single);
                    imp.miller_list.set_single_click_activate(single);
                }
            );
            apply_click(&self.settings, "click-policy");
            self.settings
                .connect_changed(Some("click-policy"), apply_click);
            self.settings.bind("grid-zoom", &*obj, "icon-size").build();
            self.settings
                .bind("list-zoom", &*obj, "list-icon-size")
                .build();

            // Ctrl+wheel zooms; small touchpad deltas add up to whole steps.
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
            let acc = std::cell::Cell::new(0.0f64);
            scroll.connect_scroll(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |c, _, dy| {
                    if !c
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK)
                    {
                        acc.set(0.0);
                        return glib::Propagation::Proceed;
                    }
                    let sum = acc.get() + dy;
                    if sum.abs() >= 1.0 {
                        let action = if sum < 0.0 {
                            "win.zoom-in"
                        } else {
                            "win.zoom-out"
                        };
                        let _ = obj.activate_action(action, None);
                        acc.set(0.0);
                    } else {
                        acc.set(sum);
                    }
                    glib::Propagation::Stop
                }
            ));
            obj.add_controller(scroll);
            // The global sort order only drives folders without a remembered one.
            obj.apply_global_sort();
            for key in ["sort-key", "sort-reversed"] {
                self.settings.connect_changed(
                    Some(key),
                    glib::clone!(
                        #[weak]
                        obj,
                        move |_, _| {
                            if obj.imp().folder_sort.get().is_none() {
                                obj.apply_global_sort();
                            }
                        }
                    ),
                );
            }
            self.settings
                .bind("show-hidden", &self.model, "show-hidden")
                .build();

            self.grid_view.set_model(Some(&self.model.selection()));
            self.column_view.set_model(Some(&self.model.selection()));
            obj.setup_grid_factory();
            obj.setup_columns();
            obj.setup_miller();

            self.grid_view.connect_activate(glib::clone!(
                #[weak]
                obj,
                move |_, pos| obj.activate_position(pos)
            ));
            self.column_view.connect_activate(glib::clone!(
                #[weak]
                obj,
                move |_, pos| obj.activate_position(pos)
            ));
            self.miller_list.connect_activate(glib::clone!(
                #[weak]
                obj,
                move |_, pos| obj.activate_position(pos)
            ));

            let update = glib::clone!(
                #[weak]
                obj,
                move |_: &FolderModel| obj.update_stack()
            );
            self.model.connect_loading_notify(update.clone());
            self.model.connect_error_message_notify(update.clone());
            self.model.selection().connect_items_changed(glib::clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.update_stack();
                    obj.update_floating_bar();
                }
            ));
            self.model
                .selection()
                .connect_selection_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_, _, _| obj.update_floating_bar()
                ));
            self.model.connect_loading_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.update_floating_bar()
            ));
            obj.update_stack();
        }
    }

    impl WidgetImpl for BrowserView {}
    impl BoxImpl for BrowserView {}

    impl BrowserView {
        fn set_view_mode(&self, mode: ViewMode) {
            self.view_mode.set(mode);
            // Only the list can show a folder's children in place.
            if mode != ViewMode::List {
                self.model.collapse_all();
            }
            self.obj().update_stack();
            self.obj().rebuild_columns();
        }
    }
}

glib::wrapper! {
    pub struct BrowserView(ObjectSubclass<imp::BrowserView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

fn unbind_captions(label: &gtk::Label) {
    if let Some(handle) =
        unsafe { label.steal_data::<futures_util::future::AbortHandle>("count-abort") }
    {
        handle.abort();
    }
}

fn global_view_mode(settings: &gio::Settings, key: &str) -> ViewMode {
    match settings.enum_(key) {
        v if v == ViewMode::List as i32 => ViewMode::List,
        v if v == ViewMode::Columns as i32 => ViewMode::Columns,
        _ => ViewMode::Grid,
    }
}

/// Store a per-folder metadata attribute without waiting for the metadata daemon.
fn remember(dir: gio::File, attribute: &'static str, value: String) {
    let info = gio::FileInfo::new();
    info.set_attribute_string(attribute, &value);
    glib::spawn_future_local(async move {
        if let Err(e) = dir
            .set_attributes_future(
                &info,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
        {
            glib::g_debug!("spiral", "cannot set {attribute} on {}: {e}", dir.uri());
        }
    });
}

fn folders_selected(n: usize) -> String {
    ngettext("%d folder selected", "%d folders selected", n as u32).replace("%d", &n.to_string())
}

fn items_selected(n: usize) -> String {
    ngettext("%d item selected", "%d items selected", n as u32).replace("%d", &n.to_string())
}

/// True when at least four files were seen (2000 at most) and half or more are images or videos.
fn mostly_media(model: &FolderModel) -> bool {
    let sel = model.selection();
    let mut files = 0u32;
    let mut media = 0u32;
    for i in 0..sel.n_items().min(2000) {
        let Some(info) = model.info_at(i) else {
            continue;
        };
        if file_utils::is_dir(&info) {
            continue;
        }
        files += 1;
        if info
            .content_type()
            .is_some_and(|ct| ct.starts_with("image/") || ct.starts_with("video/"))
        {
            media += 1;
        }
    }
    files >= 4 && media * 2 >= files
}

/// Number of direct children of `dir`, or None if it cannot be read.
async fn count_children(dir: &gio::File) -> Option<u64> {
    let en = dir
        .enumerate_children_future(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::LOW,
        )
        .await
        .ok()?;
    let mut n = 0u64;
    loop {
        let batch = en.next_files_future(256, glib::Priority::LOW).await.ok()?;
        if batch.is_empty() {
            return Some(n);
        }
        n += batch.len() as u64;
    }
}

/// Width of the emblem margin beside a grid icon, as in Nautilus.
const EMBLEM_MARGIN: i32 = 18;

/// Lock shown on files the user cannot read or change, dimmed like Nautilus emblems. It
/// keeps its place when empty so icons line up across cells.
pub(crate) fn emblem_image() -> gtk::Image {
    gtk::Image::builder()
        .pixel_size(16)
        .css_classes(["dim-label"])
        .build()
}

fn set_emblem(emblem: &gtk::Image, locked: bool) {
    emblem.set_icon_name(locked.then_some("changes-prevent-symbolic"));
}

pub(crate) fn unbind_icon(image: &gtk::Image) {
    if let Some(handle) =
        unsafe { image.steal_data::<futures_util::future::AbortHandle>("thumb-abort") }
    {
        handle.abort();
    }
    image.remove_css_class("file-thumbnail");
}

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
        // Same-process drag: default to move like Nautilus does for local files.
        gtk::gdk::DragAction::MOVE
    } else {
        gtk::gdk::DragAction::COPY
    }
}

/// Cells keep a weak link to their `ListItem`: its position is live, unlike a cached
/// number, when items are inserted above it.
/// A filled star for a favourite, a hollow one otherwise.
fn set_star(button: &gtk::Button, starred: bool) {
    button.set_icon_name(if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    button.set_tooltip_text(Some(&if starred {
        gettext("Remove from Starred")
    } else {
        gettext("Add to Starred")
    }));
}

/// Files waiting on the clipboard as a cut are dimmed, the way Nautilus marks them.
pub(crate) fn set_cut(cell: &impl IsA<gtk::Widget>, info: &gio::FileInfo) {
    if crate::clipboard::is_cut(&file_utils::file_of(info)) {
        cell.add_css_class("spiral-cut");
    } else {
        cell.remove_css_class("spiral-cut");
    }
}

pub(crate) fn remember_list_item(cell: &impl IsA<gtk::Widget>, item: &gtk::ListItem) {
    unsafe { cell.set_data("list-item", item.downgrade()) };
}

/// A bound cell of the row under a point, whichever part of the row the point hits.
fn cell_at(root: &gtk::Widget, x: f64, y: f64) -> Option<gtk::Widget> {
    let row = row_widget(&root.pick(x, y, gtk::PickFlags::DEFAULT)?)?;
    let mut found: Option<gtk::Widget> = None;
    each_cell(&row, &mut |cell| {
        found.get_or_insert_with(|| cell.clone());
    });
    found
}

/// The list or grid item a widget sits in: the child of the list itself.
fn row_widget(inner: &gtk::Widget) -> Option<gtk::Widget> {
    let mut w = inner.clone();
    while let Some(parent) = w.parent() {
        if parent.is::<gtk::ListView>() || parent.is::<gtk::GridView>() {
            return Some(w);
        }
        w = parent;
    }
    None
}

/// Visit every bound cell under `root`. Cells do not nest, so a match ends that branch.
fn each_cell(root: &gtk::Widget, f: &mut impl FnMut(&gtk::Widget)) {
    let mut child = root.first_child();
    while let Some(c) = child {
        if unsafe { c.data::<glib::WeakRef<gtk::ListItem>>("list-item") }.is_some() {
            f(&c);
        } else {
            each_cell(&c, f);
        }
        child = c.next_sibling();
    }
}

/// The lock emblem of a grid or list name cell: its last child, or the icon row's.
fn cell_emblem(cell: &gtk::Widget) -> Option<gtk::Image> {
    cell.last_child().and_downcast::<gtk::Image>().or_else(|| {
        cell.first_child()?
            .last_child()
            .and_downcast::<gtk::Image>()
    })
}

pub(crate) fn cell_position(cell: &impl IsA<gtk::Widget>) -> Option<u32> {
    let weak = unsafe { cell.data::<glib::WeakRef<gtk::ListItem>>("list-item") }?;
    let pos = unsafe { weak.as_ref() }.upgrade()?.position();
    (pos != gtk::INVALID_LIST_POSITION).then_some(pos)
}

impl BrowserView {
    pub fn new(location: &gio::File) -> Self {
        let view: Self = glib::Object::new();
        view.go_to(location);
        view
    }

    pub fn action_group(&self) -> &gio::SimpleActionGroup {
        &self.imp().actions
    }

    pub fn new_chooser(location: &gio::File) -> Self {
        let view: Self = glib::Object::builder()
            .property("chooser-mode", true)
            .build();
        view.go_to(location);
        view
    }

    pub fn connect_open_in_new_tab<F: Fn(&Self, &gio::File) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "open-in-new-tab",
            false,
            glib::closure_local!(move |v: &Self, file: &gio::File| f(v, file)),
        )
    }

    pub fn connect_file_activated<F: Fn(&Self, &gio::File) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "file-activated",
            false,
            glib::closure_local!(move |v: &Self, file: &gio::File| f(v, file)),
        )
    }

    fn apply_global_sort(&self) {
        let imp = self.imp();
        let key = SortKey::from_nick(&imp.settings.string("sort-key")).unwrap_or_default();
        imp.model.set_sort_key(key);
        imp.model
            .set_sort_reversed(imp.settings.boolean("sort-reversed"));
    }

    /// Sort the current folder: remembered for this folder when views are remembered per
    /// folder, otherwise as the new global order. A chooser sorts for the session only.
    pub fn set_sort(&self, key: SortKey, reversed: bool) {
        let imp = self.imp();
        imp.model.set_sort_key(key);
        imp.model.set_sort_reversed(reversed);
        match self.location() {
            _ if self.chooser_mode() => {}
            Some(dir) if crate::prefs::remember_view() => {
                imp.folder_sort.set(Some((key, reversed)));
                let value = format!("{}-{}", key.nick(), if reversed { "desc" } else { "asc" });
                remember(dir, "metadata::spiral-sort", value);
            }
            _ => {
                let _ = imp.settings.set_string("sort-key", key.nick());
                let _ = imp.settings.set_boolean("sort-reversed", reversed);
            }
        }
    }

    /// Pick the view and sort order for a folder: remembered per folder, else (for the
    /// view) guessed from its media content once loaded, else the global default.
    fn resolve_view_mode(&self, file: &gio::File) {
        let imp = self.imp();
        let generation = imp.nav_gen.get() + 1;
        imp.nav_gen.set(generation);
        imp.folder_view.set(None);
        if imp.folder_sort.take().is_some() {
            self.apply_global_sort();
        }
        // A chooser keeps one view of its own; per-folder memory and guessing are for
        // the file manager.
        if self.chooser_mode() {
            self.set_view_mode(global_view_mode(&imp.settings, "chooser-view-mode"));
            return;
        }
        // Columns belong to the pane, not to a folder: walking from one folder to the
        // next is how they are read, and picking a view per folder would drop out of them
        // at the first click. Leaving them is what the view button and Ctrl+1/2 are for.
        let keep = self.view_mode() == ViewMode::Columns;
        if !keep {
            self.set_view_mode(global_view_mode(&imp.settings, "view-mode"));
        }
        let remember = crate::prefs::remember_view();
        let guess = crate::prefs::guess_view() && !keep;
        if !remember && !guess {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            file,
            async move {
                if remember
                    && let Ok(info) = file
                        .query_info_future(
                            "metadata::spiral-view,metadata::spiral-sort",
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    && view.imp().nav_gen.get() == generation
                {
                    if let Some((key, dir)) = info
                        .attribute_string("metadata::spiral-sort")
                        .and_then(|s| s.split_once('-').map(|(k, d)| (k.to_string(), d == "desc")))
                        && let Some(key) = SortKey::from_nick(&key)
                    {
                        view.imp().folder_sort.set(Some((key, dir)));
                        view.model().set_sort_key(key);
                        view.model().set_sort_reversed(dir);
                    }
                    if let Some(mode) = info
                        .attribute_string("metadata::spiral-view")
                        .and_then(|s| ViewMode::from_nick(&s))
                        .filter(|_| !keep)
                    {
                        view.imp().folder_view.set(Some(mode));
                        view.set_view_mode(mode);
                        return;
                    }
                }
                if !guess || view.imp().nav_gen.get() != generation {
                    return;
                }
                let model = view.model();
                if model.loading() {
                    let (tx, rx) = futures_channel::oneshot::channel::<()>();
                    let tx = std::cell::RefCell::new(Some(tx));
                    let id = model.connect_loading_notify(move |m| {
                        if !m.loading()
                            && let Some(tx) = tx.take()
                        {
                            let _ = tx.send(());
                        }
                    });
                    let _ = rx.await;
                    model.disconnect(id);
                }
                if view.imp().nav_gen.get() == generation && mostly_media(&model) {
                    view.set_view_mode(ViewMode::Grid);
                }
            }
        ));
    }

    /// Step to the next view, in the order the view button shows.
    pub fn toggle_view_mode(&self) {
        self.choose_view_mode(self.view_mode().next());
    }

    /// Switch view: for this folder only when views are remembered per folder, otherwise
    /// as the new global default.
    pub fn choose_view_mode(&self, next: ViewMode) {
        self.set_view_mode(next);
        let nick = next.nick();
        match self.location() {
            _ if self.chooser_mode() => {
                let _ = self.imp().settings.set_string("chooser-view-mode", nick);
            }
            Some(dir) if crate::prefs::remember_view() => {
                self.imp().folder_view.set(Some(next));
                remember(dir, "metadata::spiral-view", nick.to_string());
            }
            _ => {
                let _ = self.imp().settings.set_string("view-mode", nick);
            }
        }
    }

    /// Navigate to `file`, pushing onto history.
    pub fn go_to(&self, file: &gio::File) {
        let imp = self.imp();
        if imp.model.location().is_some_and(|l| l.equal(file)) {
            return;
        }
        {
            let mut hist = imp.history.borrow_mut();
            let pos = imp.history_pos.get();
            if !hist.is_empty() {
                hist.truncate(pos + 1);
            }
            hist.push(file.clone());
            imp.history_pos.set(hist.len() - 1);
        }
        self.set_location_internal(file);
    }

    fn set_location_internal(&self, file: &gio::File) {
        let imp = self.imp();
        imp.model.set_search_text("");
        imp.model.set_location(Some(file));
        imp.location.replace(Some(file.clone()));
        self.resolve_view_mode(file);
        // Model is empty right after set_location; scroll once the first items land.
        let sel = imp.model.selection();
        let id = std::rc::Rc::new(std::cell::RefCell::new(None));
        let id2 = id.clone();
        *id.borrow_mut() = Some(sel.connect_items_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |sel, _, _, added| {
                if added == 0 {
                    return;
                }
                let imp = view.imp();
                imp.grid_view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
                imp.miller_list
                    .scroll_to(0, gtk::ListScrollFlags::NONE, None);
                imp.column_view
                    .scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
                if let Some(id) = id2.borrow_mut().take() {
                    sel.disconnect(id);
                }
            }
        )));
        let hist = imp.history.borrow();
        let pos = imp.history_pos.get();
        imp.can_go_back.set(pos > 0);
        imp.can_go_forward.set(pos + 1 < hist.len());
        drop(hist);
        self.notify_can_go_back();
        self.notify_can_go_forward();
        self.notify_location();
    }

    pub fn go_back(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        if pos == 0 {
            return;
        }
        imp.history_pos.set(pos - 1);
        let file = imp.history.borrow()[pos - 1].clone();
        self.set_location_internal(&file);
    }

    pub fn go_forward(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        if pos + 1 >= imp.history.borrow().len() {
            return;
        }
        imp.history_pos.set(pos + 1);
        let file = imp.history.borrow()[pos + 1].clone();
        self.set_location_internal(&file);
    }

    pub fn go_up(&self) {
        if let Some(parent) = self.location().and_then(|l| l.parent()) {
            self.go_to(&parent);
        }
    }

    pub fn reload(&self) {
        self.imp().model.reload();
    }

    /// Open the item at `pos`: descend into folders, launch files.
    fn activate_position(&self, pos: u32) {
        let Some(info) = self.imp().model.info_at(pos) else {
            return;
        };
        let file = file_utils::file_of(&info);
        if file_utils::is_dir(&info) {
            self.go_to(&file);
        } else if self.chooser_mode() {
            self.emit_by_name::<()>("file-activated", &[&file]);
        } else {
            self.launch(&file);
        }
    }

    pub fn launch(&self, file: &gio::File) {
        let ctx = self.display().app_launch_context();
        let uri = file.uri();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await
                {
                    view.show_error(&gettext("Could Not Open"), e.message());
                }
            }
        ));
    }

    pub(crate) fn show_error(&self, heading: &str, message: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(message)
            .build();
        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self));
    }

    fn update_stack(&self) {
        let imp = self.imp();
        let name = if imp.model.error_message().is_some() {
            if let Some(msg) = imp.model.error_message() {
                imp.error_page.set_description(Some(&msg));
            }
            "error"
        } else if imp.view_mode.get() == ViewMode::Columns && !imp.model.searching() {
            // The strip keeps the path on screen even where the folder itself is empty,
            // and a search reaches past the folder, which no column can draw.
            "columns"
        } else if !imp.model.loading() && imp.model.n_items() == 0 {
            imp.empty_page.set_title(&if imp.model.searching() {
                gettext("No Results Found")
            } else {
                gettext("Folder is Empty")
            });
            "empty"
        } else {
            match imp.view_mode.get() {
                ViewMode::Grid => "grid",
                _ => "list",
            }
        };
        imp.stack.set_visible_child_name(name);
    }

    /// Bottom-right status: selection summary while something is selected, spinner while loading.
    fn update_floating_bar(&self) {
        let imp = self.imp();
        let infos = imp.model.selected_infos();
        let loading = imp.model.loading();
        imp.floating_spinner.set_visible(loading);
        if infos.is_empty() {
            imp.floating_primary.set_text(&if loading {
                gettext("Loading…")
            } else {
                String::new()
            });
            imp.floating_details.set_text("");
            imp.floating_bar.set_visible(loading);
            return;
        }
        let folders = infos.iter().filter(|i| file_utils::is_dir(i)).count();
        let files = infos.len() - folders;
        let size: u64 = infos
            .iter()
            .filter(|i| !file_utils::is_dir(i))
            .map(|i| i.size() as u64)
            .sum();
        let primary = match (folders, files) {
            (1, 0) | (0, 1) => gettext("“%s” selected").replace("%s", &infos[0].display_name()),
            (f, 0) => folders_selected(f),
            (0, n) => items_selected(n),
            (f, n) => {
                let others = ngettext(
                    "%d other item selected",
                    "%d other items selected",
                    n as u32,
                )
                .replace("%d", &n.to_string());
                // Translators: %f is "%d folders selected", %n is "%d other items selected".
                gettext("%f, %n")
                    .replace("%f", &folders_selected(f))
                    .replace("%n", &others)
            }
        };
        imp.floating_primary.set_text(&primary);
        imp.floating_details.set_text(&if files > 0 {
            format!("({})", crate::prefs::size(size))
        } else {
            String::new()
        });
        imp.floating_bar.set_visible(true);
    }

    /// Caption lines under the name, per the `captions` setting. Folder item counts arrive async.
    fn bind_captions(&self, label: &gtk::Label, info: &gio::FileInfo) {
        unbind_captions(label);
        let kinds: Vec<String> = self
            .imp()
            .settings
            .strv("captions")
            .iter()
            .map(|s| s.to_string())
            .filter(|s| s != "none")
            .collect();
        if kinds.is_empty() {
            label.set_visible(false);
            return;
        }
        let lines: Vec<Option<String>> =
            kinds.iter().map(|k| file_utils::caption(info, k)).collect();
        let render = |lines: &[Option<String>]| {
            lines
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        label.set_text(&render(&lines));
        label.set_visible(true);
        if let Some(idx) = kinds.iter().position(|k| k == "size")
            && file_utils::is_dir(info)
            && crate::prefs::counts_for(&file_utils::file_of(info))
        {
            let dir = file_utils::file_of(info);
            let (fut, handle) = futures_util::future::abortable(glib::clone!(
                #[weak]
                label,
                async move {
                    let count = count_children(&dir).await;
                    let mut lines = lines;
                    lines[idx] = count.map(file_utils::items_string);
                    label.set_text(
                        &lines
                            .iter()
                            .flatten()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            ));
            unsafe { label.set_data("count-abort", handle) };
            glib::spawn_future_local(async move {
                let _ = fut.await;
            });
        }
    }

    /// Icon now, thumbnail later (cancelled on unbind); the lock emblem for files that
    /// cannot be read or changed.
    pub(crate) fn bind_icon(&self, image: &gtk::Image, emblem: &gtk::Image, info: &gio::FileInfo) {
        unbind_icon(image);
        set_emblem(
            emblem,
            file_utils::is_locked(info, self.imp().can_write.get()),
        );
        image.set_from_gicon(&crate::file_utils::icon_of(info));
        if info.is_hidden() || info.is_backup() {
            image.add_css_class("hidden-file");
        } else {
            image.remove_css_class("hidden-file");
        }
        let info = info.clone();
        let (fut, handle) = futures_util::future::abortable(glib::clone!(
            #[weak]
            image,
            async move {
                if let Some(texture) = crate::thumbnails::load(&info).await {
                    image.set_paintable(Some(&texture));
                    image.add_css_class("file-thumbnail");
                }
            }
        ));
        unsafe { image.set_data("thumb-abort", handle) };
        glib::spawn_future_local(async move {
            let _ = fut.await;
        });
    }

    /// Select `files` once the directory finished loading (used by FileManager1.ShowItems).
    pub fn select_files_when_loaded(&self, files: Vec<gio::File>) {
        let view = self.clone();
        glib::spawn_future_local(async move {
            let model = view.model();
            while model.loading() {
                glib::timeout_future(std::time::Duration::from_millis(50)).await;
            }
            let sel = model.selection();
            sel.unselect_all();
            let mut first = None;
            for f in &files {
                if let Some(pos) = model.position_of(f) {
                    sel.select_item(pos, false);
                    first.get_or_insert(pos);
                }
            }
            if let Some(pos) = first {
                view.imp()
                    .grid_view
                    .scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
                view.imp()
                    .miller_list
                    .scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
                view.imp()
                    .column_view
                    .scroll_to(pos, None, gtk::ListScrollFlags::FOCUS, None);
            }
        });
    }

    /// Drag the selection out of the view. The source sits above the list and captures
    /// the press, because the list's own rubberband gesture claims it otherwise; a press
    /// that misses every row is left alone so rubberband selection still works.
    fn setup_drag_source(&self) {
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

    /// A press on empty space takes the focus and drops the selection. GTK hands the focus
    /// to list items only, so without this the keys bound to the view -- Ctrl+A, Delete,
    /// F2 -- stay dead until a file is clicked.
    fn setup_background_click(&self) {
        let click = gtk::GestureClick::builder()
            .button(0)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, _, x, y| {
                let stack = view.imp().stack.clone().upcast::<gtk::Widget>();
                if cell_at(&stack, x, y).is_some() || view.in_side_column(x, y) {
                    return;
                }
                view.grab_view_focus();
                let held = gesture.current_event_state();
                let selecting = held.contains(gdk::ModifierType::CONTROL_MASK)
                    || held.contains(gdk::ModifierType::SHIFT_MASK);
                // A right click keeps the selection; the menu it opens acts on the folder.
                if !selecting && gesture.current_button() != gdk::BUTTON_SECONDARY {
                    view.model().selection().unselect_all();
                }
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
        target.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            gtk::gdk::DragAction::empty(),
            move |t, _, _| match view.cell_folder(&cell) {
                Some(_) => preferred_action(t),
                None => gtk::gdk::DragAction::empty(),
            }
        ));
        target.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            gtk::gdk::DragAction::empty(),
            move |t, _, _| match view.cell_folder(&cell) {
                Some(_) => preferred_action(t),
                None => gtk::gdk::DragAction::empty(),
            }
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            false,
            move |t, value, x, y| {
                let Some(folder) = view.cell_folder(&cell) else {
                    return false;
                };
                view.drop_files(t, value, &folder, x, y)
            }
        ));
        cell.add_controller(target);
    }

    /// The folder a cell currently shows, if it is one.
    fn cell_folder(&self, cell: &gtk::Widget) -> Option<gio::File> {
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
    fn ask_drop_action(&self, target: &gtk::DropTarget, x: f64, y: f64) {
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
    fn run_drop(
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

    /// Re-apply the cell state that lives outside the file info: lock emblems, cut
    /// dimming, stars. The rows keep their objects, so the factories never rebind them.
    pub(crate) fn refresh_cells(&self) {
        let imp = self.imp();
        let writable = imp.can_write.get();
        let roots: [gtk::Widget; 3] = [
            imp.grid_view.clone().upcast(),
            imp.column_view.clone().upcast(),
            imp.miller_list.clone().upcast(),
        ];
        for root in roots {
            each_cell(&root, &mut |cell| {
                let Some(info) = cell_position(cell).and_then(|p| self.model().info_at(p)) else {
                    return;
                };
                set_cut(cell, &info);
                if let Some(button) = cell.downcast_ref::<gtk::Button>() {
                    set_star(
                        button,
                        crate::starred::is_starred(&file_utils::file_of(&info)),
                    );
                } else if let Some(emblem) = cell_emblem(cell) {
                    set_emblem(&emblem, file_utils::is_locked(&info, writable));
                }
            });
        }
    }

    pub fn grab_view_focus(&self) {
        let imp = self.imp();
        match imp.stack.visible_child_name().as_deref() {
            Some("grid") => imp.grid_view.grab_focus(),
            Some("columns") => imp.miller_list.grab_focus(),
            _ => imp.column_view.grab_focus(),
        };
    }

    fn setup_grid_factory(&self) {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let image = gtk::Image::builder()
                .pixel_size(view.icon_size())
                .css_classes(["spiral-image"])
                .build();
            view.bind_property("icon-size", &image, "pixel-size")
                .sync_create()
                .build();
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .lines(3)
                .max_width_chars(1)
                .justify(gtk::Justification::Center)
                .build();
            view.bind_property("icon-size", &label, "width-request")
                .sync_create()
                .build();
            let captions = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .lines(3)
                .max_width_chars(1)
                .justify(gtk::Justification::Center)
                .visible(false)
                .css_classes(["caption", "dim-label"])
                .build();
            view.bind_property("icon-size", &captions, "width-request")
                .sync_create()
                .build();
            let labels = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            labels.append(&label);
            labels.append(&captions);
            // Icon between two emblem-wide margins, the lock stacked at the top of the
            // right one: the Nautilus grid cell geometry.
            image.set_margin_start(EMBLEM_MARGIN);
            image.set_hexpand(true);
            let emblem = emblem_image();
            emblem.set_width_request(EMBLEM_MARGIN);
            emblem.set_valign(gtk::Align::Start);
            let icon_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            icon_row.append(&image);
            icon_row.append(&emblem);
            let bx = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .css_classes(["spiral-view-cell"])
                .build();
            bx.append(&icon_row);
            bx.append(&labels);
            item.set_child(Some(&bx));
            remember_list_item(&bx, item);
            view.setup_cell_dnd(&bx);
        });
        let view = self.downgrade();
        factory.connect_bind(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let bx = item.child().unwrap();
            let icon_row = bx.first_child().unwrap();
            let image = icon_row.first_child().and_downcast::<gtk::Image>().unwrap();
            let emblem = icon_row.last_child().and_downcast::<gtk::Image>().unwrap();
            let labels = bx.last_child().unwrap();
            let label = labels.first_child().and_downcast::<gtk::Label>().unwrap();
            let captions = labels.last_child().and_downcast::<gtk::Label>().unwrap();
            view.bind_icon(&image, &emblem, &info);
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            label.set_tooltip_text(Some(&info.display_name()));
            item.set_accessible_label(&info.display_name());
            view.bind_captions(&captions, &info);
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(bx) = item.child() else { return };
            if let Some(image) = bx
                .first_child()
                .and_then(|row| row.first_child())
                .and_downcast::<gtk::Image>()
            {
                unbind_icon(&image);
            }
            if let Some(captions) = bx
                .last_child()
                .and_then(|l| l.last_child())
                .and_downcast::<gtk::Label>()
            {
                unbind_captions(&captions);
            }
        });
        self.imp().grid_view.set_factory(Some(&factory));
    }

    fn setup_columns(&self) {
        let cv = &self.imp().column_view;
        let name_factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        name_factory.connect_setup(move |_, item| {
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
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build(),
            );
            bx.append(&emblem_image());
            // Folders unfold in place when the tree preference is on.
            let expander = gtk::TreeExpander::builder().child(&bx).build();
            item.set_child(Some(&expander));
            remember_list_item(&bx, item);
            view.setup_cell_dnd(&bx);
        });
        let view = self.downgrade();
        name_factory.connect_bind(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
            expander.set_list_row(item.item().and_downcast::<gtk::TreeListRow>().as_ref());
            expander.set_hide_expander(!crate::prefs::tree_view());
            let bx = expander.child().unwrap();
            let image = bx.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = image.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let emblem = bx.last_child().and_downcast::<gtk::Image>().unwrap();
            view.bind_icon(&image, &emblem, &info);
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
        });
        name_factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
                return;
            };
            expander.set_list_row(None);
            if let Some(image) = expander
                .child()
                .and_then(|b| b.first_child())
                .and_downcast::<gtk::Image>()
            {
                unbind_icon(&image);
            }
        });
        let name_col = gtk::ColumnViewColumn::new(Some(&gettext("Name")), Some(name_factory));
        name_col.set_expand(true);
        cv.append_column(&name_col);

        // `tip` fills in what an abbreviated column drops, on hover.
        let text_col = |title: String,
                        xalign: f32,
                        ellipsize: gtk::pango::EllipsizeMode,
                        f: fn(&gio::FileInfo) -> String,
                        tip: Option<fn(&gio::FileInfo) -> String>| {
            let factory = gtk::SignalListItemFactory::new();
            let view = self.downgrade();
            factory.connect_setup(move |_, item| {
                let Some(view) = view.upgrade() else { return };
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let label = gtk::Label::builder()
                    .xalign(xalign)
                    .ellipsize(ellipsize)
                    // Let the column width decide, not the longest value in it.
                    .max_width_chars(1)
                    .css_classes(["spiral-view-cell", "dim-label"])
                    .build();
                if xalign > 0.5 {
                    label.add_css_class("numeric");
                }
                item.set_child(Some(&label));
                remember_list_item(&label, item);
                view.setup_cell_dnd(&label);
            });
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                    return;
                };
                let label = item.child().and_downcast::<gtk::Label>().unwrap();
                set_cut(&label, &info);
                label.set_text(&f(&info));
                label.set_tooltip_text(tip.map(|t| t(&info)).as_deref());
            });
            gtk::ColumnViewColumn::new(Some(&title), Some(factory))
        };
        let text = |key: &str| -> fn(&gio::FileInfo) -> String {
            match key {
                "size" => file_utils::size_string,
                "type" => file_utils::short_type_string,
                "modified" => file_utils::modified_string,
                "accessed" => file_utils::accessed_string,
                "created" => file_utils::created_string,
                "owner" => |i| file_utils::caption(i, "owner").unwrap_or_default(),
                "group" => |i| file_utils::caption(i, "group").unwrap_or_default(),
                _ => |i| file_utils::permissions_string(i).unwrap_or_default(),
            }
        };
        // Widths that fit the usual values; the name column keeps the rest.
        let width = |key: &str| match key {
            "size" => 88,
            "owner" | "group" => 104,
            "permissions" => 112,
            "type" => 92,
            _ => 148,
        };
        let mut columns: Vec<(&'static str, gtk::ColumnViewColumn)> = Vec::new();
        for (key, title) in file_utils::optional_columns() {
            let col = text_col(
                title,
                if key == "size" { 1.0 } else { 0.0 },
                gtk::pango::EllipsizeMode::End,
                text(key),
                (key == "type").then_some(file_utils::type_string as fn(&gio::FileInfo) -> String),
            );
            col.set_fixed_width(width(key));
            col.set_resizable(true);
            cv.append_column(&col);
            columns.push((key, col));
        }
        let star_col = self.star_column();
        cv.append_column(&star_col);
        columns.push(("star", star_col));
        // Search results come from anywhere below the folder; say where.
        let location_col = text_col(
            gettext("Location"),
            0.0,
            gtk::pango::EllipsizeMode::Middle,
            file_utils::location_of,
            Some(file_utils::location_of),
        );
        location_col.set_visible(false);
        location_col.set_expand(true);
        cv.insert_column(1, &location_col);
        self.imp().model.connect_searching_notify(glib::clone!(
            #[weak]
            location_col,
            move |m| location_col.set_visible(m.searching())
        ));
        let (size_col, type_col, mod_col) = (
            columns[0].1.clone(),
            columns[1].1.clone(),
            columns[2].1.clone(),
        );
        self.imp().columns.replace(columns);
        self.apply_visible_columns();
        self.imp().settings.connect_changed(
            Some("visible-columns"),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_, _| view.apply_visible_columns()
            ),
        );

        // Header clicks drive FolderModel sort props instead of the column view's own sorter.
        for (col, key) in [
            (&name_col, SortKey::Name),
            (&size_col, SortKey::Size),
            (&type_col, SortKey::Type),
            (&mod_col, SortKey::Modified),
        ] {
            let sorter = gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal);
            col.set_sorter(Some(&sorter));
            unsafe { col.set_data("sort-key", key) };
        }
        let cv_sorter = cv.sorter().and_downcast::<gtk::ColumnViewSorter>().unwrap();
        cv_sorter.connect_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |s, _| {
                if view.imp().syncing_header.get() {
                    return;
                }
                let Some(col) = s.primary_sort_column() else {
                    return;
                };
                let key = unsafe { *col.data::<SortKey>("sort-key").unwrap().as_ref() };
                let reversed = s.primary_sort_order() == gtk::SortType::Descending;
                view.set_sort(key, reversed);
            }
        ));
        // The header arrow follows the model, whichever way the order was set.
        for prop in ["sort-key", "sort-reversed"] {
            self.imp().model.connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_, _| view.sync_sort_header()
                ),
            );
        }
        self.sync_sort_header();
    }

    /// Show the columns the `visible-columns` key names; the name column always stays.
    fn apply_visible_columns(&self) {
        let on = self.imp().settings.strv("visible-columns");
        for (key, col) in self.imp().columns.borrow().iter() {
            col.set_visible(on.iter().any(|k| k == key));
        }
    }

    /// A star per row that toggles the favourite, like the Nautilus star column.
    fn star_column(&self) -> gtk::ColumnViewColumn {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(view) = view.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let button = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .css_classes(["flat", "circular", "spiral-star"])
                .build();
            button.connect_clicked(glib::clone!(
                #[weak]
                item,
                move |button| {
                    let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o))
                    else {
                        return;
                    };
                    let file = file_utils::file_of(&info);
                    let starred = !crate::starred::is_starred(&file);
                    crate::starred::set_starred(&file, starred);
                    set_star(button, starred);
                }
            ));
            item.set_child(Some(&button));
            remember_list_item(&button, item);
            view.setup_cell_dnd(&button);
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let button = item.child().and_downcast::<gtk::Button>().unwrap();
            set_cut(&button, &info);
            set_star(
                &button,
                crate::starred::is_starred(&file_utils::file_of(&info)),
            );
        });
        gtk::ColumnViewColumn::new(Some(&gettext("Star")), Some(factory))
    }

    fn sync_sort_header(&self) {
        let imp = self.imp();
        let cv = &imp.column_view;
        let key = imp.model.sort_key();
        let order = if imp.model.sort_reversed() {
            gtk::SortType::Descending
        } else {
            gtk::SortType::Ascending
        };
        let col =
            cv.columns().iter::<gtk::ColumnViewColumn>().flatten().find(
                |c| unsafe { c.data::<SortKey>("sort-key").map(|k| *k.as_ref()) } == Some(key),
            );
        imp.syncing_header.set(true);
        // Clear first: the column view would otherwise keep the old column as a secondary sort.
        cv.sort_by_column(None::<&gtk::ColumnViewColumn>, order);
        cv.sort_by_column(col.as_ref(), order);
        imp.syncing_header.set(false);
    }
}
