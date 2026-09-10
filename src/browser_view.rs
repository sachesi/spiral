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
        pub list_scroll: TemplateChild<gtk::ScrolledWindow>,
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
        pub open_section: TemplateChild<gio::Menu>,
        /// How many items the template put in `open_section`; what follows is ours.
        pub open_items: Cell<u32>,
        #[template_child]
        pub tags_section: TemplateChild<gio::Menu>,
        /// The row of tag dots the item menu shows, kept between menus; the popover drops
        /// it whenever it builds a menu afresh, and it is put back.
        pub tag_picker: RefCell<Option<gtk::Box>>,
        #[template_child]
        pub background_menu: TemplateChild<gio::MenuModel>,
        #[template_child]
        pub new_section: TemplateChild<gio::Menu>,
        #[template_child]
        pub drop_menu: TemplateChild<gio::MenuModel>,
        /// Files waiting for the drop menu to say what to do with them.
        pub pending_drop: RefCell<Option<(Vec<gio::File>, gio::File)>>,
        /// The folder a drag is resting on, and the wait before it springs open.
        pub hover_pos: Cell<Option<u32>>,
        pub hover_timer: RefCell<Option<glib::SourceId>>,
        /// The row marked as the one a drag is over, to unmark when it moves on.
        pub drop_row: RefCell<Option<gtk::Widget>>,
        /// The columns of the Miller view beside the folder being viewed: the path
        /// that leads to it, then the folder the selection points at.
        pub side_columns: RefCell<Vec<crate::miller::SideColumn>>,
        pub preview_column: RefCell<Option<crate::miller::SideColumn>>,
        /// Bumped per selection so only the last of a run of them lists a folder.
        pub preview_gen: Cell<u64>,
        /// Set while a rebuild of the strip is waiting for the current change to end.
        pub columns_pending: Cell<bool>,
        /// Set while the columns of the list are waiting to be fitted to a new width.
        pub fit_pending: Cell<bool>,
        /// Set while the bar and the actions are waiting to be told the selection changed.
        pub selection_pending: Cell<bool>,
        /// Whether the selection among search results is the first result, picked by the
        /// view rather than by hand; set while the view is picking it; set while a pick
        /// waits for the change of the results to end.
        pub first_picked: Cell<bool>,
        pub picking: Cell<bool>,
        pub pick_pending: Cell<bool>,
        /// Where a drag is hovering over the strip, and the frame callback that pushes
        /// the strip along and marks the column the drop would land in.
        pub drag_at: Cell<(f64, f64)>,
        pub drag_tick: RefCell<Option<gtk::TickCallbackId>>,

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

        pub history: RefCell<Vec<super::Visit>>,
        pub history_pos: Cell<usize>,
        /// Where mounting the location stands, so a server that turns it down is not asked
        /// again each time the folder is listed, and the page can say what goes on meanwhile.
        pub mounting: RefCell<super::Mounting>,
        /// A location and the name it was listed under where it was opened from: a server
        /// found on the network is reached by an address, and has no name of its own until
        /// a share on it is mounted.
        pub given_name: RefCell<Option<(gio::File, String)>>,
        pub settings: gio::Settings,
        /// The handler on the display's clipboard, which outlives the view.
        pub clipboard_handler: RefCell<Option<glib::SignalHandlerId>>,
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
            // Inside the trash, where nothing can be trashed, Delete asks to delete for good.
            klass.add_binding_action(Key::KP_Delete, M::empty(), "view.delete-from-trash");
            klass.add_binding_action(Key::Delete, M::empty(), "view.delete-from-trash");
            klass.add_binding_action(Key::Delete, M::SHIFT_MASK, "view.delete");
            klass.add_binding_action(Key::F2, M::empty(), "view.rename");
            klass.add_binding_action(Key::c, M::CONTROL_MASK, "view.copy");
            klass.add_binding_action(Key::x, M::CONTROL_MASK, "view.cut");
            klass.add_binding_action(Key::v, M::CONTROL_MASK, "view.paste");
            klass.add_binding_action(Key::a, M::CONTROL_MASK, "view.select-all");
            klass.add_binding_action(Key::s, M::CONTROL_MASK, "view.select-pattern");
            klass.add_binding_action(
                Key::i,
                M::CONTROL_MASK | M::SHIFT_MASK,
                "view.invert-selection",
            );
            klass.add_binding_action(Key::n, M::CONTROL_MASK | M::SHIFT_MASK, "view.new-folder");
            klass.add_binding_action(Key::Return, M::ALT_MASK, "view.properties");
            klass.add_binding_action(Key::i, M::CONTROL_MASK, "view.properties");
            klass.add_binding_action(Key::o, M::CONTROL_MASK, "view.open");
            for enter in [Key::Return, Key::KP_Enter] {
                klass.add_binding_action(enter, M::CONTROL_MASK, "view.open-new-tab");
                klass.add_binding_action(enter, M::SHIFT_MASK, "view.open-new-window");
            }
            klass.add_binding_action(
                Key::o,
                M::CONTROL_MASK | M::ALT_MASK,
                "view.open-item-location",
            );
            // Not "view.create-link", which only stands while the menu offers it.
            klass.add_binding_action(Key::m, M::CONTROL_MASK, "view.link");
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
                list_scroll: Default::default(),
                columns_scroll: Default::default(),
                columns_box: Default::default(),
                miller_scroll: Default::default(),
                miller_list: Default::default(),
                side_columns: Default::default(),
                preview_column: Default::default(),
                preview_gen: Default::default(),
                columns_pending: Default::default(),
                fit_pending: Default::default(),
                selection_pending: Default::default(),
                first_picked: Default::default(),
                picking: Default::default(),
                pick_pending: Default::default(),
                drag_at: Default::default(),
                drag_tick: Default::default(),
                error_page: Default::default(),
                empty_page: Default::default(),
                floating_bar: Default::default(),
                floating_spinner: Default::default(),
                floating_primary: Default::default(),
                floating_details: Default::default(),
                item_menu: Default::default(),
                open_section: Default::default(),
                open_items: Default::default(),
                tags_section: Default::default(),
                tag_picker: Default::default(),
                background_menu: Default::default(),
                new_section: Default::default(),
                drop_menu: Default::default(),
                pending_drop: Default::default(),
                hover_pos: Default::default(),
                hover_timer: Default::default(),
                drop_row: Default::default(),
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
                mounting: Default::default(),
                given_name: Default::default(),
                history_pos: Default::default(),
                settings: gio::Settings::new(crate::config::APP_ID),
                clipboard_handler: Default::default(),
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

        fn dispose(&self) {
            if let Some(id) = self.clipboard_handler.take() {
                self.obj().clipboard().disconnect(id);
            }
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
                            obj.end_hover();
                            gtk::gdk::DragAction::empty()
                        } else {
                            obj.hover_folder(x, y);
                            preferred_action(t)
                        }
                    }
                );
                target.connect_enter(action.clone());
                target.connect_motion(action);
                target.connect_leave(glib::clone!(
                    #[weak]
                    obj,
                    move |_| obj.end_hover()
                ));
                target.connect_drop(glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    false,
                    move |t, value, x, y| match obj.location() {
                        // The folder under the pointer takes the drop. Cells carry targets
                        // of their own, but only the grid's ever see a drag: in a column
                        // view the crossing never reaches them, so the view looks for itself.
                        Some(loc) if !obj.in_side_column(x, y) => {
                            let dest = obj.folder_at(x, y).unwrap_or(loc);
                            obj.drop_files(t, value, &dest, x, y)
                        }
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
            // Turning the column view off puts the views that are in it back in the list.
            self.settings.connect_changed(
                Some("use-column-view"),
                glib::clone!(
                    #[weak]
                    obj,
                    move |_, _| obj.set_view_mode(obj.view_mode())
                ),
            );
            // The dots beside the names come and go with the tags and the preference.
            for key in ["use-tags", "tags"] {
                self.settings.connect_changed(
                    Some(key),
                    glib::clone!(
                        #[weak]
                        obj,
                        move |_, _| {
                            obj.refresh_cells();
                            obj.update_action_state();
                        }
                    ),
                );
            }
            // Caption lines are read as a grid cell is bound, so changing them has to
            // build the cells again.
            self.settings.connect_changed(
                Some("captions"),
                glib::clone!(
                    #[weak]
                    obj,
                    move |_, _| obj.setup_grid_factory()
                ),
            );
            for key in crate::prefs::VIEW_KEYS {
                self.settings.connect_changed(
                    Some(key),
                    glib::clone!(
                        #[weak]
                        obj,
                        move |_, _| {
                            obj.reload();
                            obj.refresh_columns();
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
            self.settings
                .bind("grid-zoom", &*obj, "icon-size")
                .flags(gio::SettingsBindFlags::GET)
                .build();
            self.settings
                .bind("list-zoom", &*obj, "list-icon-size")
                .flags(gio::SettingsBindFlags::GET)
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
            let (sort_key, sort_reversed) = obj.sort_keys();
            for key in [sort_key, sort_reversed] {
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
                .flags(gio::SettingsBindFlags::GET)
                .build();

            obj.setup_grid_factory();
            obj.setup_columns();
            obj.setup_miller();
            if let Some(hadj) = self.grid_view.hadjustment() {
                hadj.connect_page_size_notify(glib::clone!(
                    #[weak]
                    obj,
                    move |_| obj.fit_grid_columns()
                ));
            }
            obj.connect_icon_size_notify(|obj| obj.fit_grid_columns());

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
            self.model.connect_error_message_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.mount_location()
            ));
            self.model.selection().connect_items_changed(glib::clone!(
                #[weak]
                obj,
                move |_, _, _, _| {
                    obj.update_stack();
                    obj.queue_selection_update();
                    obj.queue_pick_first_result();
                }
            ));
            self.model
                .selection()
                .connect_selection_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        let imp = obj.imp();
                        if !imp.picking.get() {
                            imp.first_picked.set(false);
                        }
                        obj.queue_selection_update();
                    }
                ));
            self.model.connect_loading_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.queue_selection_update()
            ));
            obj.update_stack();
        }
    }

    impl WidgetImpl for BrowserView {}
    impl BoxImpl for BrowserView {}

    impl BrowserView {
        fn set_view_mode(&self, mode: ViewMode) {
            // With the column view turned off the list stands in for it, whatever the
            // settings or a folder remember from when it was on.
            let mode = match mode {
                ViewMode::Columns if !crate::prefs::column_view() => ViewMode::List,
                mode => mode,
            };
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
        if file_utils::content_type_of(&info)
            .is_some_and(|ct| ct.starts_with("image/") || ct.starts_with("video/"))
        {
            media += 1;
        }
    }
    files >= 4 && media * 2 >= files
}

// Item counts already worked out, oldest first, keyed by the folder and the time it last
// changed. Counting means enumerating the folder in full, and a caption is bound afresh
// every time its row is scrolled back into view.
thread_local! {
    static COUNTS: RefCell<CountCache> = RefCell::new(CountCache::default());
}

/// The folder in `dir` that holds `file`, or `file` itself when it is in `dir`; `None`
/// when `file` is not below `dir`.
fn child_toward(dir: &gio::File, file: &gio::File) -> Option<gio::File> {
    let mut child = file.clone();
    loop {
        let parent = child.parent()?;
        if parent.equal(dir) {
            return Some(child);
        }
        child = parent;
    }
}

/// A step in a tab's history: the folder, and the search it was showing and what was
/// selected when it was left, which going back to it brings back.
pub struct Visit {
    file: gio::File,
    search: Option<Search>,
    selected: Vec<gio::File>,
}

/// A search as it stood: the words, the filters, and what it had found. Only the last
/// search left in a tab keeps what it found, since that can be a great many files; one
/// further back searches again.
#[derive(Clone)]
struct Search {
    text: String,
    kind: String,
    date: String,
    matching: String,
    hits: Option<Vec<gio::FileInfo>>,
    /// Whether the search had come to its end, or goes on when shown again.
    finished: bool,
}

/// Where mounting the location of a view stands: nothing asked, a server being reached, or
/// one that answered with the reason it kept in `Failed`.
#[derive(Default)]
pub enum Mounting {
    #[default]
    Idle,
    Underway,
    Failed(String),
}

#[derive(Default)]
struct CountCache {
    seen: std::collections::HashMap<(String, i64), u64>,
    order: std::collections::VecDeque<(String, i64)>,
}

const COUNT_CACHE_ENTRIES: usize = 4096;

/// Number of direct children of `dir` as of `stamp`, the time it last changed.
async fn count_children_at(dir: &gio::File, stamp: i64) -> Option<u64> {
    let key = (dir.uri().to_string(), stamp);
    if let Some(n) = COUNTS.with(|c| c.borrow().seen.get(&key).copied()) {
        return Some(n);
    }
    let n = count_children(dir).await?;
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.seen.insert(key.clone(), n).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > COUNT_CACHE_ENTRIES {
            let Some(oldest) = c.order.pop_front() else {
                break;
            };
            c.seen.remove(&oldest);
        }
    });
    Some(n)
}

/// The application a file would open in, which is what decides whether a terminal is needed.
async fn default_app(file: &gio::File) -> Option<gio::AppInfo> {
    let info = file
        .query_info_future(
            "standard::content-type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .ok()?;
    gio::AppInfo::default_for_type(&file_utils::content_type_of(&info)?, !file.is_native())
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

/// Columns the grid may have at most, whatever fits; the `max-columns` of the template.
const GRID_MAX_COLUMNS: i32 = 20;

/// What the name column is left with before the columns beside it start giving way.
const NAME_MIN_WIDTH: i32 = 220;

/// The star column: one flat button wide.
const STAR_WIDTH: i32 = 40;

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

/// A tag's colour as a dot; CSS paints it.
pub(crate) fn tag_dot(color: &str) -> gtk::Image {
    gtk::Image::builder()
        .css_classes(["spiral-tag-dot", &crate::tags::dot_class(color)])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build()
}

/// Show the file's tags on its cell as a wash of the first colour: across the row in
/// the list and column views, behind the name in the grid, where `name` is the label.
/// A file that carries tags is made known to the index while it is here, in case it was
/// tagged elsewhere or moved by another program.
pub(crate) fn bind_tags(
    cell: &impl IsA<gtk::Widget>,
    name: Option<&gtk::Label>,
    info: &gio::FileInfo,
) {
    let names = if crate::tags::enabled() {
        crate::tags::of_info(info)
    } else {
        Vec::new()
    };
    let color = names
        .iter()
        .find_map(|n| crate::tags::color_of(n).filter(|c| !c.is_empty()));
    match name {
        Some(label) => wash(label.upcast_ref(), color.as_deref()),
        None => wash_row(cell.upcast_ref(), color),
    }
    if !names.is_empty() {
        crate::tags::note(&file_utils::file_of(info), &names);
    }
}

/// Give `widget` the wash class of `color`, or none.
fn wash(widget: &gtk::Widget, color: Option<&str>) {
    for class in widget.css_classes() {
        if class.starts_with("spiral-tag-wash") {
            widget.remove_css_class(&class);
        }
    }
    if let Some(c) = color {
        widget.add_css_class("spiral-tag-wash");
        widget.add_css_class(&crate::tags::wash_class(c));
    }
}

/// Wash the row the cell sits in. A cell bound before it is in the window belongs to a
/// row widget GTK is still holding as a floating reference, and walking up to it from
/// here would sink that reference and drop the row; such a cell is done once the main
/// loop comes round, by which time the row is in the list.
fn wash_row(cell: &gtk::Widget, color: Option<String>) {
    let apply = move |cell: &gtk::Widget| {
        if let Some(row) = row_widget(cell) {
            wash(&row, color.as_deref());
        }
    };
    if cell.root().is_some() {
        apply(cell);
    } else {
        glib::idle_add_local_once(glib::clone!(
            #[weak]
            cell,
            move || apply(&cell)
        ));
    }
}

/// The name label of a grid cell, which carries its wash; a list cell has none.
fn cell_name(cell: &gtk::Widget) -> Option<gtk::Label> {
    let mut child = cell.first_child();
    while let Some(c) = child {
        if c.has_css_class("spiral-grid-name") {
            return c.downcast().ok();
        }
        if let Some(found) = cell_name(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

pub(crate) fn unbind_icon(image: &gtk::Image) {
    if let Some(handle) =
        unsafe { image.steal_data::<futures_util::future::AbortHandle>("thumb-abort") }
    {
        handle.abort();
    }
    if let Some(id) = unsafe { image.steal_data::<glib::SignalHandlerId>("thumb-map") } {
        image.disconnect(id);
    }
    image.remove_css_class("file-thumbnail");
}

/// Wait until the folder has stopped listing. A big folder is put in order once its
/// listing ends, so a thumbnail asked for before that is for a file about to move somewhere
/// else: opening a folder of fifty thousand files spent every one of its first requests
/// that way, on files that were nowhere near the screen by the time they were made. A
/// search is not waited for, since its results arrive for as long as it runs.
async fn folder_listed(model: &FolderModel) {
    if !model.loading() || model.searching() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = model.connect_loading_notify(move |model| {
        if !model.loading()
            && let Some(tx) = tx.borrow_mut().take()
        {
            let _ = tx.send(());
        }
    });
    let _ = rx.await;
    model.disconnect(id);
}

/// Wait until `image` is on screen. The list widgets bind far more cells than they show:
/// they keep a pool of them, and the views on the stack pages that are not showing bind as
/// well, so binding a cell says nothing about anyone ever looking at it. A folder of a few
/// thousand pictures was handing twenty times as many files to a decoder as it displayed.
async fn on_screen(image: &gtk::Image) {
    if image.is_mapped() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = image.connect_map(move |_| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(());
        }
    });
    unsafe { image.set_data("thumb-map", id) };
    let _ = rx.await;
    if let Some(id) = unsafe { image.steal_data::<glib::SignalHandlerId>("thumb-map") } {
        image.disconnect(id);
    }
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

/// How long a drag has to rest on a folder before it springs open, as in GNOME Files.
const HOVER_OPEN_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

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

/// Cells keep a weak link to their `ListItem`: its position is live, unlike a cached
/// number, when items are inserted above it.
/// A filled star for a favourite, a hollow one otherwise.
fn set_star(button: &gtk::Button, starred: bool) {
    button.set_icon_name(if starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    // Some icon themes draw the empty star as solidly as the full one, which leaves the
    // column saying nothing; faded, the two are told apart whatever the theme draws.
    button.set_opacity(if starred { 1.0 } else { 0.45 });
}

/// What the star button offers, read off the star it shows when the pointer asks.
fn star_tooltip(button: &gtk::Button) {
    button.set_has_tooltip(true);
    button.connect_query_tooltip(|button, _, _, _, tooltip| {
        let starred = button.icon_name().as_deref() == Some("starred-symbolic");
        tooltip.set_text(Some(&if starred {
            gettext("Remove from Starred")
        } else {
            gettext("Add to Starred")
        }));
        true
    });
}

/// The whole name on hover, for a label that had to cut it. Decided when the pointer
/// asks: setting a tooltip on a visible widget makes GTK look for the pointer through
/// the widget tree, a style lookup per cell per bind, which a scrolling grid paid for
/// every cell that came into view.
pub(crate) fn name_tooltip(label: &gtk::Label) {
    label.set_has_tooltip(true);
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        let cut = label.layout().is_ellipsized();
        if cut {
            tooltip.set_text(Some(&label.text()));
        }
        cut
    });
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
pub(crate) fn cell_at(root: &gtk::Widget, x: f64, y: f64) -> Option<gtk::Widget> {
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
        // A dialog searches the folder it is showing, not the tree below it: what is being
        // asked for is a file in a folder, and a walk of the disk is not part of the answer.
        view.model().set_search_recursive(false);
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

    /// Where the sort order is kept: a chooser has keys of its own, so ordering a dialog
    /// leaves the file manager's windows as they were, and is there again next time.
    fn sort_keys(&self) -> (&'static str, &'static str) {
        if self.chooser_mode() {
            ("chooser-sort-key", "chooser-sort-reversed")
        } else {
            ("sort-key", "sort-reversed")
        }
    }

    fn apply_global_sort(&self) {
        let imp = self.imp();
        let (key_name, reversed_name) = self.sort_keys();
        let key = SortKey::from_nick(&imp.settings.string(key_name)).unwrap_or_default();
        imp.model.set_sort_key(key);
        imp.model
            .set_sort_reversed(imp.settings.boolean(reversed_name));
    }

    /// Sort the current folder: remembered for this folder when views are remembered per
    /// folder, otherwise as the new global order. A chooser keeps its own order instead.
    pub fn set_sort(&self, key: SortKey, reversed: bool) {
        let imp = self.imp();
        imp.model.set_sort_key(key);
        imp.model.set_sort_reversed(reversed);
        match self.location() {
            _ if self.chooser_mode() => {
                let (key_name, reversed_name) = self.sort_keys();
                let _ = imp.settings.set_string(key_name, key.nick());
                let _ = imp.settings.set_boolean(reversed_name, reversed);
            }
            Some(dir) if crate::prefs::remember_view() => {
                imp.folder_sort.set(Some((key, reversed)));
                let value = format!("{}-{}", key.nick(), if reversed { "desc" } else { "asc" });
                remember(dir, "metadata::spiral-sort", value);
            }
            // Only the trash has the date to sort by, and every other folder would lose its
            // order to it: it stays with the trash, for as long as the trash is shown.
            Some(_) if key == SortKey::Trashed => imp.folder_sort.set(Some((key, reversed))),
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
        // next is how they are read, so a move that has nothing else to say leaves them
        // alone. Only a folder remembering a view of its own takes the pane out of them.
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
        if next == ViewMode::Columns && !crate::prefs::column_view() {
            return;
        }
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
        // Already there, unless it is showing a search: then the folder itself is a step
        // of its own, and Back returns to the search.
        if imp.model.location().is_some_and(|l| l.equal(file)) && imp.model.search_text().is_empty()
        {
            return;
        }
        self.remember_state();
        {
            let mut hist = imp.history.borrow_mut();
            let pos = imp.history_pos.get();
            if !hist.is_empty() {
                hist.truncate(pos + 1);
            }
            hist.push(Visit {
                file: file.clone(),
                search: None,
                selected: Vec::new(),
            });
            imp.history_pos.set(hist.len() - 1);
        }
        self.set_location_internal(file, None);
    }

    /// Keep the search on screen and the selection with the step of the history being
    /// left.
    fn remember_state(&self) {
        let imp = self.imp();
        let model = &imp.model;
        let text = model.search_text();
        let search = (!text.is_empty()).then(|| Search {
            text,
            kind: model.search_kind(),
            date: model.search_date(),
            matching: model.search_match(),
            hits: Some(model.search_hits()),
            finished: !model.loading(),
        });
        let selected = model.selected_files();
        let pos = imp.history_pos.get();
        let mut hist = imp.history.borrow_mut();
        if search.is_some() {
            for visit in hist.iter_mut() {
                if let Some(search) = visit.search.as_mut() {
                    search.hits = None;
                }
            }
        }
        if let Some(visit) = hist.get_mut(pos) {
            visit.search = search;
            visit.selected = selected;
        }
    }

    /// Show `file`, and `search` in it when the step of the history had one.
    fn set_location_internal(&self, file: &gio::File, search: Option<Search>) {
        let imp = self.imp();
        imp.mounting.replace(Mounting::Idle);
        imp.model.set_search_text("");
        imp.model.set_location(Some(file));
        if let Some(search) = search {
            imp.model.set_search_kind(search.kind);
            imp.model.set_search_date(search.date);
            imp.model.set_search_match(search.matching);
            match search.hits {
                Some(hits) => imp
                    .model
                    .show_search_hits(&search.text, &hits, search.finished),
                None => imp.model.set_search_text(search.text),
            }
        }
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
                view.reveal_position(0, gtk::ListScrollFlags::NONE);
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
        self.take_keyboard();
    }

    /// A folder change destroys every row, and with them whatever had the keyboard: the
    /// window is left with the focus nowhere in particular, and the file keys do nothing
    /// until something in the view is clicked. Take it back, unless it is in a text entry
    /// -- the search box, the location bar, a file name -- where typing is the point.
    fn take_keyboard(&self) {
        let focus = self.root().and_then(|root| root.focus());
        if focus.is_some_and(|w| w.is::<gtk::Editable>()) {
            return;
        }
        self.grab_view_focus();
    }

    /// Back a step; while searching, out of the search and into the folder it was in.
    pub fn go_back(&self) {
        let imp = self.imp();
        if imp.model.searching() {
            self.search_for("");
            return;
        }
        let pos = imp.history_pos.get();
        if pos == 0 {
            return;
        }
        self.remember_state();
        imp.history_pos.set(pos - 1);
        self.show_visit(pos - 1);
    }

    pub fn go_forward(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        if pos + 1 >= imp.history.borrow().len() {
            return;
        }
        self.remember_state();
        imp.history_pos.set(pos + 1);
        self.show_visit(pos + 1);
    }

    /// Show the step at `pos` with what was selected there. A step that had nothing
    /// selected and leads up from the folder being left selects the folder on the way.
    fn show_visit(&self, pos: usize) {
        let (file, search, mut selected) = {
            let hist = self.imp().history.borrow();
            let visit = &hist[pos];
            (
                visit.file.clone(),
                visit.search.clone(),
                visit.selected.clone(),
            )
        };
        if selected.is_empty()
            && search.is_none()
            && let Some(child) = self.location().and_then(|l| child_toward(&file, &l))
        {
            selected.push(child);
        }
        self.set_location_internal(&file, search);
        self.select_files_when_loaded(selected);
    }

    /// Backspace: out of the search, back to the search this folder was opened from, or
    /// up.
    pub fn go_back_or_up(&self) {
        let imp = self.imp();
        let pos = imp.history_pos.get();
        let from_search = pos > 0 && imp.history.borrow()[pos - 1].search.is_some();
        if from_search || imp.model.searching() {
            self.go_back();
        } else {
            self.go_up();
        }
    }

    /// Up to the parent, with the folder just left selected in it.
    pub fn go_up(&self) {
        let Some(here) = self.location() else { return };
        if let Some(parent) = here.parent() {
            self.go_to(&parent);
            self.select_files_when_loaded(vec![here]);
        }
    }

    /// Search the folder for `text`. With no text the folder comes back, with what was
    /// selected among the results selected in it: the result itself, or the folder in
    /// it that holds the result.
    pub fn search_for(&self, text: &str) {
        let model = self.model();
        if !text.is_empty() || !model.searching() {
            model.set_search_text(text);
            return;
        }
        let mut keep: Vec<gio::File> = Vec::new();
        if let Some(dir) = self.location() {
            for file in model.selected_files() {
                let file = child_toward(&dir, &file).unwrap_or(file);
                if !keep.iter().any(|f| f.equal(&file)) {
                    keep.push(file);
                }
            }
        }
        model.set_search_text("");
        self.select_files_when_loaded(keep);
    }

    /// While searching, keep the first result selected until another one is picked, so
    /// Enter in the search box opens it. The first result changes as results arrive in
    /// order, and the selection follows it. Not while the results are changing: the views
    /// are told of the change after the model, and a selection moved before that points
    /// them at rows they do not have yet.
    fn queue_pick_first_result(&self) {
        if self.imp().pick_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().pick_pending.set(false);
                view.pick_first_result();
            }
        ));
    }

    fn pick_first_result(&self) {
        let imp = self.imp();
        let model = &imp.model;
        if !model.searching() || model.n_items() == 0 {
            return;
        }
        let sel = model.selection();
        let set = sel.selection();
        let only_first = set.size() == 1 && set.contains(0);
        if !set.is_empty() && (!imp.first_picked.get() || only_first) {
            return;
        }
        imp.picking.set(true);
        sel.select_item(0, true);
        imp.picking.set(false);
        imp.first_picked.set(true);
    }

    /// Read the folder again, leaving the view where it was. The listing is thrown away
    /// and read from the start, which empties the model: the selection goes with it and
    /// every view below it comes back at the first row. Both are put back once the
    /// folder is in again, so a reload shows what was on screen before it.
    pub fn reload(&self) {
        let imp = self.imp();
        // Asked for again by hand: a share whose password was turned down is worth another
        // try, and so is one that has since come back.
        imp.mounting.replace(Mounting::Idle);
        let offset = self.view_adjustment().map_or(0.0, |adj| adj.value());
        let selected = imp.model.selected_files();
        let focused = self.view_has_focus();
        imp.model.reload();
        if offset == 0.0 && selected.is_empty() {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let model = view.model();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while model.loading() && std::time::Instant::now() < deadline {
                    glib::timeout_future(std::time::Duration::from_millis(50)).await;
                }
                let sel = model.selection();
                let found = model.positions_of(&selected);
                for &pos in &found {
                    sel.select_item(pos, false);
                }
                // The rows are new widgets, so the keyboard was left outside the view
                // while the folder was away; it belongs where it was.
                if focused && let Some(&pos) = found.first() {
                    view.reveal_position(pos, gtk::ListScrollFlags::FOCUS);
                    view.grab_view_focus();
                }
                view.restore_offset(offset).await;
            }
        ));
    }

    /// Scroll back to `offset`. A view only works out how tall it is once the rows that
    /// arrived have been laid out, so until then the value is clamped to the little it
    /// believes it has and has to be given again.
    async fn restore_offset(&self, offset: f64) {
        for _ in 0..20 {
            let Some(adj) = self.view_adjustment() else {
                return;
            };
            adj.set_value(offset);
            if adj.value() >= offset {
                return;
            }
            glib::timeout_future(std::time::Duration::from_millis(25)).await;
        }
    }

    /// The vertical adjustment of whichever view is on screen; the others have no model
    /// and nothing to scroll.
    fn view_adjustment(&self) -> Option<gtk::Adjustment> {
        let imp = self.imp();
        if imp.grid_view.model().is_some() {
            imp.grid_view.vadjustment()
        } else if imp.miller_list.model().is_some() {
            imp.miller_list.vadjustment()
        } else {
            imp.column_view.vadjustment()
        }
    }

    /// Whether the keyboard is on something inside this view.
    fn view_has_focus(&self) -> bool {
        self.root()
            .and_then(|root| root.focus())
            .is_some_and(|widget| widget.is_ancestor(self))
    }

    /// Open the item at `pos`: descend into folders, launch files.
    pub(crate) fn activate_position(&self, pos: u32) {
        let Some(info) = self.imp().model.info_at(pos) else {
            return;
        };
        let file = file_utils::file_of(&info);
        if file_utils::is_dir(&info) {
            self.go_to(&file);
        } else if let Some(target) = file_utils::target_of(&info) {
            self.open_target(target, file_utils::listed_as(&info));
        } else if self.chooser_mode() {
            self.emit_by_name::<()>("file-activated", &[&file]);
        } else {
            self.launch(&file);
        }
    }

    /// Follow an entry that stands for somewhere else, as everything in `network:///` and
    /// `computer:///` does: the entry is not a folder and listing it says as much, so what
    /// it points at is opened instead. A share that is not mounted answers nothing about
    /// itself yet, and going there is what mounts it. `name` is what the entry was listed
    /// as, for a target that has no name of its own.
    pub(crate) fn open_target(&self, target: gio::File, name: Option<String>) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let folder = match target
                    .query_info_future(
                        "standard::type",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    Ok(info) => info.file_type() == gio::FileType::Directory,
                    // Not mounted, or not there any more: go anyway and let the folder say so.
                    Err(_) => true,
                };
                if folder {
                    if let Some(name) = name {
                        view.imp().given_name.replace(Some((target.clone(), name)));
                    }
                    view.go_to(&target);
                } else if view.chooser_mode() {
                    view.emit_by_name::<()>("file-activated", &[&target]);
                } else {
                    view.launch(&target);
                }
            }
        ));
    }

    pub fn launch(&self, file: &gio::File) {
        let ctx = self.display().app_launch_context();
        let file = file.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                // An application that asks for a terminal has to be given one; GIO will not
                // go looking for a terminal emulator and refuses the launch instead.
                if let Some(app) = default_app(&file).await
                    && let Some(result) =
                        crate::terminal::launch_if_wanted(&app, std::slice::from_ref(&file))
                {
                    if let Err(e) = result {
                        view.show_error(&gettext("Could Not Open"), e.message());
                    }
                    return;
                }
                let uri = file.uri();
                if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await
                {
                    view.show_error(&gettext("Could Not Open"), e.message());
                }
            }
        ));
    }

    /// A folder on a share that is not mounted yet: mount it, then list it again. This is
    /// how a bookmark or an address typed into the bar reaches a server, without the
    /// connect dialog having to be opened first.
    fn mount_location(&self) {
        let imp = self.imp();
        if !crate::prefs::use_network() {
            return;
        }
        let Some(error) = imp.model.error() else {
            return;
        };
        if !error.matches(gio::IOErrorEnum::NotMounted) {
            return;
        }
        let Some(file) = self.location() else {
            return;
        };
        if !matches!(*imp.mounting.borrow(), Mounting::Idle) {
            return;
        }
        imp.mounting.replace(Mounting::Underway);
        self.update_stack();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let result = crate::network::mount(&file, &view, &gio::Cancellable::new()).await;
                // The answer is about the location asked for; the view may have moved on.
                if view.location().is_none_or(|now| !now.equal(&file)) {
                    return;
                }
                match result {
                    Ok(()) => {
                        // Mounted: the address is worth trying again if it is ever lost,
                        // and the location has a mount to be named after now.
                        view.imp().mounting.replace(Mounting::Idle);
                        view.notify_location();
                        view.reload();
                    }
                    Err(e) => {
                        let why = if e.matches(gio::IOErrorEnum::FailedHandled) {
                            gettext("The password dialog was dismissed.")
                        } else {
                            e.message().to_string()
                        };
                        view.imp().mounting.replace(Mounting::Failed(why));
                        view.update_stack();
                    }
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
        let empty = !imp.model.loading() && imp.model.n_items() == 0;
        let name = if let Some(msg) = imp.model.error_message() {
            match &*imp.mounting.borrow() {
                // Not mounted yet is not a failure while the server is being reached.
                Mounting::Underway => {
                    imp.error_page
                        .set_icon_name(Some("network-server-symbolic"));
                    imp.error_page
                        .set_title(&gettext("Connecting to %s…").replace("%s", &self.host()));
                    imp.error_page.set_description(None);
                }
                Mounting::Failed(why) => {
                    imp.error_page.set_icon_name(Some("dialog-error-symbolic"));
                    imp.error_page.set_title(&gettext("Could Not Connect"));
                    imp.error_page.set_description(Some(&format!(
                        "{why}\n{}",
                        gettext("Reloading the folder tries again.")
                    )));
                }
                Mounting::Idle => {
                    imp.error_page.set_icon_name(Some("dialog-error-symbolic"));
                    imp.error_page.set_title(&gettext("Could Not Open Folder"));
                    imp.error_page
                        .set_description(Some(&self.why_not_opened(&msg)));
                }
            }
            "error"
        } else if imp.view_mode.get() == ViewMode::Columns
            && !imp.model.searching()
            && (!empty || self.shows_chain())
        {
            // The strip keeps the path on screen even where the folder itself is empty,
            // but an empty folder with no path to draw would be a blank window. A search
            // reaches past the folder, which no column can draw either.
            "columns"
        } else if empty {
            let network = self
                .location()
                .is_some_and(|f| f.uri() == crate::network::NETWORK_URI);
            let (icon, title, why) = if imp.model.searching() {
                ("folder-symbolic", gettext("No Results Found"), None)
            } else if network {
                (
                    "network-workgroup-symbolic",
                    gettext("No Servers Found"),
                    Some(self.why_no_servers()),
                )
            } else {
                ("folder-symbolic", gettext("Folder is Empty"), None)
            };
            imp.empty_page.set_icon_name(Some(icon));
            imp.empty_page.set_title(&title);
            imp.empty_page.set_description(why.as_deref());
            "empty"
        } else {
            match imp.view_mode.get() {
                ViewMode::Grid => "grid",
                _ => "list",
            }
        };
        imp.stack.set_visible_child_name(name);
        // Only the view on screen holds the model. A list keeps two hundred rows bound
        // and a grid thirty rows of cells, drawn or not, so the views on the other pages
        // would otherwise bind every row the folder changes, for nobody to look at. The
        // pages that are not views leave the model where it is: a folder emptying for a
        // moment, as one does when a search starts, must not take it off the view and
        // hand it back.
        if matches!(name, "grid" | "list" | "columns") {
            let sel = imp.model.selection();
            imp.grid_view.set_model((name == "grid").then_some(&sel));
            imp.column_view.set_model((name == "list").then_some(&sel));
            imp.miller_list
                .set_model((name == "columns").then_some(&sel));
        }
    }

    /// What the tab and the window are called after: the location's name, or the name a
    /// server was listed under where its address is all there is to go by.
    pub fn location_title(&self) -> String {
        let Some(loc) = self.location() else {
            return String::new();
        };
        match self.given_name() {
            Some((_, name)) => name,
            None => file_utils::location_name(&loc),
        }
    }

    /// The name the location was listed under, while it is a place on another machine
    /// with no mount to be named after.
    pub fn given_name(&self) -> Option<(gio::File, String)> {
        let loc = self.location()?;
        let given = self.imp().given_name.borrow().clone()?;
        (given.0.equal(&loc) && !loc.is_native() && crate::places_sidebar::mount_of(&loc).is_none())
            .then_some(given)
    }

    /// The machine the location is on, for a page that names it.
    fn host(&self) -> String {
        let uri = self.location().map(|f| f.uri()).unwrap_or_default();
        glib::Uri::parse(&uri, glib::UriFlags::NONE)
            .ok()
            .and_then(|parsed| parsed.host())
            .map_or_else(|| uri.to_string(), |host| host.to_string())
    }

    /// What to say under "No Servers Found": where servers would come from, and the ones
    /// found and left out because nothing installed here can open them.
    fn why_no_servers(&self) -> String {
        let mut text = gettext(
            "Machines that announce themselves on the network appear here. Others are reached with “Connect to Server…”.",
        );
        let left_out = self.imp().model.unreachable_schemes();
        if !left_out.is_empty() {
            let mut schemes: Vec<String> = left_out.iter().map(|s| format!("{s}://")).collect();
            schemes.dedup();
            text.push_str("\n\n");
            text.push_str(
                &ngettext(
                    "One server was found but left out: no gvfs backend for %s addresses is installed.",
                    "Some servers were found but left out: no gvfs backend for %s addresses is installed.",
                    left_out.len() as u32,
                )
                .replace("%s", &schemes.join(", ")),
            );
        }
        text
    }

    /// What to say under "Could Not Open Folder". The listing's own message as a rule, but
    /// a location whose scheme has no backend answers that it is not supported, which
    /// tells the reader nothing about what is missing.
    fn why_not_opened(&self, message: &str) -> String {
        let Some(scheme) = self.location().and_then(|f| f.uri_scheme()) else {
            return message.to_string();
        };
        if crate::network::supports(&scheme) {
            return message.to_string();
        }
        gettext("No gvfs backend for %s addresses is installed.")
            .replace("%s", &format!("{scheme}://"))
    }

    /// A rubber band changes the selection with every motion event, and reading it out
    /// walks the folder; a folder being listed changes it once per batch of files. Once
    /// per turn of the main loop is as often as a status bar and a menu need telling.
    pub(crate) fn queue_selection_update(&self) {
        if self.imp().selection_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().selection_pending.set(false);
                view.update_floating_bar();
                view.update_action_state();
            }
        ));
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
            .map(file_utils::size_of)
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
            let stamp = info
                .modification_date_time()
                .map(|d| d.to_unix())
                .unwrap_or(0);
            let (fut, handle) = futures_util::future::abortable(glib::clone!(
                #[weak]
                label,
                async move {
                    let count = count_children_at(&dir, stamp).await;
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
    pub(crate) fn bind_icon(
        &self,
        image: &gtk::Image,
        emblem: &gtk::Image,
        info: &gio::FileInfo,
        at: u32,
    ) {
        unbind_icon(image);
        set_emblem(
            emblem,
            file_utils::is_locked(info, self.imp().can_write.get()),
        );
        image.set_from_gicon(&crate::file_utils::icon_of(info));
        if file_utils::is_hidden(info) {
            image.add_css_class("hidden-file");
        } else {
            image.remove_css_class("hidden-file");
        }
        let info = info.clone();
        let model = self.model();
        let (fut, handle) = futures_util::future::abortable(glib::clone!(
            #[weak]
            image,
            async move {
                on_screen(&image).await;
                folder_listed(&model).await;
                if let Some(texture) = crate::thumbnails::load(&info, at).await {
                    image.set_paintable(Some(&texture));
                    image.add_css_class("file-thumbnail");
                }
            }
        ));
        unsafe { image.set_data("thumb-abort", handle) };
        // Low priority, as the folder appearing matters more than the pictures in it.
        glib::MainContext::default().spawn_local_with_priority(glib::Priority::LOW, async move {
            let _ = fut.await;
        });
    }

    /// Select `files` and show the first, as soon as the view has them to select (used by
    /// FileManager1.ShowItems, pasting, "Open Item Location", going back or up and leaving
    /// a search). A folder still listing is waited for: a big folder is put in order at
    /// the end, and a selection made before would not survive it. A search is not, since
    /// its results only ever go on the end. A file written a moment ago reaches the model
    /// through the folder monitor, which lags behind the operation that made it, so what
    /// is missing once nothing loads is given a moment more; a change of folder drops the
    /// lot.
    pub fn select_files_when_loaded(&self, files: Vec<gio::File>) {
        use futures_util::StreamExt;
        if files.is_empty() {
            return;
        }
        let model = self.model();
        let sel = model.selection();
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<()>();
        let poke = tx.clone();
        let loading_id = model.connect_loading_notify(move |_| {
            let _ = poke.unbounded_send(());
        });
        let items_id = sel.connect_items_changed(move |_, _, _, _| {
            let _ = tx.unbounded_send(());
        });
        let location = self.location();
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            const GRACE: std::time::Duration = std::time::Duration::from_secs(2);
            let mut missing_since: Option<std::time::Instant> = None;
            loop {
                let Some(view) = view.upgrade() else { break };
                let moved = match (view.location(), &location) {
                    (Some(now), Some(then)) => !now.equal(then),
                    (now, then) => now.is_some() != then.is_some(),
                };
                if moved {
                    break;
                }
                let found = model.positions_of(&files);
                let waiting = model.loading() && !model.searching();
                let all = found.len() >= files.len();
                let given_up = !waiting
                    && !all
                    && missing_since
                        .get_or_insert_with(std::time::Instant::now)
                        .elapsed()
                        >= GRACE;
                if !waiting && (all || given_up) {
                    sel.unselect_all();
                    for &pos in &found {
                        sel.select_item(pos, false);
                    }
                    if let Some(&pos) = found.first() {
                        view.reveal_position(pos, gtk::ListScrollFlags::FOCUS);
                    }
                    break;
                }
                drop(view);
                let wait = match missing_since {
                    Some(since) if !waiting => GRACE.saturating_sub(since.elapsed()),
                    _ => std::time::Duration::from_secs(3600),
                };
                // Nothing left to tell of a change: the model has gone.
                if let Ok(None) = glib::future_with_timeout(wait, rx.next()).await {
                    break;
                }
            }
            model.disconnect(loading_id);
            sel.disconnect(items_id);
        });
    }

    /// Let the grid have as many columns as fit, and no more. It keeps thirty rows of
    /// cells bound for as many columns as it may ever have, so left with the twenty a
    /// wide window at the smallest zoom can hold, a window showing forty cells binds six
    /// hundred every time the folder changes.
    fn fit_grid_columns(&self) {
        let imp = self.imp();
        let Some(width) = imp.grid_view.hadjustment().map(|a| a.page_size() as i32) else {
            return;
        };
        if width <= 0 {
            return;
        }
        // A cell is taken to be the icon between its emblem margins, a little less than
        // it is with its padding, so the count errs towards a column too many rather than
        // one too few.
        let cell = self.icon_size() + 2 * EMBLEM_MARGIN;
        imp.grid_view
            .set_max_columns((width / cell).clamp(1, GRID_MAX_COLUMNS) as u32);
    }

    /// Scroll to `pos` in whichever view is on screen; the others have no model to scroll.
    pub(crate) fn reveal_position(&self, pos: u32, flags: gtk::ListScrollFlags) {
        let imp = self.imp();
        if imp.grid_view.model().is_some() {
            imp.grid_view.scroll_to(pos, flags, None);
        }
        if imp.miller_list.model().is_some() {
            imp.miller_list.scroll_to(pos, flags, None);
        }
        if imp.column_view.model().is_some() {
            imp.column_view.scroll_to(pos, None, flags, None);
        }
    }

    /// Select the item `delta` places along, for the preview's arrows. The focus stays
    /// where it is, because it is in the preview.
    pub(crate) fn step_selection(&self, delta: i32) {
        let model = self.model();
        let selected = model.selection().selection();
        if selected.size() == 0 || model.n_items() == 0 {
            return;
        }
        let last = model.n_items() as i32 - 1;
        let pos = (selected.nth(0) as i32 + delta).clamp(0, last) as u32;
        model.selection().select_item(pos, true);
        self.reveal_position(pos, gtk::ListScrollFlags::NONE);
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
    fn hover_folder(&self, x: f64, y: f64) {
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
    fn folder_at(&self, x: f64, y: f64) -> Option<gio::File> {
        let info = self
            .item_at(x, y)
            .and_then(|pos| self.model().info_at(pos))?;
        file_utils::is_dir(&info).then(|| file_utils::file_of(&info))
    }

    /// Mark the row a drag is over, unmarking the one it left.
    fn set_drop_row(&self, row: Option<gtk::Widget>) {
        let row = row.filter(|_| self.view_mode() != ViewMode::Grid);
        if let Some(old) = self.imp().drop_row.replace(row.clone()) {
            old.remove_css_class("spiral-drop-row");
        }
        if let Some(row) = row {
            row.add_css_class("spiral-drop-row");
        }
    }

    /// Stop waiting for a folder to spring open.
    fn end_hover(&self) {
        let imp = self.imp();
        self.set_drop_row(None);
        imp.hover_pos.set(None);
        if let Some(id) = imp.hover_timer.borrow_mut().take() {
            id.remove();
        }
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
                    bind_tags(cell, cell_name(cell).as_ref(), &info);
                }
            });
        }
    }

    pub fn grab_view_focus(&self) {
        let imp = self.imp();
        match imp.stack.visible_child_name().as_deref() {
            Some("grid") => imp.grid_view.grab_focus(),
            Some("columns") => imp.miller_list.grab_focus(),
            Some("list") => imp.column_view.grab_focus(),
            // The empty and the error page have no view to hand the keyboard to. Handing
            // it to one that is not on screen leaves it on a widget outside everything the
            // window looks at, and the keys bound to the window stop working until
            // something else is clicked.
            _ => false,
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
                .css_classes(["spiral-grid-name"])
                .build();
            name_tooltip(&label);
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
            view.bind_icon(&image, &emblem, &info, item.position());
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            item.set_accessible_label(&info.display_name());
            bind_tags(&bx, Some(&label), &info);
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
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            name_tooltip(&label);
            bx.append(&label);
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
            view.bind_icon(&image, &emblem, &info, item.position());
            set_cut(&bx, &info);
            label.set_text(&info.display_name());
            bind_tags(&bx, None, &info);
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
                if let Some(tip) = tip {
                    label.set_has_tooltip(true);
                    label.connect_query_tooltip(glib::clone!(
                        #[weak]
                        item,
                        #[upgrade_or]
                        false,
                        move |_, _, _, _, tooltip| {
                            let Some(info) =
                                item.item().and_then(|o| crate::folder_model::info_of(&o))
                            else {
                                return false;
                            };
                            tooltip.set_text(Some(&tip(&info)));
                            true
                        }
                    ));
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
        // Search results come from anywhere below the folder; say where. A chooser searches
        // the one folder, so every result is in it and the column would say the same thing
        // on every row.
        let searchable_below = !self.chooser_mode();
        let location_col = text_col(
            gettext("Location"),
            0.0,
            gtk::pango::EllipsizeMode::Middle,
            file_utils::location_of,
            Some(file_utils::location_of),
        );
        location_col.set_visible(false);
        location_col.set_expand(true);
        // The trash says where each item was and when it was trashed.
        let trashed_on_col = text_col(
            gettext("Trashed On"),
            0.0,
            gtk::pango::EllipsizeMode::End,
            file_utils::trashed_on_string,
            None,
        );
        let trashed_from_col = text_col(
            gettext("Original Location"),
            0.0,
            gtk::pango::EllipsizeMode::Middle,
            file_utils::trashed_from,
            Some(file_utils::trashed_from),
        );
        for (key, col, width) in [
            ("trashed-on", &trashed_on_col, 148),
            ("trashed-from", &trashed_from_col, 200),
        ] {
            col.set_fixed_width(width);
            col.set_resizable(true);
            cv.insert_column(1, col);
            // First, so the room they need is found before the others are given any.
            columns.insert(0, (key, col.clone()));
        }
        cv.insert_column(1, &location_col);
        self.imp().model.connect_searching_notify(glib::clone!(
            #[weak]
            location_col,
            move |m| location_col.set_visible(searchable_below && m.searching())
        ));
        let column = |key: &str| {
            columns
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, col)| col.clone())
                .unwrap()
        };
        let (size_col, type_col, mod_col) = (column("size"), column("type"), column("modified"));
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
        self.connect_location_notify(|view| view.queue_visible_columns());
        // Resizing the window redivides the width; the page size of the scroll is what
        // the list actually got.
        self.imp()
            .list_scroll
            .hadjustment()
            .connect_page_size_notify(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| view.queue_visible_columns()
            ));

        // Header clicks drive FolderModel sort props instead of the column view's own sorter.
        for (col, key) in [
            (&name_col, SortKey::Name),
            (&size_col, SortKey::Size),
            (&type_col, SortKey::Type),
            (&mod_col, SortKey::Modified),
            (&trashed_on_col, SortKey::Trashed),
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

    /// The width is learnt while the list is being given it, and hiding a column then leaves
    /// the layout half done; the next idle is soon enough and outside the allocation.
    fn queue_visible_columns(&self) {
        if self.imp().fit_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                view.imp().fit_pending.set(false);
                view.apply_visible_columns();
            }
        ));
    }

    /// Show the columns the `visible-columns` key names, as many of them as there is room
    /// for. The name column takes what the others leave, so in a narrow window they are
    /// dropped from the right until the name is readable again, rather than the name
    /// shrinking to an ellipsis or the list running off the edge.
    fn apply_visible_columns(&self) {
        let imp = self.imp();
        let on = imp.settings.strv("visible-columns");
        // The columns of the trash are there in the trash, whatever the key says.
        let in_trash = self
            .location()
            .is_some_and(|l| l.uri().starts_with("trash:"));
        let width = imp.list_scroll.width();
        // Before the first allocation there is no width to divide; the key decides alone.
        let mut room = if width > 0 {
            width - NAME_MIN_WIDTH
        } else {
            i32::MAX
        };
        for (key, col) in imp.columns.borrow().iter() {
            // The star is a button wide and worth its place at any size.
            let cost = if *key == "star" { 0 } else { col.fixed_width() };
            let wanted = match *key {
                "trashed-on" | "trashed-from" => in_trash,
                _ => on.iter().any(|k| k == key),
            };
            let show = wanted && cost <= room;
            if show {
                room -= cost;
            }
            col.set_visible(show);
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
            star_tooltip(&button);
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
        // No header title and no more room than the button: the star is an icon people
        // recognise, and "Visible Columns" is where it is named.
        let column = gtk::ColumnViewColumn::new(None, Some(factory));
        column.set_fixed_width(STAR_WIDTH);
        column
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
