//! The displays connected, shared by the desktops on them: which icons each shows, and the
//! places kept for them all.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::displays::{Displays, Layout, Monitor};
use super::placement::Point;
use super::positions::{Place, Positions};
use crate::gtk::prelude::*;
use crate::gtk::subclass::prelude::*;
use crate::object_data::Key;
use crate::{gdk, gio, glib};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct DisplayMap {
        pub(super) displays: RefCell<Option<Displays>>,
        pub(super) layout: RefCell<Layout>,
        pub(super) connected: RefCell<Vec<(String, gdk::Monitor)>>,
        /// How many cells each display had free when it was laid out last.
        pub(super) room: RefCell<HashMap<String, usize>>,
        pub(super) positions: RefCell<Option<Positions>>,
        pub(super) folder: RefCell<Option<gio::File>>,
        /// What a drag from a desktop carries, files and places, for the desktop of another
        /// display it may be dropped on.
        pub(super) carried: RefCell<(glib::WeakRef<gdk::Drag>, Vec<Place>)>,
        pub(super) changed_queued: Cell<bool>,
        pub(super) save_queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DisplayMap {
        const NAME: &'static str = "SpiralDisplayMap";
        type Type = super::DisplayMap;
    }

    impl ObjectImpl for DisplayMap {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    pub struct DisplayMap(ObjectSubclass<imp::DisplayMap>);
}

/// Marks the displays whose changes are followed already.
static WATCHED: Key<()> = Key::new("display-watched");

thread_local! {
    static MAP: DisplayMap = DisplayMap::new();
}

impl DisplayMap {
    /// The displays of the default display, as every desktop of the process sees them.
    pub fn get() -> Self {
        MAP.with(Clone::clone)
    }

    fn new() -> Self {
        let map: Self = glib::Object::new();
        let imp = map.imp();
        imp.displays
            .replace(Some(Displays::open(&Displays::default_path())));
        imp.positions
            .replace(Some(Positions::open(&Positions::default_path())));
        let settings = crate::prefs::settings();
        for key in ["desktop-main-display", "desktop-every-display"] {
            settings.connect_changed(
                Some(key),
                glib::clone!(
                    #[weak]
                    map,
                    move |_, _| map.update()
                ),
            );
        }
        if let Some(display) = gdk::Display::default() {
            display.monitors().connect_items_changed(glib::clone!(
                #[weak]
                map,
                move |_, _, _, _| map.update()
            ));
        }
        map.update();
        map
    }

    /// Calls `f` once what the displays show changes: which are connected and shown, which
    /// is main, or the display an icon is on.
    pub fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "changed",
            false,
            glib::closure_local!(move |map: &Self| f(map)),
        )
    }

    /// Recognises the displays connected and lays them out again.
    fn update(&self) {
        let imp = self.imp();
        let monitors: Vec<gdk::Monitor> = gdk::Display::default()
            .map(|display| {
                let list = display.monitors();
                (0..list.n_items())
                    .filter_map(|at| list.item(at).and_downcast::<gdk::Monitor>())
                    .collect()
            })
            .unwrap_or_default();
        for monitor in &monitors {
            if !WATCHED.has(monitor) {
                WATCHED.set(monitor, ());
                monitor.connect_notify_local(
                    None,
                    glib::clone!(
                        #[weak(rename_to = map)]
                        self,
                        move |_, _| map.update()
                    ),
                );
            }
        }
        // A display is listed before the system has named it; it is known once it has.
        let monitors: Vec<gdk::Monitor> = monitors
            .into_iter()
            .filter(|m| m.connector().is_some())
            .collect();
        let named: Vec<Monitor> = monitors.iter().map(monitor_of).collect();
        let keys = imp
            .displays
            .borrow_mut()
            .as_mut()
            .map(|d| d.recognise(&named))
            .unwrap_or_default();
        let at: Vec<(String, Point)> = keys
            .iter()
            .zip(&monitors)
            .map(|(key, monitor)| {
                let geometry = monitor.geometry();
                let origin = Point::new(f64::from(geometry.x()), f64::from(geometry.y()));
                (key.clone(), origin)
            })
            .collect();
        let settings = crate::prefs::settings();
        let chosen = settings.string("desktop-main-display");
        let layout = Layout::new(
            &at,
            Some(chosen.as_str()).filter(|c| !c.is_empty()),
            settings.boolean("desktop-every-display"),
        );
        imp.connected
            .replace(keys.into_iter().zip(monitors).collect());
        imp.layout.replace(layout);
        self.queue_save();
        self.queue_changed();
    }

    /// The displays that show a desktop.
    pub fn shown(&self) -> Vec<gdk::Monitor> {
        let connected = self.imp().connected.borrow();
        self.imp()
            .layout
            .borrow()
            .shown()
            .into_iter()
            .filter_map(|key| connected.iter().find(|(k, _)| k == key))
            .map(|(_, monitor)| monitor.clone())
            .collect()
    }

    /// The connected displays, by their keys and the names to show for them. Two displays
    /// of the same model are told apart by their ports.
    pub fn names(&self) -> Vec<(String, String)> {
        let connected = self.imp().connected.borrow();
        let described = |monitor: &gdk::Monitor| monitor.description().map(String::from);
        connected
            .iter()
            .map(|(key, monitor)| {
                let Some(name) = described(monitor) else {
                    return (key.clone(), key.clone());
                };
                let twins = connected
                    .iter()
                    .filter(|(_, m)| described(m).as_ref() == Some(&name))
                    .count();
                match monitor.connector() {
                    Some(port) if twins > 1 => (key.clone(), format!("{name} ({port})")),
                    _ => (key.clone(), name),
                }
            })
            .collect()
    }

    pub fn key(&self, monitor: &gdk::Monitor) -> Option<String> {
        self.imp()
            .connected
            .borrow()
            .iter()
            .find(|(_, m)| m == monitor)
            .map(|(key, _)| key.clone())
    }

    /// Whether a desktop is shown on more than one display, so icons can go from one to
    /// another.
    pub fn crossing(&self) -> bool {
        self.imp().layout.borrow().shown().len() > 1
    }

    /// `drag` carries `places` from a desktop.
    pub fn set_carried(&self, drag: &gdk::Drag, places: Vec<Place>) {
        let carried = glib::WeakRef::new();
        carried.set(Some(drag));
        self.imp().carried.replace((carried, places));
    }

    /// What `drag` carries from a desktop; nothing for a drag from anywhere else. A drop
    /// that does not tell its drag is taken for the desktop's while one is on.
    pub fn carried(&self, drag: Option<&gdk::Drag>) -> Vec<Place> {
        let carried = self.imp().carried.borrow();
        match (drag, carried.0.upgrade()) {
            (Some(drag), Some(from)) if *drag != from => Vec::new(),
            (_, Some(_)) => carried.1.clone(),
            (_, None) => Vec::new(),
        }
    }

    /// The drag from a desktop is over.
    pub fn end_carried(&self) {
        self.imp().carried.replace(Default::default());
    }

    pub fn main(&self) -> Option<String> {
        self.imp().layout.borrow().main().map(str::to_owned)
    }

    /// Whether `place` is shown on the display `key`: its own, or the main display while
    /// its own is away.
    pub fn shows(&self, key: &str, place: &Place) -> bool {
        let displays = self.imp().displays.borrow();
        let home = displays.as_ref().and_then(|d| d.home(place));
        self.imp().layout.borrow().shown_on(home) == Some(key)
    }

    pub fn has_home(&self, place: &Place) -> bool {
        let displays = self.imp().displays.borrow();
        displays.as_ref().is_some_and(|d| d.home(place).is_some())
    }

    /// Notes that the display `key` has `free` cells left.
    pub fn set_room(&self, key: &str, free: usize) {
        self.imp().room.borrow_mut().insert(key.to_owned(), free);
    }

    /// Gives each of `places`, new on the main display, a home: the main display while it
    /// has `free` cells, then the next display with room.
    pub fn settle(&self, places: &[Place], free: usize) {
        let imp = self.imp();
        let mut room = imp.room.borrow_mut();
        if let Some(main) = self.main() {
            room.insert(main, free);
        }
        let layout = imp.layout.borrow().clone();
        let settled = imp
            .displays
            .borrow_mut()
            .as_mut()
            .is_some_and(|d| d.settle(&layout, places, &mut room));
        drop(room);
        if settled {
            self.queue_save();
            self.queue_changed();
        }
    }

    /// `places` were moved to the display `key`, and stay there.
    pub fn move_to(&self, places: &[Place], key: &str) {
        let mut moved = false;
        if let Some(displays) = self.imp().displays.borrow_mut().as_mut() {
            for place in places {
                moved |= displays.set_home(place, key);
            }
        }
        if moved {
            self.queue_save();
            self.queue_changed();
        }
    }

    pub fn rename(&self, from: &Place, to: Place) {
        if let Some(displays) = self.imp().displays.borrow_mut().as_mut() {
            displays.rename(from, to);
        }
        self.queue_save();
    }

    /// The desktop folder, whose files gone are forgotten.
    pub fn set_folder(&self, folder: &gio::File) {
        self.imp().folder.replace(Some(folder.clone()));
    }

    /// The places kept for the icons of every display.
    pub fn positions(&self) -> std::cell::RefMut<'_, Option<Positions>> {
        self.imp().positions.borrow_mut()
    }

    fn queue_changed(&self) {
        if self.imp().changed_queued.replace(true) {
            return;
        }
        // From the main loop: a desktop settles homes while it lays itself out, and its
        // icons cannot change under it then.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move || {
                map.imp().changed_queued.set(false);
                map.emit_by_name::<()>("changed", &[]);
            }
        ));
    }

    /// Keeps the displays and the homes once the main loop comes round, without the files
    /// gone from the folder. While the folder is not there, nothing is kept.
    fn queue_save(&self) {
        if self.imp().save_queued.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move || {
                glib::spawn_future_local(async move {
                    let imp = map.imp();
                    imp.save_queued.set(false);
                    let folder = imp.folder.borrow().clone();
                    let names = imp
                        .displays
                        .borrow()
                        .as_ref()
                        .map(Displays::files)
                        .unwrap_or_default();
                    let gone = gio::spawn_blocking(move || {
                        let folder = folder?;
                        if !folder.query_exists(gio::Cancellable::NONE) {
                            return None;
                        }
                        let gone: HashSet<String> = names
                            .into_iter()
                            .filter(|n| !folder.child(n).query_exists(gio::Cancellable::NONE))
                            .collect();
                        Some(gone)
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(gone) = gone else { return };
                    if let Some(displays) = imp.displays.borrow_mut().as_mut() {
                        displays.forget(&gone);
                        if let Err(e) = displays.save() {
                            glib::g_warning!(
                                "spiral",
                                "Could not keep the displays of the desktop icons: {e}"
                            );
                        }
                    }
                });
            }
        ));
    }
}

/// A display as the rules know it: its make and model, and its port.
fn monitor_of(monitor: &gdk::Monitor) -> Monitor {
    let model = [monitor.manufacturer(), monitor.model()]
        .into_iter()
        .flatten()
        .map(String::from)
        .collect::<Vec<_>>()
        .join(" ");
    Monitor {
        model,
        port: monitor.connector().map(String::from).unwrap_or_default(),
    }
}
