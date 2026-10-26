//! Where the icons of the desktop were left, kept in a key file of the user's state: a group
//! for each size of display they were placed on, so that a display of another size keeps its
//! own layout, and one never seen before starts from the nearest one kept, scaled to fit.
//! A display's layout stays the same while panels and docks come and go over it, and as the
//! icons change size: places are kept in cells from the corner the icons start at.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::placement::{Point, rescale};
use crate::glib;

/// What has a place on the desktop: a file of the desktop folder, by its name, or one of the
/// places shown beside the files, by the name the desktop gives it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Place {
    File(String),
    Special(String),
}

const SPECIAL_GROUP: &str = " special";
/// Before the group of a display's layout. Groups without it were kept for the area the
/// panels left free, which changed as they came and went, and in pixels; they are only read
/// until the display has a layout of its own.
const DISPLAY: &str = "display ";

pub struct Positions {
    path: PathBuf,
    file: glib::KeyFile,
}

impl Positions {
    /// `$XDG_STATE_HOME/spiral/desktop-positions`.
    pub fn default_path() -> PathBuf {
        glib::user_state_dir()
            .join("spiral")
            .join("desktop-positions")
    }

    /// The places kept at `path`; none when there is no file there or it cannot be read,
    /// which costs no more than icons put back in order.
    pub fn open(path: &Path) -> Self {
        let file = glib::KeyFile::new();
        if let Err(e) = file.load_from_file(path, glib::KeyFileFlags::NONE)
            && !e.matches(glib::FileError::Noent)
        {
            glib::g_warning!(
                "spiral",
                "Could not read the places of the desktop icons from {}: {e}",
                path.display()
            );
        }
        Self {
            path: path.to_owned(),
            file,
        }
    }

    /// The places kept for `display`, `size` large, or else those kept for its size nearest
    /// it, scaled to fit. The main display, until it has a layout of its own, takes the one
    /// kept when every display shared a layout.
    pub fn kept(
        &self,
        display: &str,
        main: bool,
        size: Point,
        cell: Point,
        margin: f64,
    ) -> HashMap<Place, Point> {
        let groups = self.file.groups();
        let sizes: Vec<Point> = groups
            .iter()
            .filter_map(|group| {
                let (key, size) = group.strip_prefix(DISPLAY)?.rsplit_once(' ')?;
                (key == display).then(|| parse_size(size)).flatten()
            })
            .collect();
        if sizes.contains(&size) {
            return self.read(&display_group(display, size));
        }
        if let Some(&from) = sizes
            .iter()
            .min_by(|a, b| distance(**a, size).total_cmp(&distance(**b, size)))
        {
            return rescale(&self.read(&display_group(display, from)), from, size);
        }
        if main {
            self.kept_shared(size, cell, margin)
        } else {
            HashMap::new()
        }
    }

    /// What was kept for a display `size` large while every display shared one layout.
    /// Places kept in pixels are read on a grid of `cell` within `margin`.
    fn kept_shared(&self, size: Point, cell: Point, margin: f64) -> HashMap<Place, Point> {
        let in_cells = |places: HashMap<Place, Point>| {
            places
                .into_iter()
                .map(|(place, at)| {
                    let cells = Point::new((at.x - margin) / cell.x, (at.y - margin) / cell.y);
                    (place, cells)
                })
                .collect()
        };
        let groups = self.file.groups();
        let sizes = |display: bool| {
            groups.iter().filter_map(move |group| {
                let group = group.as_str();
                match group.strip_prefix(DISPLAY) {
                    Some(size) if display => parse_size(size),
                    None if !display => parse_size(group),
                    _ => None,
                }
            })
        };
        let nearest = |display: bool| {
            sizes(display).min_by(|a, b| distance(*a, size).total_cmp(&distance(*b, size)))
        };
        if sizes(true).any(|kept| kept == size) {
            return self.read(&shared_group(size));
        }
        // Of the areas kept for this display, the one with the least room was the one with
        // all of the panels on it, as it mostly is.
        if let Some(area) = sizes(false)
            .filter(|area| area.x == size.x && area.y <= size.y)
            .min_by(|a, b| a.y.total_cmp(&b.y))
        {
            return in_cells(self.read(&group_name(area)));
        }
        if let Some(from) = nearest(true) {
            return rescale(&self.read(&shared_group(from)), from, size);
        }
        match nearest(false) {
            Some(from) => in_cells(rescale(&self.read(&group_name(from)), from, size)),
            None => HashMap::new(),
        }
    }

    /// Keeps `places`, in cells, as the layout of `display` while it is `size` large, in
    /// place of what was kept for it.
    pub fn keep(&mut self, display: &str, size: Point, places: &HashMap<Place, Point>) {
        let files = display_group(display, size);
        let specials = format!("{files}{SPECIAL_GROUP}");
        for group in [&files, &specials] {
            let _ = self.file.remove_group(group);
        }
        let mut sorted: Vec<_> = places.iter().collect();
        sorted.sort_by_key(|(place, _)| key_of(place));
        for (place, point) in sorted {
            let group = match place {
                Place::File(_) => &files,
                Place::Special(_) => &specials,
            };
            let value = format!("{};{}", number(point.x), number(point.y));
            self.file.set_string(group, &key_of(place), &value);
        }
    }

    /// Writes what is kept, replacing the file whole so that a crash leaves the old one.
    pub fn save(&self) -> Result<(), glib::Error> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                glib::Error::new(glib::FileError::Failed, &format!("{}: {e}", dir.display()))
            })?;
        }
        self.file.save_to_file(&self.path)
    }

    fn read(&self, group: &str) -> HashMap<Place, Point> {
        let mut places = HashMap::new();
        let specials = format!("{group}{SPECIAL_GROUP}");
        for (group, special) in [(group, false), (specials.as_str(), true)] {
            let Ok(keys) = self.file.keys(group) else {
                continue;
            };
            for key in keys.iter() {
                let Some(name) = glib::Uri::unescape_string(key.as_str(), None::<&str>) else {
                    continue;
                };
                let Ok(values) = self.file.double_list(group, key.as_str()) else {
                    continue;
                };
                if let [x, y] = values[..]
                    && x.is_finite()
                    && y.is_finite()
                {
                    let place = if special {
                        Place::Special(name.to_string())
                    } else {
                        Place::File(name.to_string())
                    };
                    places.insert(place, Point::new(x, y));
                }
            }
        }
        places
    }
}

/// A name as a key: escaped, since a file may be called anything a key cannot hold.
pub(super) fn key_of(place: &Place) -> String {
    let (Place::File(name) | Place::Special(name)) = place;
    glib::Uri::escape_string(name, None, true).to_string()
}

fn group_name(size: Point) -> String {
    format!("{}x{}", size.x.round() as i64, size.y.round() as i64)
}

/// To a thousandth of a cell, which is less than a pixel, without the zeros after it.
fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn display_group(display: &str, size: Point) -> String {
    format!("{DISPLAY}{display} {}", group_name(size))
}

fn shared_group(size: Point) -> String {
    format!("{DISPLAY}{}", group_name(size))
}

fn parse_size(group: &str) -> Option<Point> {
    let (w, h) = group.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w > 0 && h > 0).then(|| Point::new(f64::from(w), f64::from(h)))
}

fn distance(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Point = Point::new(100.0, 100.0);
    const MARGIN: f64 = 4.0;
    const MAIN: &str = "Dell U2720Q (DP-1)";

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spiral-positions-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("state").join("desktop-positions")
    }

    fn file(name: &str) -> Place {
        Place::File(name.to_owned())
    }

    #[test]
    fn places_kept_are_read_back_for_the_same_size() {
        let path = scratch("same");
        let size = Point::new(1920.0, 1080.0);
        let mut places = HashMap::new();
        places.insert(file("notes.txt"), Point::new(12.0, 30.0));
        places.insert(Place::Special("trash".into()), Point::new(12.0, 150.0));
        let mut positions = Positions::open(&path);
        positions.keep(MAIN, size, &places);
        positions.save().expect("saved");
        assert_eq!(
            Positions::open(&path).kept(MAIN, true, size, CELL, MARGIN),
            places
        );
    }

    #[test]
    fn any_file_name_is_kept_whatever_it_holds() {
        let path = scratch("names");
        let size = Point::new(800.0, 600.0);
        let mut places = HashMap::new();
        for name in [" a=b ", "[group]", "line\nbreak", "#hash", "ünï cödé"] {
            places.insert(file(name), Point::new(1.0, 2.0));
        }
        let mut positions = Positions::open(&path);
        positions.keep(MAIN, size, &places);
        positions.save().expect("saved");
        assert_eq!(
            Positions::open(&path).kept(MAIN, true, size, CELL, MARGIN),
            places
        );
    }

    #[test]
    fn a_size_never_seen_starts_from_the_nearest_scaled() {
        let path = scratch("nearest");
        let mut positions = Positions::open(&path);
        let mut small = HashMap::new();
        small.insert(file("a"), Point::new(100.0, 100.0));
        positions.keep(MAIN, Point::new(1000.0, 500.0), &small);
        let mut large = HashMap::new();
        large.insert(file("a"), Point::new(7.0, 7.0));
        positions.keep(MAIN, Point::new(4000.0, 2000.0), &large);
        let a = positions.kept(MAIN, true, Point::new(1100.0, 550.0), CELL, MARGIN)[&file("a")];
        assert!(
            (a.x - 110.0).abs() < 1e-9 && (a.y - 110.0).abs() < 1e-9,
            "{a:?}"
        );
    }

    #[test]
    fn a_display_without_a_layout_reads_the_area_kept_with_every_panel_on_it() {
        let path = scratch("areas");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[1280x800]\na=4;4\n\n[1280x692]\na=600;300\n\n[1280x768]\na=4;136\n\n\
             [1920x1080]\na=7;7\n",
        )
        .unwrap();
        let mut positions = Positions::open(&path);
        let display = Point::new(1280.0, 800.0);
        assert_eq!(
            positions.kept(MAIN, true, display, CELL, MARGIN)[&file("a")],
            Point::new(5.96, 2.96)
        );
        let mut moved = HashMap::new();
        moved.insert(file("a"), Point::new(1.0, 2.5));
        positions.keep(MAIN, display, &moved);
        positions.save().expect("saved");
        assert_eq!(
            Positions::open(&path).kept(MAIN, true, display, CELL, MARGIN),
            moved
        );
    }

    #[test]
    fn keeping_a_layout_replaces_the_old_one_of_its_size() {
        let path = scratch("replace");
        let size = Point::new(640.0, 480.0);
        let mut positions = Positions::open(&path);
        let mut first = HashMap::new();
        first.insert(file("gone"), Point::new(1.0, 1.0));
        positions.keep(MAIN, size, &first);
        let mut second = HashMap::new();
        second.insert(file("here"), Point::new(2.0, 2.0));
        positions.keep(MAIN, size, &second);
        assert_eq!(positions.kept(MAIN, true, size, CELL, MARGIN), second);
    }

    #[test]
    fn nothing_kept_and_a_broken_file_both_mean_no_places() {
        let path = scratch("broken");
        assert!(
            Positions::open(&path)
                .kept(MAIN, true, Point::new(10.0, 10.0), CELL, MARGIN)
                .is_empty()
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\xff\xfe not a key file [[[").unwrap();
        assert!(
            Positions::open(&path)
                .kept(MAIN, true, Point::new(10.0, 10.0), CELL, MARGIN)
                .is_empty()
        );
    }

    #[test]
    fn each_display_keeps_a_layout_of_its_own() {
        let path = scratch("displays");
        let size = Point::new(1920.0, 1080.0);
        let mut positions = Positions::open(&path);
        let left = HashMap::from([(file("a"), Point::new(1.0, 1.0))]);
        let right = HashMap::from([(file("b"), Point::new(3.0, 2.0))]);
        positions.keep(MAIN, size, &left);
        positions.keep("LG 27UK (DP-2)", size, &right);
        positions.save().expect("saved");
        let positions = Positions::open(&path);
        assert_eq!(positions.kept(MAIN, true, size, CELL, MARGIN), left);
        assert_eq!(
            positions.kept("LG 27UK (DP-2)", false, size, CELL, MARGIN),
            right
        );
    }

    #[test]
    fn the_layout_kept_before_displays_had_their_own_is_the_main_ones() {
        let path = scratch("shared");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[display 1920x1080]\na=2;3\n").unwrap();
        let size = Point::new(1920.0, 1080.0);
        let mut positions = Positions::open(&path);
        let shared = HashMap::from([(file("a"), Point::new(2.0, 3.0))]);
        assert_eq!(positions.kept(MAIN, true, size, CELL, MARGIN), shared);
        assert!(
            positions
                .kept("LG 27UK (DP-2)", false, size, CELL, MARGIN)
                .is_empty()
        );
        // Once the main display keeps a layout, at any size, it reads that one.
        let own = HashMap::from([(file("a"), Point::new(5.0, 5.0))]);
        positions.keep(MAIN, Point::new(1280.0, 720.0), &own);
        assert_ne!(positions.kept(MAIN, true, size, CELL, MARGIN), shared);
    }
}
