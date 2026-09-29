//! The desktop's icons: a tile for each file of the folder and for each place shown beside
//! the files, on the cells of the desktop as its arrangement has them. The tiles of files
//! are made like the grid's, and carry the position of their item, so the view's menus,
//! drag and drop and actions find them as they find the grid's.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::desktop::drop_ins;
use crate::desktop::placement::{self, Arrangement, Grid, Point};
use crate::desktop::positions::{Place, Positions};
use crate::desktop::specials::{self, Special};

use super::*;

/// Where the tile of a file is among the items of the model, kept up to date as items come
/// and go, as a list item's position is.
pub(crate) static SLOT: Key<Rc<Cell<u32>>> = Key::new("desktop-slot");
/// The key of the place a tile stands for, on the tiles that are not files.
static SPECIAL: Key<String> = Key::new("desktop-special");

/// Kept clear round the edge of the desktop.
const MARGIN: f64 = 4.0;
/// A tile is the icon and this much more: across, the margins either side of it that the
/// lock emblem sits in and the name reaches into; down, the room round the icon and the
/// name. The lines of the name and the captions come on top, and the spacing set goes
/// between tiles.
const TILE_EXTRA: (i32, i32) = (2 * cells::EMBLEM_MARGIN, 20);
/// Lines a name takes at most; a longer one is shortened in the middle.
pub(super) const NAME_LINES: i32 = 2;
/// How long a tile takes to glide to a new place.
const GLIDE_MS: u32 = 250;
/// The caption label shows no more lines than this, however many captions there are.
const CAPTION_LINES: i32 = 3;
/// How much of the extra width the name takes.
pub(super) const NAME_EXTRA: i32 = 16;
/// Letters typed within this long of each other make one name to look for.
const TYPE_AHEAD_PAUSE: Duration = Duration::from_millis(1000);
/// How long a place dropped files are put at waits for them to arrive.
const EXPECTED_FOR: Duration = Duration::from_secs(30);

pub(crate) const ZOOM_SIZES: [i32; 5] = [32, 48, 64, 96, 128];

struct Tile {
    frame: gtk::Box,
    cell: gtk::Box,
    slot: Rc<Cell<u32>>,
    place: Place,
}

struct SpecialTile {
    frame: gtk::Box,
    cell: gtk::Box,
    special: Special,
}

/// Where the band being drawn starts and ends, and the items and places selected before it.
type Band = (Point, Point, gtk::Bitset, HashSet<String>);

/// A drag of the desktop's icons that the system carries, so the files among them can go to
/// other windows. The tiles stay where they are, and the drag shows the icons the whole
/// way, over the windows too: in their layout over the desktop; away from it gathered under
/// the pointer, led by the file nearest it as the drag began, while the places, which go
/// nowhere else, leave it.
struct Carry {
    places: Vec<Place>,
    flock: Option<flock::Flock>,
    origin: (f64, f64),
    away: bool,
}

/// What is under a point of the desktop.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Item(u32),
    Special(String),
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct IconCanvas {
        pub(super) view: glib::WeakRef<BrowserView>,
        pub(super) model: RefCell<Option<gtk::MultiSelection>>,
        pub(super) tiles: RefCell<Vec<Tile>>,
        pub(super) specials: RefCell<Vec<SpecialTile>>,
        pub(super) band: RefCell<Option<gtk::Box>>,
        pub(super) banding: RefCell<Option<Band>>,
        /// Places kept for the desktop's size, and the places the tiles are at.
        pub(super) kept: RefCell<HashMap<Place, Point>>,
        pub(super) placed: RefCell<HashMap<Place, Point>>,
        pub(super) positions: RefCell<Option<Positions>>,
        /// Where each tile was drawn last, and the place it was to rest at when not lifted.
        pub(super) shown: RefCell<HashMap<Place, Point>>,
        pub(super) resting: RefCell<HashMap<Place, Point>>,
        /// Where the tiles set out from as they glide to their places.
        pub(super) glide_from: RefCell<HashMap<Place, Point>>,
        pub(super) glide: std::cell::OnceCell<adw::TimedAnimation>,
        pub(super) area: Cell<(i32, i32)>,
        pub(super) save_queued: Cell<bool>,
        /// The places selected, on their own or along with files.
        pub(super) special_selected: RefCell<HashSet<String>>,
        /// Where the keyboard is: the tile that moves with the arrows.
        pub(super) cursor: RefCell<Option<Hit>>,
        /// A press on a tile already selected, which narrows the selection to it on
        /// release unless a drag starts.
        pub(super) narrow: RefCell<Option<Hit>>,
        pub(super) typed: RefCell<(String, Option<Instant>)>,
        /// Where a drag of the desktop's own icons starts, until the system takes it up.
        pub(super) drag_origin: Cell<Option<(f64, f64)>>,
        /// What is being dragged across the desktop by a place, and how far it has come.
        /// Places are no files to hand over in a drag, so they move within the desktop only,
        /// and the files selected with them move along.
        pub(super) lifted: RefCell<Option<(Vec<Place>, Point)>>,
        pub(super) carry: RefCell<Option<Carry>>,
        /// Whether the system took the press the band or a place's move started from.
        pub(super) band_cancelled: Cell<bool>,
        /// Names of files dropped on the desktop from elsewhere, and where they were dropped.
        pub(super) expected: RefCell<Vec<(String, Point, Instant)>>,
        pub(super) trash_monitor: RefCell<Option<gio::FileMonitor>>,
        pub(super) volumes: RefCell<Option<gio::VolumeMonitor>>,
        pub(super) actions: RefCell<Option<gio::SimpleActionGroup>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IconCanvas {
        const NAME: &'static str = "SpiralIconCanvas";
        type Type = super::IconCanvas;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("spiral-desktop");
            klass.set_accessible_role(gtk::AccessibleRole::Grid);
        }
    }

    impl ObjectImpl for IconCanvas {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for IconCanvas {
        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            self.obj().lay_out(width, height);
        }
    }
}

glib::wrapper! {
    pub struct IconCanvas(ObjectSubclass<imp::IconCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl IconCanvas {
    pub(crate) fn new(view: &BrowserView) -> Self {
        let canvas: Self = glib::Object::builder()
            .property("focusable", true)
            .property("hexpand", true)
            .property("vexpand", true)
            .build();
        let imp = canvas.imp();
        imp.view.set(Some(view));
        imp.positions
            .replace(Some(Positions::open(&Positions::default_path())));
        let band = gtk::Box::builder()
            .css_classes(["spiral-desktop-band"])
            .can_target(false)
            .visible(false)
            .build();
        band.set_parent(&canvas);
        imp.band.replace(Some(band));
        canvas.setup_actions(view);
        canvas.setup_input();
        canvas.follow_settings(view);
        canvas.rebuild_specials();
        canvas.watch_places();
        canvas
    }

    fn view(&self) -> BrowserView {
        self.imp()
            .view
            .upgrade()
            .expect("the view outlives its desktop")
    }

    fn settings(&self) -> gio::Settings {
        self.view().imp().settings.clone()
    }

    /// Show the items of `model`, the view's selection.
    pub(crate) fn set_model(&self, model: &gtk::MultiSelection) {
        let imp = self.imp();
        if imp.model.borrow().as_ref() == Some(model) {
            return;
        }
        imp.model.replace(Some(model.clone()));
        model.connect_items_changed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |model, position, removed, added| {
                canvas.items_changed(model, position, removed, added)
            }
        ));
        model.connect_selection_changed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, position, n| canvas.show_selection(position, n)
        ));
        self.items_changed(model, 0, 0, model.n_items());
    }

    fn arrangement(&self) -> Arrangement {
        Arrangement::from_nick(&self.settings().string("desktop-arrangement")).unwrap_or_default()
    }

    fn tile_size(&self) -> (i32, i32) {
        let view = self.view();
        let icon = view.icon_size();
        let captions = (view.caption_kinds().len() as i32).min(CAPTION_LINES);
        // Captions are drawn smaller than the name, so a line of the name holds one.
        let metrics = self.pango_context().metrics(None, None);
        let line = gtk::pango::units_to_double(metrics.ascent() + metrics.descent()).ceil() as i32;
        (
            icon + TILE_EXTRA.0,
            icon + TILE_EXTRA.1 + (NAME_LINES + captions) * line,
        )
    }

    fn spacing(&self) -> i32 {
        self.settings().int("desktop-icon-spacing")
    }

    fn aligned_right(&self) -> bool {
        self.settings().string("desktop-alignment") == "right"
    }

    fn grid(&self) -> Grid {
        let (w, h) = self.imp().area.get();
        let (tile_w, tile_h) = self.tile_size();
        let spacing = self.spacing();
        Grid::new(
            Point::new(f64::from(w), f64::from(h)),
            Point::new(f64::from(tile_w + spacing), f64::from(tile_h + spacing)),
            MARGIN,
        )
        .aligned_right(self.aligned_right())
    }

    fn items_changed(&self, model: &gtk::MultiSelection, position: u32, removed: u32, added: u32) {
        let view = self.view();
        let imp = self.imp();
        let new: Vec<Tile> = (position..position + added)
            .filter_map(|at| {
                let info = model
                    .item(at)
                    .and_then(|o| crate::folder_model::info_of(&o))?;
                Some(self.file_tile(&view, &info, at))
            })
            .collect();
        let old: Vec<Tile> = {
            let mut tiles = imp.tiles.borrow_mut();
            let end = (position + removed).min(tiles.len() as u32) as usize;
            let start = (position as usize).min(end);
            tiles.splice(start..end, new).collect()
        };
        // A file renamed in place keeps its place.
        if let ([gone], Some(came)) = (old.as_slice(), self.tile_place(position))
            && added == 1
            && gone.place != came
        {
            let mut placed = imp.placed.borrow_mut();
            if let Some(at) = placed.remove(&gone.place) {
                placed.insert(came.clone(), at);
                imp.kept.borrow_mut().insert(came, at);
            }
        }
        for tile in old {
            unbind_grid_cell(tile.cell.upcast_ref());
            tile.frame.unparent();
        }
        for (at, tile) in imp
            .tiles
            .borrow()
            .iter()
            .enumerate()
            .skip(position as usize)
        {
            tile.slot.set(at as u32);
        }
        self.show_selection(position, added);
        self.show_cursor();
        self.queue_allocate();
    }

    fn tile_place(&self, at: u32) -> Option<Place> {
        self.imp()
            .tiles
            .borrow()
            .get(at as usize)
            .map(|t| t.place.clone())
    }

    fn file_tile(&self, view: &BrowserView, info: &gio::FileInfo, at: u32) -> Tile {
        let cell = view.grid_cell(true);
        let slot = Rc::new(Cell::new(at));
        SLOT.set(&cell, Rc::clone(&slot));
        view.bind_grid_cell(cell.upcast_ref(), info, at);
        view.setup_cell_dnd(&cell);
        let frame = self.frame(&cell);
        frame.update_property(&[gtk::accessible::Property::Label(&info.display_name())]);
        let name = info.name().to_string_lossy().into_owned();
        Tile {
            frame,
            cell,
            slot,
            place: Place::File(name),
        }
    }

    fn frame(&self, cell: &gtk::Box) -> gtk::Box {
        let frame = gtk::Box::builder()
            .css_classes(["spiral-desktop-tile"])
            .accessible_role(gtk::AccessibleRole::GridCell)
            .build();
        frame.append(cell);
        frame.insert_before(self, self.imp().band.borrow().as_ref());
        frame
    }

    fn show_selection(&self, position: u32, n: u32) {
        let imp = self.imp();
        let Some(model) = imp.model.borrow().clone() else {
            return;
        };
        let tiles = imp.tiles.borrow();
        for at in position..position + n {
            if let Some(tile) = tiles.get(at as usize) {
                mark(&tile.frame, "selected", model.is_selected(at));
            }
        }
    }

    fn rebuild_specials(&self) {
        let view = self.view();
        let imp = self.imp();
        for old in imp.specials.borrow_mut().drain(..) {
            old.frame.unparent();
        }
        let tiles: Vec<SpecialTile> = specials::list(&self.settings())
            .into_iter()
            .map(|special| {
                let cell = view.grid_cell(true);
                let icon_row = cell.first_child().unwrap();
                let image = icon_row.first_child().and_downcast::<gtk::Image>().unwrap();
                image.set_from_gicon(&special.icon);
                let label = cell
                    .last_child()
                    .and_then(|l| l.first_child())
                    .and_downcast::<gtk::Label>()
                    .unwrap();
                label.set_text(&special.name);
                self.bind_special_captions(&cell, &special);
                let frame = self.frame(&cell);
                frame.update_property(&[gtk::accessible::Property::Label(&special.name)]);
                SPECIAL.set(&frame, special.key.clone());
                self.special_drop(&frame, &special);
                SpecialTile {
                    frame,
                    cell,
                    special,
                }
            })
            .collect();
        let selected = imp.special_selected.borrow().clone();
        for tile in &tiles {
            mark(
                &tile.frame,
                "selected",
                selected.contains(&tile.special.key),
            );
        }
        imp.specials.replace(tiles);
        self.show_cursor();
        self.update_trash();
        self.queue_allocate();
    }

    fn watch_places(&self) {
        let imp = self.imp();
        let monitor = gio::VolumeMonitor::get();
        let rebuild = glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_: &gio::VolumeMonitor, _: &gio::Mount| canvas.rebuild_specials()
        );
        monitor.connect_mount_added(rebuild.clone());
        monitor.connect_mount_removed(rebuild.clone());
        monitor.connect_mount_changed(rebuild);
        imp.volumes.replace(Some(monitor));
        if let Ok(trash) = gio::File::for_uri(specials::TRASH_URI)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            trash.connect_changed(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, _, _, _| canvas.update_trash()
            ));
            imp.trash_monitor.replace(Some(trash));
        }
    }

    fn update_trash(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            async move {
                let full = specials::trash_full().await;
                for tile in canvas.imp().specials.borrow().iter() {
                    if tile.special.is_trash()
                        && let Some(image) = tile
                            .cell
                            .first_child()
                            .and_then(|row| row.first_child())
                            .and_downcast::<gtk::Image>()
                    {
                        image.set_from_gicon(&specials::trash_icon(full));
                    }
                }
                for tile in canvas.imp().specials.borrow().iter() {
                    if tile.special.is_trash() {
                        canvas.bind_special_captions(&tile.cell, &tile.special);
                    }
                }
            }
        ));
    }

    /// Files dropped on the trash go to it; on Home or a drive, into it, as the view drops
    /// files into a folder.
    fn special_drop(&self, frame: &gtk::Box, special: &Special) {
        let target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE | gtk::gdk::DragAction::LINK,
        );
        let trash = special.is_trash();
        target.connect_enter(move |t, _, _| {
            if trash {
                gtk::gdk::DragAction::MOVE
            } else {
                preferred_action(t)
            }
        });
        let folder = special.file.clone();
        target.connect_drop(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            #[upgrade_or]
            false,
            move |t, value, x, y| {
                let view = canvas.view();
                if !trash {
                    return view.drop_files(t, value, &folder, x, y);
                }
                let Ok(list) = value.get::<gtk::gdk::FileList>() else {
                    return false;
                };
                let files = list.files();
                if files.is_empty() {
                    return false;
                }
                view.submit_kind(crate::ops::JobKind::Trash { files });
                true
            }
        ));
        frame.add_controller(target);
    }

    /// Selects the place `key` alone among the places, or none.
    fn select_special(&self, key: Option<&str>) {
        self.set_specials(key.into_iter().map(str::to_owned).collect());
    }

    fn set_specials(&self, keys: HashSet<String>) {
        let imp = self.imp();
        if *imp.special_selected.borrow() == keys {
            return;
        }
        for tile in imp.specials.borrow().iter() {
            mark(&tile.frame, "selected", keys.contains(&tile.special.key));
        }
        imp.special_selected.replace(keys);
    }

    /// Where the keyboard is, ringed while it is in use.
    fn set_cursor(&self, hit: Option<Hit>) {
        self.imp().cursor.replace(hit);
        self.show_cursor();
    }

    fn show_cursor(&self) {
        let imp = self.imp();
        let cursor = imp.cursor.borrow();
        for (at, tile) in imp.tiles.borrow().iter().enumerate() {
            mark(&tile.frame, "cursor", *cursor == Some(Hit::Item(at as u32)));
        }
        for tile in imp.specials.borrow().iter() {
            let on = matches!(&*cursor, Some(Hit::Special(key)) if *key == tile.special.key);
            mark(&tile.frame, "cursor", on);
        }
    }

    fn is_selected(&self, hit: &Hit) -> bool {
        match hit {
            Hit::Item(at) => self.view().model().selection().is_selected(*at),
            Hit::Special(key) => self.imp().special_selected.borrow().contains(key),
        }
    }

    /// Makes `hit` all that is selected, files and places alike.
    fn select_only(&self, hit: &Hit) {
        let model = self.view().model().selection();
        match hit {
            Hit::Item(at) => {
                model.select_item(*at, true);
                self.select_special(None);
            }
            Hit::Special(key) => {
                model.unselect_all();
                self.select_special(Some(key));
            }
        }
    }

    /// Adds `hit` to what is selected, or with `toggle` takes it out when it is in.
    fn select_also(&self, hit: &Hit, toggle: bool) {
        let selected = self.is_selected(hit);
        match hit {
            Hit::Item(at) => {
                let model = self.view().model().selection();
                if selected && toggle {
                    model.unselect_item(*at);
                } else {
                    model.select_item(*at, false);
                }
            }
            Hit::Special(key) => {
                let mut keys = self.imp().special_selected.borrow().clone();
                if selected && toggle {
                    keys.remove(key);
                } else {
                    keys.insert(key.clone());
                }
                self.set_specials(keys);
            }
        }
    }

    fn select_none(&self) {
        self.view().model().selection().unselect_all();
        self.select_special(None);
    }

    /// Selects every place with the files Select All selects.
    pub(crate) fn select_all_places(&self) {
        let keys = self
            .imp()
            .specials
            .borrow()
            .iter()
            .map(|t| t.special.key.clone())
            .collect();
        self.set_specials(keys);
    }

    /// Selects the places that are not, as Invert Selection does the files.
    pub(crate) fn invert_places(&self) {
        let selected = self.imp().special_selected.borrow().clone();
        let keys = self
            .imp()
            .specials
            .borrow()
            .iter()
            .map(|t| t.special.key.clone())
            .filter(|key| !selected.contains(key))
            .collect();
        self.set_specials(keys);
    }

    /// Opens the places selected, as Open does the files selected with them.
    pub(crate) fn open_selected_places(&self) {
        let keys: Vec<String> = self
            .imp()
            .special_selected
            .borrow()
            .iter()
            .cloned()
            .collect();
        for key in keys {
            self.open_special(&key);
        }
    }

    fn open_hit(&self, hit: &Hit) {
        match hit {
            Hit::Item(at) => self.view().activate_position(*at),
            Hit::Special(key) => self.open_special(key),
        }
    }

    /// The menu of the places selected without files: a place's own, or Open for several.
    fn places_menu(&self) -> Option<gio::Menu> {
        let selected = self.imp().special_selected.borrow().clone();
        if selected.len() == 1 {
            let key = selected.iter().next()?;
            return Some(self.special_menu(&self.special(key)?));
        }
        let menu = gio::Menu::new();
        menu.append(Some(&gettext("_Open")), Some("desktop.open-places"));
        Some(menu)
    }

    /// The places of what is selected, files and places both.
    fn selected_places(&self) -> Vec<Place> {
        let imp = self.imp();
        let model = self.view().model().selection();
        imp.tiles
            .borrow()
            .iter()
            .filter(|t| model.is_selected(t.slot.get()))
            .map(|t| t.place.clone())
            .chain(
                imp.special_selected
                    .borrow()
                    .iter()
                    .map(|key| Place::Special(key.clone())),
            )
            .collect()
    }

    fn special(&self, key: &str) -> Option<Special> {
        self.imp()
            .specials
            .borrow()
            .iter()
            .find(|t| t.special.key == key)
            .map(|t| t.special.clone())
    }

    fn open_special(&self, key: &str) {
        if let Some(special) = self.special(key) {
            self.view().launch(&special.file);
        }
    }

    fn leave_special(&self, key: &str, eject: bool) {
        let Some(mount) = self.special(key).and_then(|s| s.mount().cloned()) else {
            return;
        };
        let operation = gtk::MountOperation::new(self.root().and_downcast_ref::<gtk::Window>());
        let view = self.view();
        let done = glib::clone!(
            #[weak]
            view,
            move |result: Result<(), glib::Error>| {
                if let Err(e) = result
                    && !e.matches(gio::IOErrorEnum::Cancelled)
                    && !e.matches(gio::IOErrorEnum::FailedHandled)
                {
                    view.show_error(&gettext("Could Not Remove the Drive"), e.message());
                }
            }
        );
        if eject {
            mount.eject_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&operation),
                gio::Cancellable::NONE,
                done,
            );
        } else {
            mount.unmount_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&operation),
                gio::Cancellable::NONE,
                done,
            );
        }
    }

    fn special_menu(&self, special: &Special) -> gio::Menu {
        let menu = gio::Menu::new();
        let key = special.key.to_variant();
        let item = |label: String, action: &str| {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some(action), Some(&key));
            item
        };
        let open = gio::Menu::new();
        open.append_item(&item(gettext("_Open"), "desktop.open-special"));
        menu.append_section(None, &open);
        if special.is_trash() {
            let section = gio::Menu::new();
            section.append(Some(&gettext("_Empty Trash")), Some("desktop.empty-trash"));
            menu.append_section(None, &section);
        }
        if let Some(mount) = special.mount() {
            let section = gio::Menu::new();
            if mount.can_eject() {
                section.append_item(&item(gettext("_Eject"), "desktop.eject-special"));
            } else {
                section.append_item(&item(gettext("_Unmount"), "desktop.unmount-special"));
            }
            menu.append_section(None, &section);
        }
        menu
    }

    /// Sets every tile off from where it is drawn towards its place: those whose place
    /// changed, those let go of, and those still on their way to one.
    fn glide(&self) {
        let imp = self.imp();
        let still = gtk::Settings::for_display(&self.display()).gtk_interface_reduced_motion()
            == gtk::ReducedMotion::Reduce;
        if still {
            return;
        }
        let glide = imp.glide.get_or_init(|| {
            let target = adw::CallbackAnimationTarget::new(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_| canvas.queue_allocate()
            ));
            adw::TimedAnimation::builder()
                .widget(self)
                .value_from(0.0)
                .value_to(1.0)
                .duration(GLIDE_MS)
                .easing(adw::Easing::Ease)
                .target(&target)
                .build()
        });
        imp.glide_from.replace(imp.shown.borrow().clone());
        glide.reset();
        glide.play();
    }

    fn lay_out(&self, width: i32, height: i32) {
        let imp = self.imp();
        if imp.area.replace((width, height)) != (width, height) {
            let size = Point::new(f64::from(width), f64::from(height));
            let kept = imp
                .positions
                .borrow()
                .as_ref()
                .map(|p| p.kept(size))
                .unwrap_or_default();
            imp.kept.replace(self.seen_from_start_side(&kept));
            imp.placed.borrow_mut().clear();
        }
        let grid = self.grid();
        let places = self.places();
        let mut known = imp.kept.borrow().clone();
        known.extend(imp.placed.borrow().iter().map(|(p, at)| (p.clone(), *at)));
        {
            let mut expected = imp.expected.borrow_mut();
            expected.retain(|(_, _, when)| when.elapsed() < EXPECTED_FOR);
            expected.retain(|(name, at, _)| {
                let place = Place::File(name.clone());
                if places.contains(&place) {
                    known.insert(place, *at);
                    false
                } else {
                    true
                }
            });
        }
        let placed = placement::arrange(&grid, self.arrangement(), &places, &known);
        if *imp.placed.borrow() != placed {
            imp.placed.replace(placed);
            self.queue_save();
        }
        let (cw, ch) = self.tile_size();
        let cell = grid.cell_size();
        let inset = Point::new(
            (cell.x - f64::from(cw)) / 2.0,
            (cell.y - f64::from(ch)) / 2.0,
        );
        let lifted = imp.lifted.borrow().clone();
        let is_lifted = |place: &Place| lifted.as_ref().is_some_and(|(p, _)| p.contains(place));
        let placed = imp.placed.borrow();
        let settles = {
            let (resting, shown) = (imp.resting.borrow(), imp.shown.borrow());
            placed.iter().any(|(place, at)| {
                !is_lifted(place)
                    && match resting.get(place) {
                        Some(was) => was != at,
                        None => shown.contains_key(place),
                    }
            })
        };
        if settles {
            self.glide();
        }
        let glide = imp
            .glide
            .get()
            .filter(|glide| glide.state() == adw::AnimationState::Playing);
        let progress = glide.map(|glide| glide.value());
        let from = imp.glide_from.borrow();
        let mut shown = HashMap::with_capacity(placed.len());
        let frames = imp
            .specials
            .borrow()
            .iter()
            .map(|t| (Place::Special(t.special.key.clone()), t.frame.clone()))
            .chain(
                imp.tiles
                    .borrow()
                    .iter()
                    .map(|t| (t.place.clone(), t.frame.clone())),
            )
            .collect::<Vec<_>>();
        for (place, frame) in frames {
            let Some(&rest) = placed.get(&place) else {
                continue;
            };
            let target = match &lifted {
                Some((places, by)) if places.contains(&place) => {
                    Point::new(rest.x + by.x, rest.y + by.y)
                }
                _ => rest,
            };
            let at = match (progress, from.get(&place)) {
                (Some(t), Some(from)) => Point::new(
                    from.x + (target.x - from.x) * t,
                    from.y + (target.y - from.y) * t,
                ),
                _ => target,
            };
            shown.insert(place, at);
            let (_, natural, _, _) = frame.measure(gtk::Orientation::Vertical, cw);
            let transform = gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(
                (at.x + inset.x) as f32,
                (at.y + inset.y) as f32,
            ));
            frame.allocate(cw, natural.min(ch), -1, Some(transform));
        }
        drop(from);
        imp.shown.replace(shown);
        imp.resting.replace(
            placed
                .iter()
                .filter(|(place, _)| !is_lifted(place))
                .map(|(place, at)| (place.clone(), *at))
                .collect(),
        );
        drop(placed);
        if let Some(band) = imp.band.borrow().as_ref() {
            match *imp.banding.borrow() {
                Some((a, b, ..)) => {
                    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
                    let (w, h) = ((a.x - b.x).abs(), (a.y - b.y).abs());
                    band.set_visible(true);
                    band.allocate(
                        w.round() as i32,
                        h.round() as i32,
                        -1,
                        Some(
                            gtk::gsk::Transform::new()
                                .translate(&gtk::graphene::Point::new(x as f32, y as f32)),
                        ),
                    );
                }
                None => band.set_visible(false),
            }
        }
    }

    /// Keep the places for this size once the main loop comes round, and only once the
    /// folder is listed: while it is, the files not listed yet would lose theirs.
    fn queue_save(&self) {
        let imp = self.imp();
        if imp.save_queued.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move || {
                let imp = canvas.imp();
                imp.save_queued.set(false);
                if canvas.view().model().loading() {
                    return;
                }
                let (w, h) = imp.area.get();
                let size = Point::new(f64::from(w), f64::from(h));
                let mut placed = imp.placed.borrow().clone();
                // What has no tile keeps its place for when it has one: a drive taken out,
                // a place not shown for now, a file the view has yet to hand over, which it
                // may do after saying the folder is read. A file gone from the folder is gone.
                let folder = canvas.view().location();
                for (place, at) in imp.kept.borrow().iter() {
                    let there = match place {
                        Place::Special(_) => true,
                        Place::File(name) => folder
                            .as_ref()
                            .is_some_and(|f| f.child(name).query_exists(gio::Cancellable::NONE)),
                    };
                    if there {
                        placed.entry(place.clone()).or_insert(*at);
                    }
                }
                if let Some(positions) = imp.positions.borrow_mut().as_mut() {
                    positions.keep(size, &canvas.seen_from_start_side(&placed));
                    if let Err(e) = positions.save() {
                        glib::g_warning!(
                            "spiral",
                            "Could not keep the places of the desktop icons: {e}"
                        );
                    }
                }
                imp.kept.replace(placed);
            }
        ));
    }

    /// Places measured from the side the icons start at, or back: they are kept that way,
    /// so the icons stay by that side whichever it is.
    fn seen_from_start_side(&self, places: &HashMap<Place, Point>) -> HashMap<Place, Point> {
        let grid = self.grid();
        places
            .iter()
            .map(|(place, at)| (place.clone(), Point::new(grid.seen_from_start(at.x), at.y)))
            .collect()
    }

    /// The icons go over to the other side, each as far from it as it was from this one, as
    /// the side the grid starts at changes.
    fn mirror_places(&self) {
        let imp = self.imp();
        let width = f64::from(imp.area.get().0);
        let cell = self.grid().cell_size();
        let mirrored = |places: &HashMap<Place, Point>| {
            places
                .iter()
                .map(|(place, at)| (place.clone(), Point::new(width - cell.x - at.x, at.y)))
                .collect::<HashMap<_, _>>()
        };
        let kept = mirrored(&imp.kept.borrow());
        imp.kept.replace(kept);
        let placed = mirrored(&imp.placed.borrow());
        imp.placed.replace(placed);
        self.queue_save();
        self.queue_allocate();
    }

    fn rebind_captions(&self) {
        let view = self.view();
        let Some(model) = self.imp().model.borrow().clone() else {
            return;
        };
        for tile in self.imp().tiles.borrow().iter() {
            let captions = tile
                .cell
                .last_child()
                .and_then(|labels| labels.last_child());
            if let Some(captions) = captions.and_downcast::<gtk::Label>()
                && let Some(info) = model
                    .item(tile.slot.get())
                    .and_then(|o| crate::folder_model::info_of(&o))
            {
                view.bind_captions(&captions, &info);
            }
        }
        for tile in self.imp().specials.borrow().iter() {
            self.bind_special_captions(&tile.cell, &tile.special);
        }
        self.queue_allocate();
    }

    /// The captions under the name of a place, as under a file's. Home is a folder like
    /// any; the size of the trash is what it holds, and that of a drive the room left on it.
    fn bind_special_captions(&self, cell: &gtk::Box, special: &Special) {
        let Some(label) = cell
            .last_child()
            .and_then(|labels| labels.last_child())
            .and_downcast::<gtk::Label>()
        else {
            return;
        };
        let view = self.view();
        let kinds = view.caption_kinds();
        if kinds.is_empty() {
            label.set_visible(false);
            return;
        }
        let file = special.file.clone();
        let (trash, drive) = (special.is_trash(), special.mount().is_some());
        glib::spawn_future_local(glib::clone!(
            #[weak]
            label,
            #[weak]
            view,
            async move {
                let attributes = format!("{},trash::item-count", crate::file_utils::ATTRIBUTES);
                let Ok(info) = file
                    .query_info_future(
                        &attributes,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::LOW,
                    )
                    .await
                else {
                    label.set_visible(false);
                    return;
                };
                if !trash && !drive {
                    info.set_attribute_object("standard::file", &file);
                    view.bind_captions(&label, &info);
                    return;
                }
                let size = if trash {
                    let items = info.attribute_uint32("trash::item-count");
                    Some(crate::file_utils::items_string(u64::from(items)))
                } else {
                    file.query_filesystem_info_future("filesystem::free", glib::Priority::LOW)
                        .await
                        .ok()
                        .map(|fs| {
                            let free = crate::prefs::size(fs.attribute_uint64("filesystem::free"));
                            // Translators: the room left on a drive, under its name on the desktop.
                            gettext("%s free").replace("%s", &free)
                        })
                };
                let lines: Vec<String> = kinds
                    .iter()
                    .filter_map(|kind| match kind.as_str() {
                        "size" => size.clone(),
                        kind => crate::file_utils::caption(&info, kind),
                    })
                    .collect();
                label.set_text(&lines.join("\n"));
                label.set_visible(!lines.is_empty());
            }
        ));
    }

    /// The tiles of `moved` go `by` further, as the arrangement allows.
    /// The places first, then the files in the order they are sorted in.
    fn places(&self) -> Vec<Place> {
        let imp = self.imp();
        imp.specials
            .borrow()
            .iter()
            .map(|t| Place::Special(t.special.key.clone()))
            .chain(imp.tiles.borrow().iter().map(|t| t.place.clone()))
            .collect()
    }

    /// Lines the icons up in the sort order, as `auto` does, once: they stay where that
    /// puts them, to be moved on from there.
    fn sort_now(&self) {
        let imp = self.imp();
        let placed = placement::arrange(
            &self.grid(),
            Arrangement::Auto,
            &self.places(),
            &HashMap::new(),
        );
        imp.kept.borrow_mut().extend(placed.clone());
        imp.placed.replace(placed);
        self.queue_save();
        self.queue_allocate();
    }

    fn move_places(&self, moved: &[Place], by: Point) {
        let imp = self.imp();
        let grid = self.grid();
        let placed = placement::move_by(&grid, self.arrangement(), &imp.placed.borrow(), moved, by);
        imp.kept.borrow_mut().extend(placed.clone());
        imp.placed.replace(placed);
        self.queue_save();
        self.queue_allocate();
    }

    /// What a drag from (`x`, `y`) carries away: the files selected, with the tile under the
    /// pointer selected first, and the icons that show them and the places selected with
    /// them the whole way. None when only places are selected, which move within the
    /// desktop alone.
    pub(crate) fn prepare_drag(
        &self,
        x: f64,
        y: f64,
    ) -> Option<(Vec<gio::File>, Option<flock::Flock>)> {
        let hit = self.hit(x, y)?;
        if !self.is_selected(&hit) {
            self.select_only(&hit);
        }
        let view = self.view();
        let files = view.model().selected_files();
        if files.is_empty() {
            return None;
        }
        let imp = self.imp();
        imp.drag_origin.set(Some((x, y)));
        let model = view.model().selection();
        let tiles = imp.tiles.borrow();
        let going: Vec<&Tile> = tiles
            .iter()
            .filter(|t| model.is_selected(t.slot.get()))
            .collect();
        let distance = |tile: &&Tile| {
            tile.frame.compute_bounds(self).map_or(f32::MAX, |b| {
                let (dx, dy) = (b.center().x() - x as f32, b.center().y() - y as f32);
                dx * dx + dy * dy
            })
        };
        let leader = (0..going.len())
            .min_by(|&a, &b| distance(&going[a]).total_cmp(&distance(&going[b])))?;
        let going: Vec<gtk::Widget> = going.iter().map(|t| t.cell.clone().upcast()).collect();
        let selected = imp.special_selected.borrow();
        let staying: Vec<gtk::Widget> = imp
            .specials
            .borrow()
            .iter()
            .filter(|t| selected.contains(&t.special.key))
            .map(|t| t.cell.clone().upcast())
            .collect();
        let flock = flock::Flock::new(
            self.upcast_ref(),
            &going,
            &staying,
            leader,
            x,
            y,
            files.len(),
        );
        Some((files, flock))
    }

    /// The system has taken up the drag `prepare_drag` offered.
    pub(crate) fn begin_carry(&self, drag: &gdk::Drag, flock: Option<flock::Flock>) {
        let imp = self.imp();
        let Some(origin) = imp.drag_origin.take() else {
            return;
        };
        // A press on a place starts the desktop's own move of it too, which this takes over.
        imp.lifted.replace(None);
        if let Some(flock) = &flock {
            flock.begin(drag);
        }
        let places = self.selected_places();
        self.let_through(&places, true);
        imp.carry.replace(Some(Carry {
            places,
            flock,
            origin,
            away: false,
        }));
        self.queue_allocate();
    }

    /// The drag is over the desktop: coming back to it, the icons spread out of the pile.
    fn carry_back(&self) {
        let mut carry = self.imp().carry.borrow_mut();
        let Some(carry) = carry.as_mut().filter(|carry| carry.away) else {
            return;
        };
        carry.away = false;
        if let Some(flock) = &carry.flock {
            flock.spread();
        }
    }

    /// The drag has left the desktop: the files gather under the pointer, and the places,
    /// which cannot go anywhere else, leave the drag and the selection.
    fn go_away(&self) {
        let mut carry = self.imp().carry.borrow_mut();
        let Some(carry) = carry.as_mut().filter(|carry| !carry.away) else {
            return;
        };
        carry.away = true;
        let (files, places): (Vec<Place>, Vec<Place>) = carry
            .places
            .drain(..)
            .partition(|place| matches!(place, Place::File(_)));
        carry.places = files;
        self.let_through(&places, false);
        self.select_special(None);
        if let Some(flock) = &carry.flock {
            flock.gather();
        }
    }

    /// The drag is over without a drop on the desktop, which takes the carry itself.
    pub(crate) fn end_carry(&self) {
        let imp = self.imp();
        imp.drag_origin.set(None);
        if let Some(carry) = imp.carry.take() {
            self.let_through(&carry.places, false);
        }
    }

    fn frames_of(&self, places: &[Place]) -> Vec<gtk::Box> {
        let imp = self.imp();
        let tiles = imp.tiles.borrow();
        let specials = imp.specials.borrow();
        tiles
            .iter()
            .filter(|t| places.contains(&t.place))
            .map(|t| t.frame.clone())
            .chain(
                specials
                    .iter()
                    .filter(|t| places.contains(&Place::Special(t.special.key.clone())))
                    .map(|t| t.frame.clone()),
            )
            .collect()
    }

    /// Carried tiles are drawn under the pointer; a drop goes through them to what is below.
    fn let_through(&self, places: &[Place], through: bool) {
        for frame in self.frames_of(places) {
            frame.set_can_target(!through);
        }
    }

    /// Files dropped on the empty desktop at (`x`, `y`). Its own icons are moved there
    /// rather than copied onto themselves; the view copies or moves the rest into the
    /// folder, and they are put where they were dropped once they arrive.
    pub(crate) fn take_drop(&self, files: &[gio::File], x: f64, y: f64) -> bool {
        let view = self.view();
        let Some(folder) = view.location() else {
            return false;
        };
        let names: Vec<String> = files
            .iter()
            .filter_map(|f| f.basename())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        let here = files
            .iter()
            .all(|f| f.parent().is_some_and(|p| p.equal(&folder)));
        let imp = self.imp();
        if !here {
            let cell = self.grid().cell_size();
            let at = Point::new(x - cell.x / 2.0, y - cell.y / 2.0);
            imp.expected
                .borrow_mut()
                .extend(names.into_iter().map(|name| (name, at, Instant::now())));
            return false;
        }
        let carry = imp.carry.take();
        let (moved, by) = match carry {
            // Dragged from the desktop itself: what the drag still carries, the places with
            // the files, glides into place from where the drag showed it.
            Some(carry) => {
                let by = Point::new(x - carry.origin.0, y - carry.origin.1);
                self.let_through(&carry.places, false);
                let placed = imp.placed.borrow().clone();
                imp.shown
                    .borrow_mut()
                    .extend(carry.places.iter().filter_map(|place| {
                        let at = placed.get(place)?;
                        Some((place.clone(), Point::new(at.x + by.x, at.y + by.y)))
                    }));
                (carry.places, by)
            }
            // Dragged from a window showing the desktop's folder: the first goes where it
            // was dropped, the rest keep their places beside it.
            None => {
                let moved: Vec<Place> = names.into_iter().map(Place::File).collect();
                let cell = self.grid().cell_size();
                let first = moved
                    .first()
                    .and_then(|p| imp.placed.borrow().get(p).copied())
                    .unwrap_or(Point::new(MARGIN, MARGIN));
                let by = Point::new(x - cell.x / 2.0 - first.x, y - cell.y / 2.0 - first.y);
                (moved, by)
            }
        };
        self.move_places(&moved, by);
        true
    }

    fn hit(&self, x: f64, y: f64) -> Option<Hit> {
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let inside = |frame: &gtk::Box| {
            frame
                .compute_bounds(self)
                .is_some_and(|b| b.contains_point(&point))
        };
        let imp = self.imp();
        if let Some(tile) = imp.tiles.borrow().iter().find(|t| inside(&t.frame)) {
            return Some(Hit::Item(tile.slot.get()));
        }
        imp.specials
            .borrow()
            .iter()
            .find(|t| inside(&t.frame))
            .map(|t| Hit::Special(t.special.key.clone()))
    }

    fn setup_input(&self) {
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_PRIMARY)
            .build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, n, x, y| canvas.pressed(gesture, n, x, y)
        ));
        click.connect_released(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, n, x, y| canvas.released(gesture, n, x, y)
        ));
        self.add_controller(click);

        // A click on something selected keeps the selection, and the menu is that of the
        // files selected, whose Open opens the places as well; places alone have their own.
        // The view's menu handles the empty desktop.
        let secondary = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, _, x, y| {
                let Some(hit) = canvas.hit(x, y) else {
                    return;
                };
                canvas.grab_focus();
                if !canvas.is_selected(&hit) {
                    canvas.select_only(&hit);
                }
                canvas.set_cursor(Some(hit.clone()));
                let view = canvas.view();
                let first_file = view.model().selection().selection().minimum();
                match hit {
                    Hit::Item(_) => {}
                    Hit::Special(_) if !view.model().selection().selection().is_empty() => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        if let Some(p) = canvas.compute_point(
                            &view.imp().stack.get(),
                            &gtk::graphene::Point::new(x as f32, y as f32),
                        ) {
                            view.popup_menu_on(Some(first_file), p.x().into(), p.y().into());
                        }
                    }
                    Hit::Special(_) => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        if let Some(menu) = canvas.places_menu() {
                            canvas.popup(&menu, x, y);
                        }
                    }
                }
            }
        ));
        self.add_controller(secondary);

        let drag = gtk::GestureDrag::builder()
            .button(gdk::BUTTON_PRIMARY)
            .build();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, x, y| {
                canvas.imp().band_cancelled.set(false);
                canvas.band_begin(gesture, x, y);
            }
        ));
        // The system takes the press over when a drag of files starts from it.
        drag.connect_cancel(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| canvas.imp().band_cancelled.set(true)
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, dx, dy| {
                let imp = canvas.imp();
                if imp.carry.borrow().is_some() || imp.band_cancelled.get() {
                    return;
                }
                if let Some((_, by)) = imp.lifted.borrow_mut().as_mut() {
                    *by = Point::new(dx, dy);
                    canvas.queue_allocate();
                } else if let Some((x, y)) = gesture.start_point() {
                    canvas.band_to(Point::new(x + dx, y + dy));
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |gesture, dx, dy| {
                let imp = canvas.imp();
                imp.banding.replace(None);
                let cancelled = imp.band_cancelled.replace(false);
                if imp.carry.borrow().is_some() {
                    return;
                }
                if let Some((places, _)) = imp.lifted.take()
                    && !cancelled
                    && let Some((x, y)) = gesture.start_point()
                    && canvas.drag_check_threshold(
                        x as i32,
                        y as i32,
                        (x + dx) as i32,
                        (y + dy) as i32,
                    )
                {
                    imp.narrow.replace(None);
                    canvas.move_places(&places, Point::new(dx, dy));
                }
                canvas.queue_allocate();
            }
        ));
        self.add_controller(drag);

        let over = gtk::DropControllerMotion::new();
        over.connect_enter(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _, _| canvas.carry_back()
        ));
        over.connect_leave(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_| canvas.go_away()
        ));
        self.add_controller(over);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let handled = canvas.key(key, state);
                if handled == glib::Propagation::Stop {
                    canvas.show_focus(true);
                }
                handled
            }
        ));
        self.add_controller(keys);
    }

    /// Rings the cursor while the keyboard is in use. The desktop's window is never
    /// active, so GTK marks nothing in it as focused.
    fn show_focus(&self, shown: bool) {
        mark(self, "keyboard", shown);
    }

    fn pressed(&self, gesture: &gtk::GestureClick, n: i32, x: f64, y: f64) {
        self.grab_focus();
        self.show_focus(false);
        let imp = self.imp();
        imp.narrow.replace(None);
        let state = gesture.current_event_state();
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let Some(hit) = self.hit(x, y) else {
            if !ctrl && !shift {
                self.select_none();
            }
            return;
        };
        self.set_cursor(Some(hit.clone()));
        // Icons stand where they were put, in no order a range could follow, so Shift adds
        // to the selection as Ctrl does.
        if ctrl || shift {
            self.select_also(&hit, true);
        } else if self.is_selected(&hit) {
            imp.narrow.replace(Some(hit.clone()));
        } else {
            self.select_only(&hit);
        }
        if n == 2 && !crate::prefs::single_click() && !ctrl && !shift {
            imp.narrow.replace(None);
            self.open_hit(&hit);
        }
    }

    fn released(&self, gesture: &gtk::GestureClick, n: i32, x: f64, y: f64) {
        let imp = self.imp();
        let hit = self.hit(x, y);
        let narrowed = imp.narrow.take();
        if let Some(narrowed) = narrowed.filter(|narrowed| hit.as_ref() == Some(narrowed)) {
            self.select_only(&narrowed);
        }
        let state = gesture.current_event_state();
        let plain =
            !state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
        if n == 1
            && plain
            && crate::prefs::single_click()
            && let Some(hit) = hit
        {
            self.open_hit(&hit);
        }
    }

    fn band_begin(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        match self.hit(x, y) {
            Some(Hit::Special(key)) => {
                let places = if self.imp().special_selected.borrow().contains(&key) {
                    self.selected_places()
                } else {
                    vec![Place::Special(key)]
                };
                self.imp()
                    .lifted
                    .replace(Some((places, Point::new(0.0, 0.0))));
                return;
            }
            Some(Hit::Item(_)) => {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            None => {}
        }
        let model = self.view().model().selection();
        let ctrl = gesture
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK);
        let (before, places_before) = if ctrl {
            (
                model.selection(),
                self.imp().special_selected.borrow().clone(),
            )
        } else {
            (gtk::Bitset::new_empty(), HashSet::new())
        };
        let start = Point::new(x, y);
        self.imp()
            .banding
            .replace(Some((start, start, before, places_before)));
    }

    fn band_to(&self, end: Point) {
        let imp = self.imp();
        let Some((start, _, before, places_before)) = imp.banding.borrow().clone() else {
            return;
        };
        imp.banding
            .replace(Some((start, end, before.clone(), places_before.clone())));
        let rect = gtk::graphene::Rect::new(
            start.x.min(end.x) as f32,
            start.y.min(end.y) as f32,
            (start.x - end.x).abs() as f32,
            (start.y - end.y).abs() as f32,
        );
        let selected = before.copy();
        for tile in imp.tiles.borrow().iter() {
            if tile
                .frame
                .compute_bounds(self)
                .is_some_and(|b| b.intersection(&rect).is_some())
            {
                selected.add(tile.slot.get());
            }
        }
        let mut places = places_before;
        for tile in imp.specials.borrow().iter() {
            if tile
                .frame
                .compute_bounds(self)
                .is_some_and(|b| b.intersection(&rect).is_some())
            {
                places.insert(tile.special.key.clone());
            }
        }
        let model = self.view().model().selection();
        let all = gtk::Bitset::new_range(0, model.n_items());
        model.set_selection(&selected, &all);
        self.set_specials(places);
        self.queue_allocate();
    }

    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let step = match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some((-1.0, 0.0)),
            gdk::Key::Right | gdk::Key::KP_Right => Some((1.0, 0.0)),
            gdk::Key::Up | gdk::Key::KP_Up => Some((0.0, -1.0)),
            gdk::Key::Down | gdk::Key::KP_Down => Some((0.0, 1.0)),
            _ => None,
        };
        if let Some(step) = step {
            if let Some(hit) = self.neighbour(step) {
                self.go_to_hit(hit, shift);
            }
            return glib::Propagation::Stop;
        }
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter
                if !state.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::SHIFT_MASK
                        | gdk::ModifierType::ALT_MASK,
                ) =>
            {
                // Open opens the places along with the files; with no files it is off.
                if self.view().model().selection().selection().is_empty() {
                    self.open_selected_places();
                } else {
                    let _ = self.view().activate_action("view.open", None);
                }
                return glib::Propagation::Stop;
            }
            gdk::Key::Escape => {
                self.select_none();
                return glib::Propagation::Stop;
            }
            _ => {}
        }
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        match key.to_unicode().filter(|c| !c.is_control()) {
            Some(c) => {
                self.type_ahead(c);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    /// Where every tile is, the places and the files alike.
    fn hits(&self) -> Vec<(Hit, Point)> {
        let imp = self.imp();
        let placed = imp.placed.borrow();
        let specials = imp.specials.borrow();
        let tiles = imp.tiles.borrow();
        specials
            .iter()
            .filter_map(|t| {
                let at = placed.get(&Place::Special(t.special.key.clone()))?;
                Some((Hit::Special(t.special.key.clone()), *at))
            })
            .chain(tiles.iter().filter_map(|t| {
                let at = placed.get(&t.place)?;
                Some((Hit::Item(t.slot.get()), *at))
            }))
            .collect()
    }

    /// The tile nearest the cursor's in the direction `(dx, dy)`, or the first of all when
    /// the keyboard is on none.
    fn neighbour(&self, (dx, dy): (f64, f64)) -> Option<Hit> {
        let hits = self.hits();
        let current = self.imp().cursor.borrow().clone();
        let from = current
            .as_ref()
            .and_then(|c| hits.iter().find(|(h, _)| h == c).map(|(_, at)| *at));
        let Some(from) = from else {
            return hits
                .iter()
                .min_by(|a, b| (a.1.x, a.1.y).partial_cmp(&(b.1.x, b.1.y)).unwrap())
                .map(|(h, _)| h.clone());
        };
        hits.into_iter()
            .filter(|(h, _)| Some(h) != current.as_ref())
            .filter_map(|(h, at)| {
                let (x, y) = (at.x - from.x, at.y - from.y);
                let along = x * dx + y * dy;
                if along <= 0.5 {
                    return None;
                }
                let across = (x * dy - y * dx).abs();
                Some((h, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(h, _)| h)
    }

    fn go_to_hit(&self, hit: Hit, extend: bool) {
        if extend {
            self.select_also(&hit, false);
        } else {
            self.select_only(&hit);
        }
        self.set_cursor(Some(hit));
    }

    /// Letters typed in a run pick the first name that starts with them.
    fn type_ahead(&self, c: char) {
        let imp = self.imp();
        let wanted = {
            let mut typed = imp.typed.borrow_mut();
            if typed.1.is_none_or(|at| at.elapsed() > TYPE_AHEAD_PAUSE) {
                typed.0.clear();
            }
            typed.0.extend(c.to_lowercase());
            typed.1 = Some(Instant::now());
            typed.0.clone()
        };
        let model = self.view().model();
        let found = (0..model.n_items()).find(|&at| {
            model
                .info_at(at)
                .is_some_and(|info| info.display_name().to_lowercase().starts_with(&wanted))
        });
        if let Some(at) = found {
            self.go_to_hit(Hit::Item(at), false);
        }
    }

    fn popup(&self, menu: &gio::Menu, x: f64, y: f64) {
        let view = self.view();
        let point = self
            .compute_point(&view, &gtk::graphene::Point::new(x as f32, y as f32))
            .map(|p| (f64::from(p.x()), f64::from(p.y())))
            .unwrap_or((x, y));
        view.popup_model(menu.upcast_ref(), point.0, point.1);
    }

    /// The menu of the empty desktop.
    pub(crate) fn background_menu(&self) -> gio::MenuModel {
        let view = self.view();
        let menu = gio::Menu::new();
        menu.append_section(None, &*view.imp().new_section);
        let edit = gio::Menu::new();
        edit.append(Some(&gettext("_Paste")), Some("view.paste"));
        edit.append(Some(&gettext("Select _All")), Some("view.select-all"));
        menu.append_section(None, &edit);

        let arrange = gio::Menu::new();
        for (label, nick) in [
            (gettext("In a _Grid"), "grid"),
            (gettext("By _Sort Order"), "auto"),
            (gettext("_Anywhere"), "free"),
        ] {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("desktop.desktop-arrangement"),
                Some(&nick.to_variant()),
            );
            arrange.append_item(&item);
        }
        let sort = gio::Menu::new();
        let keys = gio::Menu::new();
        for (label, nick) in [
            (gettext("_Name"), "name"),
            (gettext("_Size"), "size"),
            (gettext("_Type"), "type"),
            (gettext("_Modified"), "modified"),
        ] {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("desktop.desktop-sort-key"),
                Some(&nick.to_variant()),
            );
            keys.append_item(&item);
        }
        sort.append_section(None, &keys);
        let order = gio::Menu::new();
        order.append(
            Some(&gettext("_Reversed Order")),
            Some("desktop.desktop-sort-reversed"),
        );
        sort.append_section(None, &order);
        let arranging = gio::Menu::new();
        arranging.append_submenu(Some(&gettext("A_rrange Icons")), &arrange);
        arranging.append_submenu(Some(&gettext("S_ort By")), &sort);
        if self.arrangement() != Arrangement::Auto {
            arranging.append(Some(&gettext("Sort _Now")), Some("desktop.sort-now"));
        }
        menu.append_section(None, &arranging);

        // Only when there is a terminal to open, as in the view's own menus.
        let terminal_found = view
            .imp()
            .actions
            .lookup_action("folder-terminal")
            .is_some_and(|action| action.is_enabled());
        if terminal_found {
            let terminal = gio::Menu::new();
            terminal.append(
                Some(&gettext("Open in _Terminal")),
                Some("view.folder-terminal"),
            );
            menu.append_section(None, &terminal);
        }

        let settings = gio::Menu::new();
        settings.append(
            Some(&gettext("Desktop _Settings…")),
            Some("desktop.settings"),
        );
        menu.append_section(None, &settings);

        let entries = drop_ins::load(&drop_ins::dirs(), None);
        if !entries.is_empty() {
            let section = gio::Menu::new();
            for app in &entries {
                let Some(path) = app.filename() else {
                    continue;
                };
                let item = gio::MenuItem::new(Some(&app.name()), None);
                item.set_action_and_target_value(
                    Some("desktop.drop-in"),
                    Some(&path.to_string_lossy().to_variant()),
                );
                section.append_item(&item);
            }
            menu.append_section(None, &section);
        }
        menu.upcast()
    }

    fn setup_actions(&self, view: &BrowserView) {
        let group = gio::SimpleActionGroup::new();
        let settings = view.imp().settings.clone();
        for key in [
            "desktop-arrangement",
            "desktop-sort-key",
            "desktop-sort-reversed",
        ] {
            group.add_action(&settings.create_action(key));
        }

        let with_key = |name: &str, f: fn(&IconCanvas, &str)| {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            action.connect_activate(glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, target| {
                    if let Some(key) = target.and_then(|t| t.get::<String>()) {
                        f(&canvas, &key);
                    }
                }
            ));
            group.add_action(&action);
        };
        with_key("open-special", |c, key| c.open_special(key));
        let open = gio::SimpleAction::new("open-places", None);
        open.connect_activate(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| canvas.open_selected_places()
        ));
        group.add_action(&open);
        with_key("unmount-special", |c, key| c.leave_special(key, false));
        with_key("eject-special", |c, key| c.leave_special(key, true));
        with_key("drop-in", |c, path| c.run_drop_in(path));

        let sort_now = gio::SimpleAction::new("sort-now", None);
        sort_now.connect_activate(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| canvas.sort_now()
        ));
        group.add_action(&sort_now);

        let empty = gio::SimpleAction::new("empty-trash", None);
        empty.connect_activate(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| canvas.view().empty_trash()
        ));
        group.add_action(&empty);

        let preferences = gio::SimpleAction::new("settings", None);
        preferences.connect_activate(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_, _| {
                let dialog = crate::dialogs::preferences_dialog();
                dialog.set_visible_page_name("desktop");
                dialog.present(Some(&canvas));
            }
        ));
        group.add_action(&preferences);
        view.insert_action_group("desktop", Some(&group));
        self.imp().actions.replace(Some(group));
    }

    fn run_drop_in(&self, path: &str) {
        let Some(app) = gio_unix::DesktopAppInfo::from_filename(path) else {
            return;
        };
        let context = self.display().app_launch_context();
        if let Err(e) = app.launch(&[], Some(&context)) {
            self.view()
                .show_error(&gettext("Could Not Open"), e.message());
        }
    }

    fn follow_settings(&self, view: &BrowserView) {
        let settings = view.imp().settings.clone();
        let relayout = glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_: &gio::Settings, _: &str| canvas.queue_allocate()
        );
        for key in ["desktop-arrangement", "desktop-icon-spacing"] {
            settings.connect_changed(Some(key), relayout.clone());
        }
        settings.connect_changed(
            Some("desktop-alignment"),
            glib::clone!(
                #[weak(rename_to = canvas)]
                self,
                move |_, _| canvas.mirror_places()
            ),
        );
        for key in ["captions", "desktop-show-captions"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = canvas)]
                    self,
                    move |_, _| canvas.rebind_captions()
                ),
            );
        }
        for key in ["desktop-sort-key", "desktop-sort-reversed"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak]
                    view,
                    move |_, _| view.apply_global_sort()
                ),
            );
        }
        for key in [
            "desktop-show-home",
            "desktop-show-trash",
            "desktop-show-drives",
        ] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak(rename_to = canvas)]
                    self,
                    move |_, _| canvas.rebuild_specials()
                ),
            );
        }
        view.connect_icon_size_notify(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |_| canvas.queue_allocate()
        ));
        view.model().connect_loading_notify(glib::clone!(
            #[weak(rename_to = canvas)]
            self,
            move |model| {
                if !model.loading() {
                    canvas.queue_save();
                }
            }
        ));
    }
}

/// Add or take off a CSS class as `on` says.
fn mark(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}
