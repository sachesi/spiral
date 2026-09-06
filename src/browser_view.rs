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
                error_page: Default::default(),
                empty_page: Default::default(),
                floating_bar: Default::default(),
                floating_spinner: Default::default(),
                floating_primary: Default::default(),
                floating_details: Default::default(),
                item_menu: Default::default(),
                background_menu: Default::default(),
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

            // Dropping on empty space copies/moves into the folder being viewed.
            if !obj.chooser_mode() {
                let target = gtk::DropTarget::new(
                    gtk::gdk::FileList::static_type(),
                    gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
                );
                target.connect_enter(|t, _, _| preferred_action(t));
                target.connect_motion(|t, _, _| preferred_action(t));
                target.connect_drop(glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    false,
                    move |t, value, _, _| match obj.location() {
                        Some(loc) => obj.drop_files(t, value, &loc),
                        None => false,
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
                        move |_, _| obj.model().reload()
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
            if mode == ViewMode::Grid {
                self.model.collapse_all();
            }
            self.obj().update_stack();
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
    if settings.enum_(key) == ViewMode::List as i32 {
        ViewMode::List
    } else {
        ViewMode::Grid
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

/// Lock shown on files the user cannot read or change; hidden until bound.
fn emblem_image() -> gtk::Image {
    gtk::Image::builder()
        .icon_name("changes-prevent-symbolic")
        .pixel_size(16)
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .visible(false)
        .css_classes(["spiral-emblem"])
        .build()
}

fn unbind_icon(image: &gtk::Image) {
    if let Some(handle) =
        unsafe { image.steal_data::<futures_util::future::AbortHandle>("thumb-abort") }
    {
        handle.abort();
    }
    image.remove_css_class("file-thumbnail");
}

/// Copy when the source only offers copy (Ctrl held), move for drags started in this process,
/// copy for drags from other applications.
pub fn preferred_action(target: &gtk::DropTarget) -> gtk::gdk::DragAction {
    let Some(drop) = target.current_drop() else {
        return gtk::gdk::DragAction::COPY;
    };
    let actions = drop.actions();
    if actions == gtk::gdk::DragAction::COPY {
        gtk::gdk::DragAction::COPY
    } else if actions.contains(gtk::gdk::DragAction::MOVE) && drop.drag().is_some() {
        // Same-process drag: default to move like Nautilus does for local files.
        gtk::gdk::DragAction::MOVE
    } else {
        gtk::gdk::DragAction::COPY
    }
}

/// Cells keep a weak link to their `ListItem`: its position is live, unlike a cached
/// number, when items are inserted above it.
fn remember_list_item(cell: &impl IsA<gtk::Widget>, item: &gtk::ListItem) {
    unsafe { cell.set_data("list-item", item.downgrade()) };
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
        self.set_view_mode(global_view_mode(&imp.settings, "view-mode"));
        let remember = crate::prefs::remember_view();
        let guess = crate::prefs::guess_view();
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

    /// Flip between grid and list: for this folder only when views are remembered per
    /// folder, otherwise as the new global default.
    pub fn toggle_view_mode(&self) {
        let next = match self.view_mode() {
            ViewMode::Grid => ViewMode::List,
            ViewMode::List => ViewMode::Grid,
        };
        self.set_view_mode(next);
        let nick = match next {
            ViewMode::Grid => "grid",
            ViewMode::List => "list",
        };
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
                    view.show_error(e.message());
                }
            }
        ));
    }

    pub(crate) fn show_error(&self, message: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Could Not Open"))
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
                ViewMode::List => "list",
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
    fn bind_icon(&self, image: &gtk::Image, emblem: &gtk::Image, info: &gio::FileInfo) {
        unbind_icon(image);
        emblem.set_visible(file_utils::is_locked(info, self.imp().can_write.get()));
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
                    .column_view
                    .scroll_to(pos, None, gtk::ListScrollFlags::FOCUS, None);
            }
        });
    }

    /// Each cell is a drag source for the selection and a drop target when it shows a folder.
    fn setup_cell_dnd(&self, cell: &gtk::Box) {
        if self.chooser_mode() {
            return;
        }
        let source = gtk::DragSource::builder()
            .actions(
                gtk::gdk::DragAction::COPY
                    | gtk::gdk::DragAction::MOVE
                    | gtk::gdk::DragAction::LINK,
            )
            .build();
        source.connect_prepare(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            cell,
            #[upgrade_or]
            None,
            move |_, _, _| {
                let pos = cell_position(&cell)?;
                let sel = view.model().selection();
                if !sel.is_selected(pos) {
                    sel.select_item(pos, true);
                }
                let files = view.model().selected_files();
                if files.is_empty() {
                    return None;
                }
                Some(gtk::gdk::ContentProvider::for_value(
                    &gtk::gdk::FileList::from_array(&files).to_value(),
                ))
            }
        ));
        source.connect_drag_begin(glib::clone!(
            #[weak]
            cell,
            move |src, _| {
                let paintable = gtk::WidgetPaintable::new(Some(&cell));
                src.set_icon(Some(&paintable), cell.width() / 2, cell.height() / 2);
            }
        ));
        cell.add_controller(source);

        let target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
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
            move |t, value, _, _| {
                let Some(folder) = view.cell_folder(&cell) else {
                    return false;
                };
                view.drop_files(t, value, &folder)
            }
        ));
        cell.add_controller(target);
    }

    /// The folder a cell currently shows, if it is one.
    fn cell_folder(&self, cell: &gtk::Box) -> Option<gio::File> {
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
        let action = preferred_action(target);
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

    pub fn grab_view_focus(&self) {
        let imp = self.imp();
        match imp.view_mode.get() {
            ViewMode::Grid => imp.grid_view.grab_focus(),
            ViewMode::List => imp.column_view.grab_focus(),
        };
    }

    fn setup_grid_factory(&self) {
        let factory = gtk::SignalListItemFactory::new();
        let view = self.clone();
        factory.connect_setup(move |_, item| {
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
            let overlay = gtk::Overlay::builder().child(&image).build();
            overlay.add_overlay(&emblem_image());
            let bx = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .valign(gtk::Align::Start)
                .css_classes(["spiral-view-cell"])
                .build();
            bx.append(&overlay);
            bx.append(&labels);
            item.set_child(Some(&bx));
            remember_list_item(&bx, item);
            view.setup_cell_dnd(&bx);
        });
        let view = self.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let bx = item.child().unwrap();
            let overlay = bx.first_child().and_downcast::<gtk::Overlay>().unwrap();
            let image = overlay.child().and_downcast::<gtk::Image>().unwrap();
            let emblem = overlay.last_child().and_downcast::<gtk::Image>().unwrap();
            let labels = bx.last_child().unwrap();
            let label = labels.first_child().and_downcast::<gtk::Label>().unwrap();
            let captions = labels.last_child().and_downcast::<gtk::Label>().unwrap();
            view.bind_icon(&image, &emblem, &info);
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
                .and_downcast::<gtk::Overlay>()
                .and_then(|o| o.child())
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
        let view = self.clone();
        name_factory.connect_setup(move |_, item| {
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
        let view = self.clone();
        name_factory.connect_bind(move |_, item| {
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

        let text_col = |title: String, xalign: f32, f: fn(&gio::FileInfo) -> String| {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let label = gtk::Label::builder()
                    .xalign(xalign)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .css_classes(["spiral-view-cell", "dim-label"])
                    .build();
                if xalign > 0.5 {
                    label.add_css_class("numeric");
                }
                item.set_child(Some(&label));
            });
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                    return;
                };
                item.child()
                    .and_downcast::<gtk::Label>()
                    .unwrap()
                    .set_text(&f(&info));
            });
            gtk::ColumnViewColumn::new(Some(&title), Some(factory))
        };
        let text = |key: &str| -> fn(&gio::FileInfo) -> String {
            match key {
                "size" => file_utils::size_string,
                "type" => file_utils::type_string,
                "modified" => file_utils::modified_string,
                "accessed" => file_utils::accessed_string,
                "created" => file_utils::created_string,
                "owner" => |i| file_utils::caption(i, "owner").unwrap_or_default(),
                "group" => |i| file_utils::caption(i, "group").unwrap_or_default(),
                _ => |i| file_utils::permissions_string(i).unwrap_or_default(),
            }
        };
        let mut columns: Vec<(&'static str, gtk::ColumnViewColumn)> = Vec::new();
        for (key, title) in file_utils::optional_columns() {
            let col = text_col(title, if key == "size" { 1.0 } else { 0.0 }, text(key));
            cv.append_column(&col);
            columns.push((key, col));
        }
        let star_col = self.star_column();
        cv.append_column(&star_col);
        columns.push(("star", star_col));
        // Search results come from anywhere below the folder; say where.
        let location_col = text_col(gettext("Location"), 0.0, file_utils::location_of);
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
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let button = gtk::Button::builder()
                .icon_name("non-starred-symbolic")
                .valign(gtk::Align::Center)
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
                    button.set_icon_name(if starred {
                        "starred-symbolic"
                    } else {
                        "non-starred-symbolic"
                    });
                }
            ));
            item.set_child(Some(&button));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(info) = item.item().and_then(|o| crate::folder_model::info_of(&o)) else {
                return;
            };
            let button = item.child().and_downcast::<gtk::Button>().unwrap();
            let starred = crate::starred::is_starred(&file_utils::file_of(&info));
            button.set_icon_name(if starred {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
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
