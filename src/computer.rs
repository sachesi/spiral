//! The Computer page at `computer:///`: the disks of this computer, the drives plugged into
//! it and the servers it reaches, each with the room left on it. Off unless turned on.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;

use crate::browser_view::BrowserView;
use crate::enums::ViewMode;
use crate::network::MountTable;
use crate::object_data::Key;
use crate::places_sidebar::{DeviceWatch, EjectTarget, PlacesSidebar};
use crate::{adw, gdk, gio, glib, gtk};

pub const URI: &str = "computer:///";

pub fn is_computer_location(file: &gio::File) -> bool {
    file.uri().starts_with("computer:")
}

/// Where a window opened without a location starts.
pub fn start_location() -> gio::File {
    if crate::prefs::start_at_computer() {
        gio::File::for_uri(URI)
    } else {
        gio::File::for_path(glib::home_dir())
    }
}

/// Show on `bar` how full a disk is, `fill` from 0 to 1: one colour, and the warning
/// colour once it is nearly out of room. The bar's own levels would show a bar near its
/// end as good news, which on a disk it is not.
pub(crate) fn show_usage(bar: &gtk::LevelBar, fill: f64) {
    for offset in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(offset));
    }
    bar.set_value(fill);
    if fill > 0.9 {
        bar.add_css_class("spiral-nearly-full");
    } else {
        bar.remove_css_class("spiral-nearly-full");
    }
}

/// What a tile stands for.
#[derive(Clone)]
enum Place {
    /// The filesystem the system runs from, which no volume stands for.
    System,
    Volume(gio::Volume),
    /// A mount without a volume: a share, a phone, a disk image.
    Mount(gio::Mount),
    /// A server connected to before, and not now.
    Server(gio::File),
}

impl Place {
    /// Where opening it goes; None for a volume still to be mounted.
    fn file(&self) -> Option<gio::File> {
        match self {
            Place::System => Some(gio::File::for_path("/")),
            Place::Volume(v) => v.get_mount().map(|m| m.default_location()),
            Place::Mount(m) => Some(m.default_location()),
            Place::Server(f) => Some(f.clone()),
        }
    }

    /// The filesystem to ask how full it is.
    fn filesystem(&self) -> Option<gio::File> {
        match self {
            Place::System => Some(gio::File::for_path("/")),
            Place::Volume(v) => v.get_mount().map(|m| m.root()),
            Place::Mount(m) => Some(m.root()),
            Place::Server(_) => None,
        }
    }

    /// What the sidebar hides it as, so a disk hidden there is hidden here too. The
    /// system's own disk is always shown.
    fn hide_key(&self) -> Option<String> {
        match self {
            Place::Volume(v) => crate::places_sidebar::volume_hide_key(v),
            Place::Mount(m) => Some(crate::places_sidebar::mount_hide_key(m)),
            _ => None,
        }
    }

    fn eject_target(&self, mounts: &MountTable) -> Option<EjectTarget> {
        let mount = match self {
            Place::Volume(v) => match v.get_mount() {
                Some(m) => m,
                None if v.can_eject() => return Some(EjectTarget::Volume(v.clone())),
                None => return None,
            },
            Place::Mount(m) => m.clone(),
            _ => return None,
        };
        crate::places_sidebar::may_unmount(&mount, mounts).then_some(EjectTarget::Mount(mount))
    }

    /// Where it is, for its tooltip: the folder a disk is mounted at and its filesystem, the
    /// device of one that is not mounted, or the address of a share or a server.
    fn whereabouts(&self, mounts: &MountTable) -> Option<String> {
        let root = match self {
            Place::Server(f) => return Some(f.parse_name().into()),
            Place::Volume(v) if v.get_mount().is_none() => {
                return v.identifier("unix-device").map(Into::into);
            }
            _ => self.filesystem()?,
        };
        // Asked for its path, a file gvfs mounted waits on the gvfs daemon.
        let Some(path) = root.is_native().then(|| root.path()).flatten() else {
            return Some(root.parse_name().into());
        };
        let at = path.display().to_string();
        Some(match mounts.at(&path) {
            Some(m) => format!("{at} ({})", m.fstype.trim_start_matches("fuse.")),
            None => at,
        })
    }

    /// Names the place across rebuilds, for the keyboard to stay on it.
    fn id(&self) -> String {
        match self {
            Place::System => "system".into(),
            Place::Volume(v) => self.hide_key().unwrap_or_else(|| v.name().into()),
            Place::Mount(_) => self.hide_key().unwrap_or_default(),
            Place::Server(f) => f.uri().into(),
        }
    }

    /// Whether it is on another machine: a share, or a drive an online account mounted.
    fn on_network(&self, mounts: &MountTable) -> bool {
        match self {
            Place::System => false,
            Place::Volume(v) => match v.get_mount() {
                Some(m) => crate::network::is_network_mount(&m.root(), mounts),
                None => v
                    .activation_root()
                    .is_some_and(|f| crate::network::is_network(&f)),
            },
            Place::Mount(m) => crate::network::is_network_mount(&m.root(), mounts),
            Place::Server(_) => true,
        }
    }

    fn removable(&self) -> bool {
        let drive = match self {
            Place::Volume(v) => v.drive(),
            Place::Mount(m) => m.drive(),
            _ => None,
        };
        drive.is_some_and(|d| d.is_removable() || d.is_media_removable())
            || match self {
                Place::Volume(v) => v.can_eject(),
                Place::Mount(m) => m.can_eject(),
                _ => false,
            }
    }
}

static PLACE: Key<Place> = Key::new("computer-place");

/// A tile's parts that fill in once the filesystem has said how full it is.
#[derive(Clone)]
struct Usage {
    bar: gtk::LevelBar,
    caption: gtk::Label,
    /// Counted in the total of the header: a disk of this computer.
    local: bool,
    /// What a share that does not say how big it is is captioned with instead.
    no_size: Option<String>,
}

/// What a filesystem said about how full it is.
#[derive(Clone)]
enum Reading {
    Size {
        free: u64,
        size: u64,
        used: u64,
    },
    /// A share that does not say, or did not answer.
    Unsized,
}

impl Usage {
    fn show(&self, reading: &Reading) {
        match *reading {
            Reading::Size { free, size, used } => {
                show_usage(&self.bar, used as f64 / size as f64);
                self.bar.set_visible(true);
                self.caption.set_label(&free_of(free, size));
                self.caption.set_visible(true);
            }
            Reading::Unsized => {
                if let Some(text) = &self.no_size {
                    self.caption.set_label(text);
                    self.caption.set_visible(true);
                }
            }
        }
        describe(&self.caption);
    }
}

/// The parts of a tile that show how full its disk is.
static USAGE: Key<Usage> = Key::new("computer-usage");

/// The spinner of a tile, and the button it stands in for while the place is mounted or
/// ejected.
static BUSY: Key<(adw::Spinner, Option<gtk::Button>)> = Key::new("computer-busy");

struct Group {
    root: gtk::Box,
    tiles: gtk::FlowBox,
}

struct Parts {
    title: gtk::Label,
    space: gtk::Label,
    computer: Group,
    removable: Group,
    network: Group,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ComputerPage {
        pub view: glib::WeakRef<BrowserView>,
        pub(super) parts: OnceCell<Parts>,
        pub(crate) devices: RefCell<Option<DeviceWatch>>,
        pub settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub jobs_handler: RefCell<Option<(crate::ops::JobManager, glib::SignalHandlerId)>>,
        /// Whether the places hidden from the page are shown, dimmed, to be brought back.
        /// Kept with the page and never saved, as the sidebar keeps its own.
        pub shows_hidden: Cell<bool>,
        /// Whether any place is hidden from the page, for the menu to offer to show them.
        pub has_hidden: Cell<bool>,
        /// The window the page is in, whose coming back to the front reads the disks again.
        pub window: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
        pub actions: gio::SimpleActionGroup,
        pub menu_tile: RefCell<Option<gtk::FlowBoxChild>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        /// Whether the arrow being handled goes down (or up), for when the box gives up on
        /// it: GTK 4.22 reports every failed move as one down, having counted the steps
        /// off the move before it says which way it went.
        pub vertical: Cell<Option<bool>>,
        /// The places whose filesystem is being asked how full it is, by id. One that does
        /// not answer, a share whose server went away, is not asked again until it has.
        pub asking: RefCell<std::collections::HashSet<String>>,
        /// The device each disk of this computer is on, by id, for the total to count a
        /// device once however many of its filesystems are shown.
        pub(super) disks: RefCell<std::collections::HashMap<String, Option<String>>>,
        /// What each place last said about how full it is, by id, shown on its tile while
        /// it is asked again so the bars do not blink with every change.
        pub(super) readings: RefCell<std::collections::HashMap<String, Reading>>,
        /// Cards in a grid, or a row for each place: what the view button picks here.
        pub layout: Cell<ViewMode>,
        /// The places being mounted or ejected, by id.
        pub busy: RefCell<std::collections::HashSet<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ComputerPage {
        const NAME: &'static str = "SpiralComputerPage";
        type Type = super::ComputerPage;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for ComputerPage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.layout.set(remembered_layout());
            obj.build();
            obj.setup_actions();
            obj.watch();
        }

        fn dispose(&self) {
            self.devices.take();
            let settings = crate::prefs::settings();
            for id in self.settings_handlers.take() {
                settings.disconnect(id);
            }
            if let Some((jobs, id)) = self.jobs_handler.take() {
                jobs.disconnect(id);
            }
            self.obj().unwatch_window();
            if let Some(popover) = self.popover.take() {
                popover.unparent();
            }
        }
    }

    impl WidgetImpl for ComputerPage {}
    impl BinImpl for ComputerPage {}
}

mod frame {
    use super::*;

    #[derive(Default)]
    pub(crate) struct TileFrame;

    #[glib::object_subclass]
    impl ObjectSubclass for TileFrame {
        const NAME: &'static str = "SpiralComputerTileFrame";
        type Type = super::TileFrame;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TileFrame {}

    impl WidgetImpl for TileFrame {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (min, nat, min_baseline, nat_baseline) = self.parent_measure(orientation, for_size);
            match orientation {
                gtk::Orientation::Horizontal => (min, min, -1, -1),
                _ => (min, nat, min_baseline, nat_baseline),
            }
        }
    }

    impl BinImpl for TileFrame {}
}

glib::wrapper! {
    /// A tile that asks for no more width than its least. The box puts as many tiles on a
    /// line as their natural width allows and stretches them to fill it, so a tile as wide
    /// as its name would leave room for fewer, every one of them wider. A long name is cut
    /// short instead, and only where the tile is short of room.
    pub(crate) struct TileFrame(ObjectSubclass<frame::TileFrame>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

glib::wrapper! {
    pub struct ComputerPage(ObjectSubclass<imp::ComputerPage>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ComputerPage {
    pub fn new(view: &BrowserView) -> Self {
        let page: Self = glib::Object::new();
        page.imp().view.set(Some(view));
        page
    }

    fn parts(&self) -> &Parts {
        self.imp().parts.get().expect("built in constructed")
    }

    fn build(&self) {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build();
        let line = || {
            gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "numeric"])
                .build()
        };
        let system = line();
        match glib::os_info("PRETTY_NAME") {
            Some(name) => system.set_label(&name),
            None => system.set_visible(false),
        }
        let space = line();
        space.set_visible(false);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        text.append(&title);
        text.append(&system);
        text.append(&space);
        let header = gtk::Box::builder().spacing(18).build();
        crate::prefs::settings()
            .bind("show-computer-summary", &header, "visible")
            .get()
            .build();
        header.append(
            &gtk::Image::builder()
                .gicon(&system_logo())
                .pixel_size(64)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
        );
        header.append(&text);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        content.append(&header);
        let computer = self.group(&gettext("This Computer"), None);
        let removable = self.group(&gettext("Removable"), None);
        let connect = gtk::Button::builder()
            .label(gettext("Connect to Server…"))
            .action_name("win.connect-server")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let network = self.group(&gettext("Network"), Some(connect.upcast_ref()));
        for group in [&computer, &removable, &network] {
            content.append(&group.root);
        }
        let clamp = adw::Clamp::builder()
            .maximum_size(1200)
            .tightening_threshold(900)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .child(&content)
            .build();
        self.set_child(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&clamp)
                .build(),
        ));
        let _ = self.imp().parts.set(Parts {
            title,
            space,
            computer,
            removable,
            network,
        });
        self.apply_layout();

        // The menu of the tile the keyboard is on; a right click opens the tile's own.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                use gdk::{Key as K, ModifierType as M};
                let menu = key == K::Menu || (key == K::F10 && state.contains(M::SHIFT_MASK));
                match page.focused_tile() {
                    Some(tile) if menu => {
                        let (x, y) = (tile.width() as f64 / 2.0, tile.height() as f64 / 2.0);
                        page.popup_menu(&tile, x, y);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        self.add_controller(keys);
        let arrows = gtk::EventControllerKey::new();
        arrows.set_propagation_phase(gtk::PropagationPhase::Capture);
        arrows.connect_key_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                use gdk::Key as K;
                page.imp().vertical.set(match key {
                    K::Down | K::KP_Down => Some(true),
                    K::Up | K::KP_Up => Some(false),
                    _ => None,
                });
                glib::Propagation::Proceed
            }
        ));
        self.add_controller(arrows);
        // The tiles are made as the page comes on screen, after the view has asked for the
        // keyboard; it goes to them then, unless it is in a text entry.
        self.connect_map(|page| {
            page.watch_window();
            page.refresh();
            let typing = page
                .root()
                .and_then(|r| r.focus())
                .is_some_and(|w| w.is::<gtk::Editable>());
            if !typing {
                page.take_focus();
            }
        });
    }

    fn group(&self, title: &str, suffix: Option<&gtk::Widget>) -> Group {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let label = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let heading = gtk::Box::builder().spacing(6).build();
        heading.append(&label);
        if let Some(suffix) = suffix {
            heading.append(suffix);
        }
        let tiles = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(4)
            .row_spacing(12)
            .column_spacing(12)
            .build();
        tiles.update_relation(&[gtk::accessible::Relation::LabelledBy(&[label.upcast_ref()])]);
        tiles.connect_child_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_, tile| page.open(tile, false)
        ));
        tiles.connect_keynav_failed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |tiles, _| page.cross_rows(tiles)
        ));
        root.append(&heading);
        root.append(&tiles);
        Group { root, tiles }
    }

    /// Up or down where the box has no tile in that column goes to the nearest tile of the
    /// next row that has one, in this group or the ones after it.
    fn cross_rows(&self, from: &gtk::FlowBox) -> glib::Propagation {
        let Some(down) = self.imp().vertical.get() else {
            return glib::Propagation::Proceed;
        };
        let parts = self.parts();
        let mut groups: Vec<&gtk::FlowBox> = [&parts.computer, &parts.removable, &parts.network]
            .into_iter()
            .filter(|g| g.root.is_visible())
            .map(|g| &g.tiles)
            .collect();
        if !down {
            groups.reverse();
        }
        let (Some(at), Some(tile)) = (groups.iter().position(|g| *g == from), self.focused_tile())
        else {
            return glib::Propagation::Proceed;
        };
        let bounds = |w: &gtk::Widget| w.compute_bounds(self);
        let Some(here) = bounds(tile.upcast_ref()) else {
            return glib::Propagation::Proceed;
        };
        for group in &groups[at..] {
            let mut tiles = Vec::new();
            let mut child = group.first_child();
            while let Some(c) = child {
                child = c.next_sibling();
                let Some(b) = bounds(&c) else { continue };
                let beyond = if down {
                    b.y() > here.y()
                } else {
                    b.y() < here.y()
                };
                if beyond {
                    tiles.push((c, b));
                }
            }
            let ys = tiles.iter().map(|(_, b)| b.y());
            let row = if down {
                ys.reduce(f32::min)
            } else {
                ys.reduce(f32::max)
            };
            let nearest = tiles
                .into_iter()
                .filter(|(_, b)| Some(b.y()) == row)
                .min_by(|(_, a), (_, b)| {
                    (a.x() - here.x())
                        .abs()
                        .total_cmp(&(b.x() - here.x()).abs())
                })
                .and_then(|(c, _)| c.downcast::<gtk::FlowBoxChild>().ok());
            if let Some(tile) = nearest {
                focus_tile(&tile);
                return glib::Propagation::Stop;
            }
        }
        glib::Propagation::Proceed
    }

    fn watch(&self) {
        let imp = self.imp();
        let changed = glib::clone!(
            #[weak(rename_to = page)]
            self,
            move || {
                if page.is_mapped() {
                    page.refresh();
                }
            }
        );
        imp.devices.replace(Some(DeviceWatch::new(changed.clone())));
        let settings = crate::prefs::settings();
        for key in [
            "computer-hidden-places",
            "use-network",
            "network-servers",
            "size-units",
        ] {
            let id = settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[strong]
                    changed,
                    move |_, _| changed()
                ),
            );
            imp.settings_handlers.borrow_mut().push(id);
        }
        // A copy or a move changes how full the disks are; read them again once it ends.
        if let Some(app) =
            gio::Application::default().and_downcast::<crate::application::SpiralApplication>()
        {
            let jobs = app.job_manager().clone();
            let running = Cell::new(jobs.running());
            let id = jobs.connect_running_notify(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |jobs| {
                    if jobs.running() < running.replace(jobs.running()) && page.is_mapped() {
                        page.reread();
                    }
                }
            ));
            imp.jobs_handler.replace(Some((jobs, id)));
        }
    }

    /// Cards in a grid, or a row for each place.
    pub fn layout(&self) -> ViewMode {
        self.imp().layout.get()
    }

    /// Lay the places out as `mode` asks, and keep it for next time. The columns have
    /// nothing to show here and stand for the list.
    pub fn set_layout(&self, mode: ViewMode) {
        let mode = match mode {
            ViewMode::Grid => ViewMode::Grid,
            _ => ViewMode::List,
        };
        if mode == self.layout() {
            return;
        }
        self.imp().layout.set(mode);
        crate::prefs::set_list_view(URI, LAYOUT_ATTRIBUTE, mode.nick());
        self.apply_layout();
        if self.is_mapped() {
            self.refresh();
        }
    }

    fn apply_layout(&self) {
        let list = self.layout() == ViewMode::List;
        let parts = self.parts();
        for group in [&parts.computer, &parts.removable, &parts.network] {
            group
                .tiles
                .set_max_children_per_line(if list { 1 } else { 4 });
            group.tiles.set_row_spacing(if list { 6 } else { 12 });
        }
    }

    fn sidebar(&self) -> Option<PlacesSidebar> {
        self.root()
            .and_downcast::<crate::window::SpiralWindow>()
            .map(|w| w.sidebar())
    }

    /// Read the disks again whenever the window comes back to the front, for what other
    /// programs wrote meanwhile, from the window the page is in now.
    fn watch_window(&self) {
        let window = self.root().and_downcast::<gtk::Window>();
        let watched = self
            .imp()
            .window
            .borrow()
            .as_ref()
            .and_then(|(w, _)| w.upgrade());
        if watched == window {
            return;
        }
        self.unwatch_window();
        let Some(window) = window else { return };
        let id = window.connect_is_active_notify(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |window| {
                if window.is_active() && page.is_mapped() {
                    page.reread();
                }
            }
        ));
        self.imp().window.replace(Some((window.downgrade(), id)));
    }

    fn unwatch_window(&self) {
        if let Some((window, id)) = self.imp().window.take()
            && let Some(window) = window.upgrade()
        {
            window.disconnect(id);
        }
    }

    /// What there is to show, by group.
    fn places(mounts: &MountTable) -> Vec<(u8, Place)> {
        const COMPUTER: u8 = 0;
        const REMOVABLE: u8 = 1;
        const NETWORK: u8 = 2;
        let monitor = gio::VolumeMonitor::get();
        let network = crate::prefs::use_network();
        let group = |place: Place| {
            let group = if network && place.on_network(mounts) {
                NETWORK
            } else if place.removable() {
                REMOVABLE
            } else {
                COMPUTER
            };
            (group, place)
        };
        let mut places = vec![(COMPUTER, Place::System)];
        places.extend(monitor.volumes().into_iter().map(Place::Volume).map(group));
        let mounts = monitor.mounts();
        for mount in &mounts {
            if !mount.is_shadowed() && mount.volume().is_none() {
                places.push(group(Place::Mount(mount.clone())));
            }
        }
        if network && !crate::network::address_schemes().is_empty() {
            for uri in crate::network::servers() {
                let server = gio::File::for_uri(&uri);
                let connected = mounts.iter().any(|m| {
                    let root = m.root();
                    root.equal(&server) || server.has_prefix(&root)
                });
                if !connected {
                    places.push((NETWORK, Place::Server(server)));
                }
            }
        }
        places
    }

    /// Lay the tiles out afresh from what there is now, and ask each filesystem how full
    /// it is.
    pub fn refresh(&self) {
        let imp = self.imp();
        let parts = self.parts();
        let focused = self
            .focused_tile()
            .and_then(|t| PLACE.get(&t))
            .map(|p| p.id());
        let groups = [&parts.computer, &parts.removable, &parts.network];
        for group in groups {
            group.tiles.remove_all();
        }
        let hidden = crate::prefs::hidden_on_computer();
        let sidebar = self.sidebar();
        let mounts = MountTable::read();
        let places = Self::places(&mounts);
        let is_hidden = |place: &Place| place.hide_key().is_some_and(|k| hidden.contains(&k));
        imp.has_hidden.set(places.iter().any(|(_, p)| is_hidden(p)));
        if !imp.has_hidden.get() {
            imp.shows_hidden.set(false);
        }
        let mut refocus = None;
        let mut shown = std::collections::HashSet::new();
        for (group, place) in places {
            let is_hidden = is_hidden(&place);
            if is_hidden && !imp.shows_hidden.get() {
                continue;
            }
            let (tile, usage) = self.tile(&place, sidebar.as_ref(), &mounts);
            if is_hidden {
                tile.add_css_class("spiral-hidden-place");
            }
            if focused.as_deref() == Some(place.id().as_str()) {
                refocus = Some(tile.clone());
            }
            groups[group as usize].tiles.append(&tile);
            if let Some(usage) = usage {
                if let Some(reading) = imp.readings.borrow().get(&place.id()) {
                    usage.show(reading);
                }
                USAGE.set(&tile, usage);
            }
            shown.insert(place.id());
        }
        imp.readings.borrow_mut().retain(|id, _| shown.contains(id));
        imp.disks.borrow_mut().retain(|id, _| shown.contains(id));
        // One gone while it was asked is asked again once it is back, even if that answer
        // never came.
        imp.asking.borrow_mut().retain(|id| shown.contains(id));
        for group in groups {
            let any = group.tiles.first_child().is_some();
            group.tiles.set_visible(any);
            group.root.set_visible(any);
        }
        // The network is there to connect to a server from even with none connected.
        if crate::prefs::use_network() && !crate::network::address_schemes().is_empty() {
            parts.network.root.set_visible(true);
        }
        self.show_device_name();
        self.reread();
        // The keyboard stays on the tile it was on, or on the first one when that went.
        if focused.is_some() {
            let first = || parts.computer.tiles.child_at_index(0);
            if let Some(tile) = refocus.or_else(first) {
                focus_tile(&tile);
            }
        }
    }

    fn tile(
        &self,
        place: &Place,
        sidebar: Option<&PlacesSidebar>,
        mounts: &MountTable,
    ) -> (gtk::FlowBoxChild, Option<Usage>) {
        let (icon, name): (gio::Icon, String) = match place {
            Place::System => (
                gio::ThemedIcon::new("drive-harddisk").upcast(),
                glib::os_info("NAME")
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| gettext("System")),
            ),
            Place::Volume(v) => (v.icon(), v.name().into()),
            Place::Mount(m) => (m.icon(), m.name().into()),
            Place::Server(f) => (
                gio::ThemedIcon::new("network-server").upcast(),
                server_name(f),
            ),
        };
        let list = self.layout() == ViewMode::List;
        let image = gtk::Image::builder()
            .gicon(&icon)
            .pixel_size(if list { 32 } else { 48 })
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let title = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        let name_line = gtk::Box::builder().spacing(6).build();
        name_line.append(&title);
        let tag = match place {
            Place::System => Some((gettext("System"), "spiral-computer-tag")),
            Place::Server(f) => {
                crate::network::protocol_of(f).map(|t| (t.to_string(), "spiral-protocol"))
            }
            _ => place
                .filesystem()
                .and_then(|f| crate::network::mount_protocol(&f, mounts))
                .map(|t| (t.to_string(), "spiral-protocol")),
        };
        if let Some((text, class)) = tag {
            name_line.append(
                &gtk::Label::builder()
                    .label(&text)
                    .valign(gtk::Align::Center)
                    .css_classes([class])
                    .build(),
            );
        }
        let bar = gtk::LevelBar::builder().visible(false).build();
        let caption = gtk::Label::builder()
            .xalign(if list { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["caption", "dim-label", "numeric"])
            .build();
        let absent = match place {
            Place::Volume(v) => v.get_mount().is_none(),
            Place::Server(_) => true,
            _ => false,
        };
        match place {
            Place::Volume(_) if absent => caption.set_label(&gettext("Not Mounted")),
            Place::Server(_) => caption.set_label(&gettext("Not Connected")),
            _ => caption.set_visible(false),
        }
        if absent {
            image.add_css_class("dim-label");
            title.add_css_class("dim-label");
        }
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_start(12)
            .margin_end(6)
            .build();
        content.append(&image);
        if list {
            content.set_margin_top(6);
            content.set_margin_bottom(6);
            name_line.set_hexpand(true);
            caption.set_valign(gtk::Align::Center);
            // The bars of all rows line up, whether or not a row has one to show.
            let slot = gtk::Box::builder()
                .width_request(140)
                .hexpand(false)
                .valign(gtk::Align::Center)
                .build();
            bar.set_hexpand(true);
            slot.append(&bar);
            content.append(&name_line);
            content.append(&caption);
            content.append(&slot);
        } else {
            content.set_margin_top(12);
            content.set_margin_bottom(12);
            content.set_width_request(280);
            let text = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            text.append(&name_line);
            text.append(&bar);
            text.append(&caption);
            content.append(&text);
        }
        let busy = self.imp().busy.borrow().contains(&place.id());
        let button = place
            .eject_target(mounts)
            .filter(|_| sidebar.is_some())
            .map(|target| {
                let button = gtk::Button::builder()
                    .icon_name("media-eject-symbolic")
                    .valign(gtk::Align::Center)
                    .visible(!busy)
                    .tooltip_text(if target.is_network(mounts) {
                        gettext("Disconnect")
                    } else {
                        gettext("Eject")
                    })
                    .css_classes(["flat", "circular"])
                    .build();
                button.connect_clicked(glib::clone!(
                    #[weak(rename_to = page)]
                    self,
                    #[strong]
                    place,
                    move |_| page.leave(&place)
                ));
                content.append(&button);
                button
            });
        // As wide as the button, for the tile not to change its shape.
        let spinner = adw::Spinner::builder()
            .width_request(34)
            .valign(gtk::Align::Center)
            .visible(busy)
            .build();
        content.append(&spinner);
        // In the list the bars line up, button or none.
        if list && button.is_none() && !busy {
            content.append(&gtk::Box::builder().width_request(34).build());
        }
        let frame: TileFrame = glib::Object::new();
        frame.set_child(Some(&content));
        let tile = gtk::FlowBoxChild::builder()
            .child(&frame)
            .css_classes(["card", "activatable", "spiral-computer-tile"])
            .build();
        tile.update_property(&[gtk::accessible::Property::Label(&name)]);
        describe(&caption);
        tile.set_tooltip_text(place.whereabouts(mounts).as_deref());
        PLACE.set(&tile, place.clone());
        BUSY.set(&tile, (spinner, button));
        self.add_tile_input(&tile, place);
        let network = place.on_network(mounts);
        let usage = place.filesystem().map(|fs| Usage {
            bar,
            caption,
            local: !network && !place.removable(),
            no_size: network.then(|| user_on_host(&fs)).filter(|c| *c != name),
        });
        (tile, usage)
    }

    /// Middle click, right click, a finger held, and files dropped on the tile.
    fn add_tile_input(&self, tile: &gtk::FlowBoxChild, place: &Place) {
        let middle = gtk::GestureClick::builder()
            .button(gdk::BUTTON_MIDDLE)
            .build();
        middle.connect_released(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            tile,
            move |g, _, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
                page.open(&tile, true);
            }
        ));
        tile.add_controller(middle);
        let right = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        right.connect_pressed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[weak]
            tile,
            move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                page.popup_menu(&tile, x, y);
            }
        ));
        tile.add_controller(right);
        crate::touch::long_press(
            tile,
            |_, _| true,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                #[weak]
                tile,
                move |x, y| page.popup_menu(&tile, x, y)
            ),
        );
        // Files dropped on a disk go to its top, as they do onto its row in the sidebar.
        if matches!(place, Place::Server(_)) {
            return;
        }
        let Some(dest) = place.file() else { return };
        let target = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        target.connect_enter(|t, _, _| crate::browser_view::preferred_action(t));
        target.connect_motion(|t, _, _| crate::browser_view::preferred_action(t));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = page)]
            self,
            #[upgrade_or]
            false,
            move |t, value, x, y| {
                page.imp()
                    .view
                    .upgrade()
                    .is_some_and(|view| view.drop_files(t, value, &dest, x, y))
            }
        ));
        tile.add_controller(target);
    }

    /// Every tile, group by group.
    fn tiles(&self) -> Vec<gtk::FlowBoxChild> {
        let parts = self.parts();
        let mut tiles = Vec::new();
        for group in [&parts.computer, &parts.removable, &parts.network] {
            let mut child = group.tiles.first_child();
            while let Some(tile) = child {
                child = tile.next_sibling();
                tiles.extend(tile.downcast::<gtk::FlowBoxChild>().ok());
            }
        }
        tiles
    }

    /// The tile of the place `id` now: the tiles are made anew on every change.
    fn tile_of(&self, id: &str) -> Option<gtk::FlowBoxChild> {
        self.tiles()
            .into_iter()
            .find(|t| PLACE.get(t).is_some_and(|p| p.id() == id))
    }

    /// Ask each filesystem shown how full it is, but those not done answering before.
    fn reread(&self) {
        for tile in self.tiles() {
            if let (Some(place), Some(usage)) = (PLACE.get(&tile), USAGE.get(&tile))
                && let Some(fs) = place.filesystem()
                && self.imp().asking.borrow_mut().insert(place.id())
            {
                self.read_usage(place.id(), fs, usage.local);
            }
        }
        self.show_total();
    }

    /// Ask `fs` how full it is, for the tile of the place `id` whichever it is by then.
    fn read_usage(&self, id: String, fs: gio::File, local: bool) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let info = fs
                    .query_filesystem_info_future(
                        "filesystem::size,filesystem::free,filesystem::used",
                        glib::Priority::DEFAULT,
                    )
                    .await;
                let reading = match info
                    .ok()
                    .filter(|i| i.attribute_uint64("filesystem::size") > 0)
                {
                    Some(info) => {
                        let size = info.attribute_uint64("filesystem::size");
                        let free = info.attribute_uint64("filesystem::free");
                        let used = if info.has_attribute("filesystem::used") {
                            info.attribute_uint64("filesystem::used")
                        } else {
                            size.saturating_sub(free)
                        };
                        Reading::Size { free, size, used }
                    }
                    None => Reading::Unsized,
                };
                let imp = page.imp();
                let sized = matches!(reading, Reading::Size { .. });
                if let Some(usage) = page.tile_of(&id).and_then(|t| USAGE.get(&t))
                    && (sized || usage.no_size.is_some())
                {
                    usage.show(&reading);
                    imp.readings.borrow_mut().insert(id.clone(), reading);
                }
                if local && sized && !imp.disks.borrow().contains_key(&id) {
                    let disk = disk_of(&fs).await;
                    imp.disks.borrow_mut().insert(id.clone(), disk);
                }
                imp.asking.borrow_mut().remove(&id);
                page.show_total();
            }
        ));
    }

    /// The free space of the disks of this computer in the header, from what they last
    /// said, so it stays put while they are asked again.
    fn show_total(&self) {
        let imp = self.imp();
        let readings = imp.readings.borrow();
        let disks = imp.disks.borrow();
        let mut counted = std::collections::HashSet::new();
        let (mut free, mut size) = (0, 0);
        for tile in self.tiles() {
            if !USAGE.get(&tile).is_some_and(|u| u.local) {
                continue;
            }
            let Some(id) = PLACE.get(&tile).map(|p| p.id()) else {
                continue;
            };
            if let (
                Some(Reading::Size {
                    free: f, size: s, ..
                }),
                Some(Some(disk)),
            ) = (readings.get(&id), disks.get(&id))
                && counted.insert(disk)
            {
                free += f;
                size += s;
            }
        }
        let space = &self.parts().space;
        space.set_visible(size > 0);
        space.set_label(&free_of(free, size));
    }

    /// The device name in the header, once /etc/machine-info is read.
    fn show_device_name(&self) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let pretty = gio::File::for_path("/etc/machine-info")
                    .load_contents_future()
                    .await
                    .ok()
                    .and_then(|(text, _)| pretty_hostname(&String::from_utf8_lossy(&text)));
                let name = pretty.unwrap_or_else(|| glib::host_name().into());
                page.parts().title.set_label(&name);
            }
        ));
    }

    fn focused_tile(&self) -> Option<gtk::FlowBoxChild> {
        self.root()?
            .focus()?
            .ancestor(gtk::FlowBoxChild::static_type())
            .and_downcast::<gtk::FlowBoxChild>()
            .filter(|t| t.is_ancestor(self))
    }

    /// Put the keyboard on the first tile. Whether there was one to put it on.
    pub fn take_focus(&self) -> bool {
        if self.focused_tile().is_some() {
            return true;
        }
        self.parts()
            .computer
            .tiles
            .child_at_index(0)
            .is_some_and(|t| focus_tile(&t))
    }

    fn open(&self, tile: &gtk::FlowBoxChild, new_tab: bool) {
        let Some(place) = PLACE.get(tile) else { return };
        let Some(view) = self.imp().view.upgrade() else {
            return;
        };
        let go = move |file: &gio::File| {
            if new_tab {
                view.emit_by_name::<()>("open-in-new-tab", &[file]);
            } else {
                view.go_to(file);
            }
        };
        match (&place, place.file()) {
            (_, Some(file)) => go(&file),
            (Place::Volume(volume), None) => self.mount_and_open(volume.clone(), go),
            _ => {}
        }
    }

    fn mount_and_open(&self, volume: gio::Volume, go: impl Fn(&gio::File) + 'static) {
        let window = self.root().and_downcast::<gtk::Window>();
        let op = gtk::MountOperation::new(window.as_ref());
        let id = Place::Volume(volume.clone()).id();
        self.set_busy(&id, true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let result = volume
                    .mount_future(gio::MountMountFlags::NONE, Some(&op))
                    .await;
                page.set_busy(&id, false);
                match result {
                    Ok(()) => {
                        if let Some(mount) = volume.get_mount() {
                            go(&mount.default_location());
                        }
                    }
                    // The passphrase dialog was dismissed: nothing to report.
                    Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                    Err(e) => {
                        let dialog = adw::AlertDialog::builder()
                            .heading(gettext("Could Not Mount"))
                            .body(e.message())
                            .build();
                        dialog.add_response("ok", &gettext("_OK"));
                        dialog.present(Some(&page));
                    }
                }
            }
        ));
    }

    fn setup_actions(&self) {
        let group = &self.imp().actions;
        let add = |name: &str, f: fn(&ComputerPage, &gtk::FlowBoxChild, Place)| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_, _| {
                    let tile = page.imp().menu_tile.borrow().clone();
                    if let Some(tile) = tile
                        && let Some(place) = PLACE.get(&tile)
                    {
                        f(&page, &tile, place);
                    }
                }
            ));
            group.add_action(&action);
        };
        add("open", |page, tile, _| page.open(tile, false));
        add("open-new-tab", |page, tile, _| page.open(tile, true));
        add("eject", |page, _, place| page.leave(&place));
        add("disconnect", |page, _, place| page.leave(&place));
        add("forget", |_, _, place| {
            if let Place::Server(file) = place {
                crate::network::forget(&file.uri());
            }
        });
        add("hide", |_, _, place| {
            if let Some(key) = place.hide_key() {
                crate::prefs::set_hidden_on_computer(&key, true);
            }
        });
        add("unhide", |_, _, place| {
            if let Some(key) = place.hide_key() {
                crate::prefs::set_hidden_on_computer(&key, false);
            }
        });
        add("properties", |page, _, place| {
            if let Some(file) = place.file() {
                crate::dialogs::PropertiesDialog::open(vec![file], page, || {});
            }
        });
        let show = gio::SimpleAction::new_stateful("show-hidden", None, &false.to_variant());
        show.connect_change_state(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |action, value| {
                let on = value.and_then(glib::Variant::get::<bool>).unwrap_or(false);
                action.set_state(&on.to_variant());
                page.imp().shows_hidden.set(on);
                page.refresh();
            }
        ));
        group.add_action(&show);
        self.insert_action_group("computer", Some(group));
    }

    /// Eject a disk, or disconnect from a share, as its row in the sidebar does.
    fn leave(&self, place: &Place) {
        let (Some(target), Some(sidebar)) =
            (place.eject_target(&MountTable::read()), self.sidebar())
        else {
            return;
        };
        let id = place.id();
        self.set_busy(&id, true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                sidebar.unmount(target).await;
                page.set_busy(&id, false);
            }
        ));
    }

    /// Show a spinner on the tile of the place `id` in place of its button, while it is
    /// mounted or ejected.
    fn set_busy(&self, id: &str, busy: bool) {
        let imp = self.imp();
        if busy {
            imp.busy.borrow_mut().insert(id.to_string());
        } else {
            imp.busy.borrow_mut().remove(id);
        }
        if let Some((spinner, button)) = self.tile_of(id).and_then(|t| BUSY.get(&t)) {
            spinner.set_visible(busy);
            if let Some(button) = button {
                button.set_visible(!busy);
            }
        }
    }

    fn popup_menu(&self, tile: &gtk::FlowBoxChild, x: f64, y: f64) {
        let Some(place) = PLACE.get(tile) else { return };
        let imp = self.imp();
        imp.menu_tile.replace(Some(tile.clone()));
        let sidebar = self.sidebar();
        let in_window = sidebar.is_some();
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append(Some(&gettext("_Open")), Some("computer.open"));
        if in_window {
            open.append(
                Some(&gettext("Open in New _Tab")),
                Some("computer.open-new-tab"),
            );
        }
        menu.append_section(None, &open);
        let leave = gio::Menu::new();
        let mounts = MountTable::read();
        if in_window && let Some(target) = place.eject_target(&mounts) {
            if target.is_network(&mounts) {
                leave.append(Some(&gettext("_Disconnect")), Some("computer.disconnect"));
            } else {
                leave.append(Some(&gettext("_Eject")), Some("computer.eject"));
            }
        }
        if matches!(place, Place::Server(_)) {
            leave.append(Some(&gettext("_Forget Server")), Some("computer.forget"));
        }
        if leave.n_items() > 0 {
            menu.append_section(None, &leave);
        }
        let visibility = gio::Menu::new();
        if let Some(key) = place.hide_key() {
            if crate::prefs::hidden_on_computer().contains(&key) {
                visibility.append(Some(&gettext("_Show")), Some("computer.unhide"));
            } else {
                visibility.append(Some(&gettext("_Hide")), Some("computer.hide"));
            }
        }
        if imp.has_hidden.get() {
            if let Some(action) = imp
                .actions
                .lookup_action("show-hidden")
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_state(&imp.shows_hidden.get().to_variant());
            }
            visibility.append(
                Some(&gettext("Show Hidden _Places")),
                Some("computer.show-hidden"),
            );
        }
        if visibility.n_items() > 0 {
            menu.append_section(None, &visibility);
        }
        if place.filesystem().is_some_and(|_| place.file().is_some()) {
            let properties = gio::Menu::new();
            properties.append(Some(&gettext("P_roperties")), Some("computer.properties"));
            menu.append_section(None, &properties);
        }
        let existing = imp.popover.borrow().clone();
        let popover = existing.unwrap_or_else(|| {
            let p = gtk::PopoverMenu::from_model(gio::MenuModel::NONE);
            p.set_parent(self);
            p.set_has_arrow(false);
            p.set_halign(gtk::Align::Start);
            imp.popover.replace(Some(p.clone()));
            p
        });
        popover.set_menu_model(Some(&menu));
        let at = tile
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
        crate::touch::popup(&popover);
    }
}

/// Where the page's layout is kept, among the views the lists of files remember.
const LAYOUT_ATTRIBUTE: &str = "metadata::spiral-view";

fn remembered_layout() -> ViewMode {
    match crate::prefs::list_view(URI)
        .get(LAYOUT_ATTRIBUTE)
        .and_then(|nick| ViewMode::from_nick(nick))
    {
        Some(ViewMode::List) => ViewMode::List,
        _ => ViewMode::Grid,
    }
}

/// Tell screen readers what a tile's caption says, its free space or why it is dimmed:
/// they read the tile by its name alone.
fn describe(caption: &gtk::Label) {
    if caption.is_visible()
        && let Some(tile) = caption.ancestor(gtk::FlowBoxChild::static_type())
    {
        tile.update_property(&[gtk::accessible::Property::Description(&caption.label())]);
    }
}

/// Put the keyboard on `tile` the way the arrows do: grabbed directly, it leaves the box
/// without a cursor, and the arrows do nothing.
fn focus_tile(tile: &gtk::FlowBoxChild) -> bool {
    tile.child_focus(gtk::DirectionType::TabForward)
}

/// The distribution's logo, or a computer where the icon themes have none. Not both names
/// for GTK to choose from: it takes the first one the user's theme has before it looks in
/// hicolor, where the logo is.
fn system_logo() -> gio::Icon {
    let logo = glib::os_info("LOGO").filter(|logo| {
        gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(logo))
    });
    gio::ThemedIcon::new(logo.as_deref().unwrap_or("computer")).upcast()
}

/// What tells one disk from another in the total. The subvolumes of a btrfs disk are each
/// a filesystem of their own, sharing the room of the one device they are on.
async fn disk_of(fs: &gio::File) -> Option<String> {
    if fs.is_native()
        && let Some(mount) = fs.path().and_then(|p| MountTable::read().at(&p))
        && mount.source.starts_with("/dev/")
    {
        return Some(mount.source);
    }
    fs.query_info_future(
        "id::filesystem",
        gio::FileQueryInfoFlags::NONE,
        glib::Priority::DEFAULT,
    )
    .await
    .ok()
    .and_then(|i| i.attribute_string("id::filesystem"))
    .map(Into::into)
}

/// The name the computer goes by when it is given one to be shown, as `hostnamectl
/// --pretty` and the desktops' settings set it; the page shows the hostname otherwise.
fn pretty_hostname(machine_info: &str) -> Option<String> {
    machine_info
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_HOSTNAME="))
        .and_then(|v| glib::shell_unquote(v).ok())
        .and_then(|v| v.into_string().ok())
        .filter(|name| !name.trim().is_empty())
}

fn on_host(what: &str, host: &str) -> String {
    // Translators: a share or a user on a server: "media on nas", "pi on 192.168.1.40".
    gettext("%s on %s")
        .replacen("%s", what, 1)
        .replacen("%s", host, 1)
}

/// A server connected to before, named as gvfs names it once it is connected: by the
/// share on it for Windows sharing, by the user on it otherwise, or by the host alone.
fn server_name(file: &gio::File) -> String {
    let Ok(uri) = glib::Uri::parse(&file.uri(), glib::UriFlags::NONE) else {
        return file.parse_name().into();
    };
    let Some(host) = uri.host() else {
        return file.parse_name().into();
    };
    let share = (uri.scheme() == "smb")
        .then(|| {
            uri.path()
                .split('/')
                .find(|s| !s.is_empty())
                .and_then(|s| glib::Uri::unescape_string(s, None::<&str>))
        })
        .flatten();
    match share.or_else(|| uri.user()) {
        Some(what) => on_host(&what, &host),
        None => host.into(),
    }
}

/// The user and the host a share is on, for a share that says nothing of its size.
fn user_on_host(file: &gio::File) -> String {
    let Ok(uri) = glib::Uri::parse(&file.uri(), glib::UriFlags::NONE) else {
        return file.parse_name().into();
    };
    match (uri.user(), uri.host()) {
        (Some(user), Some(host)) => on_host(&user, &host),
        (None, Some(host)) => host.into(),
        _ => file.parse_name().into(),
    }
}

fn free_of(free: u64, size: u64) -> String {
    // Translators: free space of a disk, then its size: "36.0 GB free of 2.0 TB".
    gettext("%s free of %s")
        .replacen("%s", &crate::prefs::size(free), 1)
        .replacen("%s", &crate::prefs::size(size), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_named_by_its_share_or_its_user() {
        let name = |uri| server_name(&gio::File::for_uri(uri));
        assert_eq!(name("smb://nas/media/films"), "media on nas");
        assert_eq!(name("sftp://pi@192.168.1.40/home/pi"), "pi on 192.168.1.40");
        assert_eq!(name("ftp://example.org/"), "example.org");
    }
}
