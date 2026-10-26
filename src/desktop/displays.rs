//! Which display each icon of the desktop is on, with more than one: every display shows the
//! desktop folder, and each file, Home, the trash and each drive is on one of them, its home.
//! An icon whose home is not connected visits the main display until it is back. Kept in a
//! key file of the user's state, as the places of the icons are.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::placement::Point;
use super::positions::{Place, key_of};
use crate::glib;

/// A display as the system names it: its make and model, and the port it is plugged into.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub model: String,
    pub port: String,
}

/// A display seen before, under the key its icons are kept by.
#[derive(Debug, Clone, PartialEq)]
struct Record {
    key: String,
    model: String,
    port: String,
}

/// The displays connected now, in the order of the layout, and which of them shows a desktop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    keys: Vec<String>,
    main: Option<String>,
    every: bool,
}

impl Layout {
    /// Of `connected`, keys and where each sits, the one `chosen` is main while it is
    /// connected, and otherwise the first in the layout: the leftmost, the top one of those.
    /// Only the main display shows a desktop unless `every` is set.
    pub fn new(connected: &[(String, Point)], chosen: Option<&str>, every: bool) -> Self {
        let mut ordered: Vec<&(String, Point)> = connected.iter().collect();
        ordered.sort_by(|(_, a), (_, b)| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
        let keys: Vec<String> = ordered.into_iter().map(|(key, _)| key.clone()).collect();
        let main = chosen
            .filter(|chosen| keys.iter().any(|key| key == chosen))
            .map(str::to_owned)
            .or_else(|| keys.first().cloned());
        Self { keys, main, every }
    }

    pub fn main(&self) -> Option<&str> {
        self.main.as_deref()
    }

    /// The displays that show a desktop, in the order of the layout.
    pub fn shown(&self) -> Vec<&str> {
        self.keys
            .iter()
            .map(String::as_str)
            .filter(|key| self.every || Some(*key) == self.main())
            .collect()
    }

    /// The display an icon whose home is `home` is shown on: its home while that shows a
    /// desktop, the main display otherwise.
    pub fn shown_on<'a>(&'a self, home: Option<&'a str>) -> Option<&'a str> {
        match home {
            Some(home) if self.shown().contains(&home) => Some(home),
            _ => self.main(),
        }
    }
}

/// The displays seen and the home of each icon.
pub struct Displays {
    path: PathBuf,
    records: Vec<Record>,
    homes: HashMap<Place, String>,
}

const HOMES: &str = "homes";
const SPECIAL_HOMES: &str = "homes special";
const DISPLAY: &str = "display ";

impl Displays {
    /// `$XDG_STATE_HOME/spiral/desktop-displays`.
    pub fn default_path() -> PathBuf {
        glib::user_state_dir()
            .join("spiral")
            .join("desktop-displays")
    }

    /// What is kept at `path`; nothing when there is no file there or it cannot be read,
    /// which puts every icon on the main display.
    pub fn open(path: &Path) -> Self {
        let file = glib::KeyFile::new();
        if let Err(e) = file.load_from_file(path, glib::KeyFileFlags::NONE)
            && !e.matches(glib::FileError::Noent)
        {
            glib::g_warning!(
                "spiral",
                "Could not read the displays of the desktop icons from {}: {e}",
                path.display()
            );
        }
        let mut records = Vec::new();
        for group in file.groups() {
            let Some(key) = group.strip_prefix(DISPLAY) else {
                continue;
            };
            let read = |name: &str| file.string(group.as_str(), name).unwrap_or_default();
            records.push(Record {
                key: key.to_owned(),
                model: read("model").into(),
                port: read("port").into(),
            });
        }
        let mut homes = HashMap::new();
        for (group, special) in [(HOMES, false), (SPECIAL_HOMES, true)] {
            let Ok(keys) = file.keys(group) else {
                continue;
            };
            for escaped in keys.iter() {
                let (Some(name), Ok(home)) = (
                    glib::Uri::unescape_string(escaped.as_str(), None::<&str>),
                    file.string(group, escaped.as_str()),
                ) else {
                    continue;
                };
                let place = if special {
                    Place::Special(name.into())
                } else {
                    Place::File(name.into())
                };
                homes.insert(place, home.into());
            }
        }
        Self {
            path: path.to_owned(),
            records,
            homes,
        }
    }

    /// The keys of `connected`, in the same order: a display seen before keeps its key, found
    /// by its model and port, or by its model alone when it moved to another port; one never
    /// seen is given a key of its own. Two of the same model are told apart by their ports.
    pub fn recognise(&mut self, connected: &[Monitor]) -> Vec<String> {
        let mut keys: Vec<Option<String>> = vec![None; connected.len()];
        let mut taken = vec![false; self.records.len()];
        let mut find = |same_port: bool, keys: &mut Vec<Option<String>>| {
            for (at, monitor) in connected.iter().enumerate() {
                if keys[at].is_some() {
                    continue;
                }
                let found = self.records.iter().enumerate().position(|(i, record)| {
                    !taken[i]
                        && record.model == monitor.model
                        && (!same_port || record.port == monitor.port)
                });
                if let Some(i) = found {
                    taken[i] = true;
                    keys[at] = Some(self.records[i].key.clone());
                }
            }
        };
        find(true, &mut keys);
        find(false, &mut keys);
        connected
            .iter()
            .zip(keys)
            .map(|(monitor, key)| match key {
                Some(key) => {
                    if let Some(record) = self.records.iter_mut().find(|r| r.key == key) {
                        record.port = monitor.port.clone();
                    }
                    key
                }
                None => {
                    let key = self.new_key(monitor);
                    self.records.push(Record {
                        key: key.clone(),
                        model: monitor.model.clone(),
                        port: monitor.port.clone(),
                    });
                    key
                }
            })
            .collect()
    }

    fn new_key(&self, monitor: &Monitor) -> String {
        let base = match (monitor.model.is_empty(), monitor.port.is_empty()) {
            (false, false) => format!("{} ({})", monitor.model, monitor.port),
            (false, true) => monitor.model.clone(),
            (true, false) => monitor.port.clone(),
            (true, true) => "display".to_owned(),
        };
        // A group of a key file cannot hold brackets.
        let base = base.replace('[', "(").replace(']', ")");
        let mut key = base.clone();
        let mut n = 2;
        while self.records.iter().any(|r| r.key == key) {
            key = format!("{base} {n}");
            n += 1;
        }
        key
    }

    /// The display `place` belongs on, when it has one.
    pub fn home(&self, place: &Place) -> Option<&str> {
        self.homes.get(place).map(String::as_str)
    }

    /// Makes `key` the home of `place`, where it was moved to; false when it already was.
    pub fn set_home(&mut self, place: &Place, key: &str) -> bool {
        if self.home(place) == Some(key) {
            return false;
        }
        self.homes.insert(place.clone(), key.to_owned());
        true
    }

    /// Gives each of `places` without a home one: the main display, or, while it has no
    /// cell free, the next display of `layout` that has one. `room` is how many cells each
    /// display has free; those taken here are counted off it. Whether any home was given.
    pub fn settle(
        &mut self,
        layout: &Layout,
        places: &[Place],
        room: &mut HashMap<String, usize>,
    ) -> bool {
        let Some(main) = layout.main().map(str::to_owned) else {
            return false;
        };
        // From the main display on, then round to the displays before it.
        let shown = layout.shown();
        let start = shown.iter().position(|key| *key == main).unwrap_or(0);
        let order: Vec<String> = shown[start..]
            .iter()
            .chain(&shown[..start])
            .map(|key| (*key).to_owned())
            .collect();
        let mut changed = false;
        for place in places {
            if self.homes.contains_key(place) {
                continue;
            }
            let home = order
                .iter()
                .find(|key| room.get(*key).copied().unwrap_or(0) > 0)
                .unwrap_or(&main)
                .clone();
            if let Some(free) = room.get_mut(&home) {
                *free = free.saturating_sub(1);
            }
            self.homes.insert(place.clone(), home);
            changed = true;
        }
        changed
    }

    /// A file renamed keeps its home.
    pub fn rename(&mut self, from: &Place, to: Place) {
        if let Some(home) = self.homes.remove(from) {
            self.homes.insert(to, home);
        }
    }

    /// The names of the files that have a home.
    pub fn files(&self) -> Vec<String> {
        self.homes
            .keys()
            .filter_map(|place| match place {
                Place::File(name) => Some(name.clone()),
                Place::Special(_) => None,
            })
            .collect()
    }

    /// Forgets the homes of the files `gone` from the folder; the places beside the files
    /// keep theirs for when they are back.
    pub fn forget(&mut self, gone: &HashSet<String>) {
        self.homes
            .retain(|place, _| !matches!(place, Place::File(name) if gone.contains(name)));
    }

    /// Writes what is kept, replacing the file whole so that a crash leaves the old one.
    pub fn save(&self) -> Result<(), glib::Error> {
        let file = glib::KeyFile::new();
        for record in &self.records {
            let group = format!("{DISPLAY}{}", record.key);
            file.set_string(&group, "model", &record.model);
            file.set_string(&group, "port", &record.port);
        }
        let mut homes: Vec<_> = self.homes.iter().collect();
        homes.sort_by_key(|(place, _)| key_of(place));
        for (place, home) in homes {
            let group = match place {
                Place::File(_) => HOMES,
                Place::Special(_) => SPECIAL_HOMES,
            };
            file.set_string(group, &key_of(place), home);
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                glib::Error::new(glib::FileError::Failed, &format!("{}: {e}", dir.display()))
            })?;
        }
        file.save_to_file(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiral-displays-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("state").join("desktop-displays")
    }

    fn monitor(model: &str, port: &str) -> Monitor {
        Monitor {
            model: model.into(),
            port: port.into(),
        }
    }

    fn file(name: &str) -> Place {
        Place::File(name.into())
    }

    fn at(key: &str, x: f64, y: f64) -> (String, Point) {
        (key.into(), Point::new(x, y))
    }

    #[test]
    fn a_display_is_known_again_by_its_model_and_port() {
        let mut displays = Displays::open(&scratch("known"));
        let first = displays.recognise(&[monitor("Dell U2720Q", "DP-1")]);
        let again = displays.recognise(&[monitor("Dell U2720Q", "DP-1")]);
        assert_eq!(first, again);
        assert_eq!(first, ["Dell U2720Q (DP-1)"]);
    }

    #[test]
    fn a_display_moved_to_another_port_keeps_its_key() {
        let mut displays = Displays::open(&scratch("moved"));
        let first = displays.recognise(&[
            monitor("Laptop panel", "eDP-1"),
            monitor("Dell U2720Q", "DP-1"),
        ]);
        let moved = displays.recognise(&[
            monitor("Laptop panel", "eDP-1"),
            monitor("Dell U2720Q", "HDMI-A-1"),
        ]);
        assert_eq!(first, moved);
    }

    #[test]
    fn two_of_the_same_model_are_told_apart_by_their_ports() {
        let mut displays = Displays::open(&scratch("twins"));
        let both = displays.recognise(&[monitor("LG 27UK", "DP-1"), monitor("LG 27UK", "DP-2")]);
        assert_ne!(both[0], both[1]);
        // Listed the other way round, each is still found by its port.
        let swapped = displays.recognise(&[monitor("LG 27UK", "DP-2"), monitor("LG 27UK", "DP-1")]);
        assert_eq!(swapped, [both[1].clone(), both[0].clone()]);
    }

    #[test]
    fn the_main_display_is_the_chosen_one_or_else_the_first_in_the_layout() {
        let connected = [
            at("right", 1920.0, 0.0),
            at("left", 0.0, 0.0),
            at("below", 0.0, 1080.0),
        ];
        assert_eq!(Layout::new(&connected, None, true).main(), Some("left"));
        assert_eq!(
            Layout::new(&connected, Some("right"), true).main(),
            Some("right")
        );
        // Chosen but not connected, the first in the layout stands in.
        assert_eq!(
            Layout::new(&connected, Some("tv"), true).main(),
            Some("left")
        );
        assert_eq!(Layout::new(&[], None, true).main(), None);
    }

    #[test]
    fn an_icon_whose_display_is_gone_visits_the_main_one() {
        let layout = Layout::new(
            &[at("left", 0.0, 0.0), at("right", 1920.0, 0.0)],
            None,
            true,
        );
        assert_eq!(layout.shown_on(Some("right")), Some("right"));
        assert_eq!(layout.shown_on(Some("unplugged")), Some("left"));
        assert_eq!(layout.shown_on(None), Some("left"));
        // With a desktop on the main display only, every icon is there.
        let alone = Layout::new(
            &[at("left", 0.0, 0.0), at("right", 1920.0, 0.0)],
            None,
            false,
        );
        assert_eq!(alone.shown(), ["left"]);
        assert_eq!(alone.shown_on(Some("right")), Some("left"));
    }

    #[test]
    fn an_icon_visiting_keeps_its_home_unless_it_is_moved() {
        let mut displays = Displays::open(&scratch("visit"));
        displays.set_home(&file("a"), "right");
        displays.set_home(&file("b"), "right");
        let without = Layout::new(&[at("left", 0.0, 0.0)], None, true);
        assert_eq!(without.shown_on(displays.home(&file("a"))), Some("left"));
        // "b" is moved while it visits, so it stays where it was put.
        assert!(displays.set_home(&file("b"), "left"));
        let back = Layout::new(
            &[at("left", 0.0, 0.0), at("right", 1920.0, 0.0)],
            None,
            true,
        );
        assert_eq!(back.shown_on(displays.home(&file("a"))), Some("right"));
        assert_eq!(back.shown_on(displays.home(&file("b"))), Some("left"));
    }

    #[test]
    fn new_icons_go_to_the_main_display_and_past_it_once_it_is_full() {
        let mut displays = Displays::open(&scratch("settle"));
        displays.set_home(&file("old"), "left");
        let layout = Layout::new(
            &[
                at("left", 0.0, 0.0),
                at("middle", 1920.0, 0.0),
                at("right", 3840.0, 0.0),
            ],
            Some("middle"),
            true,
        );
        let mut room = HashMap::from([
            ("left".into(), 2),
            ("middle".into(), 1),
            ("right".into(), 0),
        ]);
        let places = [file("old"), file("a"), file("b")];
        assert!(displays.settle(&layout, &places, &mut room));
        assert_eq!(displays.home(&file("old")), Some("left"));
        assert_eq!(displays.home(&file("a")), Some("middle"));
        // The main one full, the next with room, round from the start of the layout.
        assert_eq!(displays.home(&file("b")), Some("left"));
        // The cells taken stay counted off for the next new file.
        assert_eq!(room["left"], 1);
        displays.settle(&layout, &[file("c")], &mut room);
        assert_eq!(displays.home(&file("c")), Some("left"));
        // Nowhere with room, the main display takes it anyway.
        displays.settle(&layout, &[file("d")], &mut room);
        assert_eq!(displays.home(&file("d")), Some("middle"));
    }

    #[test]
    fn homes_and_displays_are_read_back_as_kept() {
        let path = scratch("saved");
        let mut displays = Displays::open(&path);
        let keys = displays.recognise(&[monitor("Dell U2720Q", "DP-1")]);
        displays.set_home(&file("notes [1].txt"), &keys[0]);
        displays.set_home(&Place::Special("trash".into()), &keys[0]);
        displays.save().expect("saved");
        let mut read = Displays::open(&path);
        assert_eq!(read.home(&file("notes [1].txt")), Some(keys[0].as_str()));
        assert_eq!(
            read.home(&Place::Special("trash".into())),
            Some(keys[0].as_str())
        );
        assert_eq!(read.recognise(&[monitor("Dell U2720Q", "DP-1")]), keys);
    }

    #[test]
    fn a_file_gone_takes_its_home_along_and_a_place_beside_the_files_keeps_its_own() {
        let mut displays = Displays::open(&scratch("forget"));
        displays.set_home(&file("gone"), "left");
        displays.set_home(&file("here"), "left");
        displays.set_home(&Place::Special("drive:x".into()), "right");
        displays.forget(&HashSet::from(["gone".to_owned(), "drive:x".to_owned()]));
        assert_eq!(displays.files(), ["here"]);
        assert_eq!(
            displays.home(&Place::Special("drive:x".into())),
            Some("right")
        );
    }

    #[test]
    fn a_file_renamed_stays_on_its_display() {
        let mut displays = Displays::open(&scratch("rename"));
        displays.set_home(&file("draft.txt"), "right");
        displays.rename(&file("draft.txt"), file("final.txt"));
        assert_eq!(displays.home(&file("draft.txt")), None);
        assert_eq!(displays.home(&file("final.txt")), Some("right"));
    }
}
