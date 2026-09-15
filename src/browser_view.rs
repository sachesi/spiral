//! One folder view (a tab): grid or list over a shared `FolderModel`, with history.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::subclass::prelude::*;

use crate::enums::{SortKey, ViewMode};
use crate::file_utils;
use crate::folder_model::FolderModel;
use crate::object_data::Key;
use crate::{adw, gdk, gio, glib, gtk};

mod cells;
mod columns;
mod dnd;
mod navigation;
mod opening;
mod selection;

pub(crate) use cells::*;
pub(crate) use dnd::*;
use navigation::*;
use opening::*;

static COUNT_ABORT: Key<futures_util::future::AbortHandle> = Key::new("count-abort");
static THUMB_ABORT: Key<futures_util::future::AbortHandle> = Key::new("thumb-abort");
static THUMB_MAP: Key<glib::SignalHandlerId> = Key::new("thumb-map");
static LIST_ITEM: Key<glib::WeakRef<gtk::ListItem>> = Key::new("list-item");
static SORT_KEY: Key<SortKey> = Key::new("sort-key");

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
        /// Whether anything was selected as of the last change to the selection or the
        /// items, since the selection says nothing when what was selected goes away.
        pub had_selection: Cell<bool>,
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
        /// Whether gvfs has answered for the current folder. Until it has, asking a file
        /// on another machine for its local path blocks for as long as mounting takes.
        pub reached: Cell<bool>,
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
        /// The generation of the cut set the cells were last dimmed for.
        pub cut_gen: Cell<u64>,
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
            klass.add_binding_action(Key::c, M::CONTROL_MASK, "view.copy-network-address");
            klass.add_binding_action(Key::x, M::CONTROL_MASK, "view.cut");
            klass.add_binding_action(Key::v, M::CONTROL_MASK, "view.paste");
            klass.add_binding_action(
                Key::c,
                M::CONTROL_MASK | M::SHIFT_MASK,
                "view.copy-to-other-pane",
            );
            klass.add_binding_action(
                Key::x,
                M::CONTROL_MASK | M::SHIFT_MASK,
                "view.move-to-other-pane",
            );
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
                had_selection: Default::default(),
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
                reached: Cell::new(true),
                nav_gen: Default::default(),
                location: Default::default(),
                history: Default::default(),
                mounting: Default::default(),
                given_name: Default::default(),
                history_pos: Default::default(),
                settings: gio::Settings::new(crate::config::APP_ID),
                clipboard_handler: Default::default(),
                cut_gen: Default::default(),
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
                // The width is told while the grid is being laid out, which drops a change
                // to its columns made then; they change once the layout is done.
                hadj.connect_page_size_notify(glib::clone!(
                    #[weak]
                    obj,
                    move |_| {
                        glib::idle_add_local_once(glib::clone!(
                            #[weak]
                            obj,
                            move || obj.fit_grid_columns()
                        ));
                    }
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
                move |sel, position, removed, added| {
                    obj.update_stack();
                    obj.queue_selection_update();
                    obj.queue_pick_first_result();
                    let selected = !sel.selection().is_empty();
                    if obj.imp().had_selection.replace(selected)
                        && !selected
                        && removed > 0
                        && added == 0
                        && sel.n_items() > 0
                    {
                        obj.queue_select_neighbor(position);
                    }
                    if added > 0 {
                        obj.queue_select_replaced();
                    }
                }
            ));
            self.model
                .selection()
                .connect_selection_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |sel, _, _| {
                        let imp = obj.imp();
                        if !imp.picking.get() {
                            imp.first_picked.set(false);
                        }
                        imp.had_selection.set(!sel.selection().is_empty());
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

fn global_view_mode(settings: &gio::Settings, key: &str) -> ViewMode {
    match settings.enum_(key) {
        v if v == ViewMode::List as i32 => ViewMode::List,
        v if v == ViewMode::Columns as i32 => ViewMode::Columns,
        _ => ViewMode::Grid,
    }
}

/// Store a per-folder metadata attribute without waiting for the metadata daemon.
fn remember(dir: gio::File, attribute: &'static str, value: String) {
    if crate::folder_model::is_list_location(&dir) {
        crate::prefs::set_list_view(&dir.uri(), attribute, &value);
        return;
    }
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

/// Whether `dir` keeps a view and an order of its own. A folder keeps them in gvfs
/// metadata, which may not be there; favorites and tags are lists, with no folder to keep
/// them on, and keep them in the settings.
pub(crate) fn keeps_own_view(dir: &gio::File) -> bool {
    if crate::folder_model::is_list_location(dir) {
        crate::prefs::settings().boolean("remember-view")
    } else {
        crate::prefs::remember_view()
    }
}

/// The `metadata::` `attributes` `dir` keeps, wherever it keeps them.
pub(crate) async fn remembered(dir: &gio::File, attributes: &str) -> Option<gio::FileInfo> {
    if !crate::folder_model::is_list_location(dir) {
        return dir
            .query_info_future(
                attributes,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
            .ok();
    }
    let info = gio::FileInfo::new();
    for (attribute, value) in crate::prefs::list_view(&dir.uri()) {
        if attributes.split(',').any(|a| a == attribute) {
            info.set_attribute_string(&attribute, &value);
        }
    }
    Some(info)
}

/// The order a folder remembers, from an info asked for `metadata::spiral-sort`.
pub(crate) fn remembered_sort(info: &gio::FileInfo) -> Option<(SortKey, bool)> {
    let value = info.attribute_string("metadata::spiral-sort")?;
    let (key, dir) = value.split_once('-')?;
    Some((SortKey::from_nick(key)?, dir == "desc"))
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
    pub(crate) fn sort_keys(&self) -> (&'static str, &'static str) {
        if self.chooser_mode() {
            ("chooser-sort-key", "chooser-sort-reversed")
        } else {
            ("sort-key", "sort-reversed")
        }
    }

    /// The order of a folder that remembers none of its own.
    pub(crate) fn global_sort(&self) -> (SortKey, bool) {
        let imp = self.imp();
        let (key_name, reversed_name) = self.sort_keys();
        let key = SortKey::from_nick(&imp.settings.string(key_name)).unwrap_or_default();
        (key, imp.settings.boolean(reversed_name))
    }

    fn apply_global_sort(&self) {
        let (key, reversed) = self.global_sort();
        let imp = self.imp();
        imp.model.set_sort_key(key);
        imp.model.set_sort_reversed(reversed);
    }

    /// Sort the current folder: remembered for this folder when views are remembered per
    /// folder, otherwise as the new global order. A chooser keeps its own order instead.
    pub fn set_sort(&self, key: SortKey, reversed: bool) {
        let imp = self.imp();
        imp.model.set_sort_key(key);
        imp.model.set_sort_reversed(reversed);
        match self.location() {
            // Only the trash has the date to sort by, and every other folder would lose its
            // order to it: it stays with the trash, for as long as the trash is shown, where
            // it is not remembered for the trash itself.
            Some(_)
                if key == SortKey::Trashed
                    && (self.chooser_mode() || !crate::prefs::remember_view()) =>
            {
                imp.folder_sort.set(Some((key, reversed)));
            }
            _ if self.chooser_mode() => {
                let (key_name, reversed_name) = self.sort_keys();
                let _ = imp.settings.set_string(key_name, key.nick());
                let _ = imp.settings.set_boolean(reversed_name, reversed);
            }
            Some(dir) if keeps_own_view(&dir) => {
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
        // next is how they are read, so a move that has nothing else to say leaves them
        // alone. Only a folder remembering a view of its own takes the pane out of them.
        let keep = self.view_mode() == ViewMode::Columns;
        if !keep {
            self.set_view_mode(global_view_mode(&imp.settings, "view-mode"));
        }
        let remember = keeps_own_view(file);
        // Nor are the columns ever traded for a grid by a guess.
        let guess = crate::prefs::guess_view() && self.view_mode() != ViewMode::Columns;
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
                    && let Some(info) =
                        remembered(&file, "metadata::spiral-view,metadata::spiral-sort").await
                    && view.imp().nav_gen.get() == generation
                {
                    if let Some((key, dir)) = remembered_sort(&info) {
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
                if view.imp().nav_gen.get() == generation
                    && view.view_mode() != ViewMode::Columns
                    && mostly_media(&model)
                {
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
            Some(dir) if keeps_own_view(&dir) => {
                self.imp().folder_view.set(Some(next));
                remember(dir, "metadata::spiral-view", nick.to_string());
            }
            _ => {
                let _ = self.imp().settings.set_string("view-mode", nick);
            }
        }
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
        // A page that held the keyboard hands it on as it goes: left on a page off screen,
        // it would stay there and the keys bound to the view with it.
        let hand_on =
            imp.stack.visible_child_name().is_some_and(|n| n != name) && self.view_has_focus();
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
        if hand_on {
            self.grab_view_focus();
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

    pub fn grab_view_focus(&self) {
        let imp = self.imp();
        match imp.stack.visible_child_name().as_deref() {
            Some("grid") => imp.grid_view.grab_focus(),
            Some("columns") => imp.miller_list.grab_focus(),
            Some("list") => imp.column_view.grab_focus(),
            // The empty and the error page have no view to hand the keyboard to, so the box
            // around each takes it. Handing it to a view that is not on screen leaves it on
            // a widget outside everything the window looks at, and the keys bound to the
            // window stop working until something else is clicked.
            Some("empty") => imp.empty_page.parent().is_some_and(|p| p.grab_focus()),
            Some("error") => imp.error_page.parent().is_some_and(|p| p.grab_focus()),
            _ => false,
        };
    }
}
