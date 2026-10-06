//! Where the icons of the desktop were left, kept in a key file of the user's state: a group
//! for each size of display they were placed on, so that a display of another size keeps its
//! own layout, and one never seen before starts from the nearest one kept, scaled to fit.
//! A display's layout stays the same while panels and docks come and go over it.

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
/// panels left free, which changed as they came and went; they are only read until the
/// display has a layout of its own.
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

    /// The places kept for a display `size` large, or else those kept for the size nearest
    /// it, scaled to fit.
    pub fn kept(&self, size: Point) -> HashMap<Place, Point> {
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
            return self.read(&display_group(size));
        }
        // Of the areas kept for this display, the one with the least room was the one with
        // all of the panels on it, as it mostly is.
        if let Some(area) = sizes(false)
            .filter(|area| area.x == size.x && area.y <= size.y)
            .min_by(|a, b| a.y.total_cmp(&b.y))
        {
            return self.read(&group_name(area));
        }
        if let Some(from) = nearest(true) {
            return rescale(&self.read(&display_group(from)), from, size);
        }
        match nearest(false) {
            Some(from) => rescale(&self.read(&group_name(from)), from, size),
            None => HashMap::new(),
        }
    }

    /// Keeps `places` as the layout of a display `size` large, in place of what was kept for it.
    pub fn keep(&mut self, size: Point, places: &HashMap<Place, Point>) {
        let files = display_group(size);
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
            let value = format!("{};{}", point.x.round(), point.y.round());
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
fn key_of(place: &Place) -> String {
    let (Place::File(name) | Place::Special(name)) = place;
    glib::Uri::escape_string(name, None, true).to_string()
}

fn group_name(size: Point) -> String {
    format!("{}x{}", size.x.round() as i64, size.y.round() as i64)
}

fn display_group(size: Point) -> String {
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
        positions.keep(size, &places);
        positions.save().expect("saved");
        assert_eq!(Positions::open(&path).kept(size), places);
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
        positions.keep(size, &places);
        positions.save().expect("saved");
        assert_eq!(Positions::open(&path).kept(size), places);
    }

    #[test]
    fn a_size_never_seen_starts_from_the_nearest_scaled() {
        let path = scratch("nearest");
        let mut positions = Positions::open(&path);
        let mut small = HashMap::new();
        small.insert(file("a"), Point::new(100.0, 100.0));
        positions.keep(Point::new(1000.0, 500.0), &small);
        let mut large = HashMap::new();
        large.insert(file("a"), Point::new(7.0, 7.0));
        positions.keep(Point::new(4000.0, 2000.0), &large);
        let a = positions.kept(Point::new(1100.0, 550.0))[&file("a")];
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
            positions.kept(display)[&file("a")],
            Point::new(600.0, 300.0)
        );
        let mut moved = HashMap::new();
        moved.insert(file("a"), Point::new(12.0, 12.0));
        positions.keep(display, &moved);
        positions.save().expect("saved");
        assert_eq!(Positions::open(&path).kept(display), moved);
    }

    #[test]
    fn keeping_a_layout_replaces_the_old_one_of_its_size() {
        let path = scratch("replace");
        let size = Point::new(640.0, 480.0);
        let mut positions = Positions::open(&path);
        let mut first = HashMap::new();
        first.insert(file("gone"), Point::new(1.0, 1.0));
        positions.keep(size, &first);
        let mut second = HashMap::new();
        second.insert(file("here"), Point::new(2.0, 2.0));
        positions.keep(size, &second);
        assert_eq!(positions.kept(size), second);
    }

    #[test]
    fn nothing_kept_and_a_broken_file_both_mean_no_places() {
        let path = scratch("broken");
        assert!(
            Positions::open(&path)
                .kept(Point::new(10.0, 10.0))
                .is_empty()
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"\xff\xfe not a key file [[[").unwrap();
        assert!(
            Positions::open(&path)
                .kept(Point::new(10.0, 10.0))
                .is_empty()
        );
    }
}
