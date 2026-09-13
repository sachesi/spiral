//! What a picture, a recording or a video says about itself: the size and EXIF of a
//! picture; the length, streams and tags of a recording or a video.
//!
//! None of it is read in the file manager. The bundled helper reads it inside the thumbnail
//! sandbox (`spiral-thumbnailer --probe`) and prints one `key<TAB>value` line per fact; what
//! comes back is taken for text someone else wrote, as the file was: only the known keys, the
//! numbers parsed, the words cut short and stripped of control characters. Where the sandbox
//! does not run, there is nothing to show.
//!
//! Where a photo was taken is never read out of it: a panel that shows it is a panel that
//! gives it away with every screen shared.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gettextrs::{gettext, ngettext};

use crate::{file_utils, gio, glib};
use gio::prelude::*;

/// How long the helper has for one file. The discoverer gives up on a stream first.
const LIMIT: Duration = Duration::from_secs(4);
const DISCOVER_TIMEOUT: u64 = 3;
/// Longest text taken from the helper for one fact.
const TEXT_MAX: usize = 200;
const CACHE_ENTRIES: usize = 256;

/// What the helper found in one file. Every fact is optional: files carry what they carry.
#[derive(Debug, Default, PartialEq)]
pub struct Facts {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// When the picture was taken, as the camera wrote it: ISO 8601, with the offset from
    /// UTC where the camera gave one, local time otherwise.
    pub taken: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub aperture: Option<f64>,
    /// In seconds.
    pub exposure: Option<f64>,
    pub iso: Option<u32>,
    /// In millimetres.
    pub focal: Option<f64>,
    /// In milliseconds.
    pub duration: Option<u64>,
    pub video: Option<String>,
    pub fps: Option<f64>,
    pub audio: Option<String>,
    pub channels: Option<u32>,
    /// In hertz.
    pub rate: Option<u32>,
    /// In bits per second.
    pub bitrate: Option<u32>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Which reading a file of `content_type` gets: "image", "media", or none.
pub fn kind_of(content_type: &str) -> Option<&'static str> {
    if content_type.starts_with("image/") {
        Some("image")
    } else if content_type.starts_with("audio/") || content_type.starts_with("video/") {
        Some("media")
    } else {
        None
    }
}

// ---- in the helper --------------------------------------------------------------------------

/// Run inside the sandbox: the facts of the file at `path`, as `key<TAB>value` lines.
pub fn probe(kind: &str, path: &Path) -> String {
    let facts = match kind {
        "image" => probe_image(path),
        "media" => probe_media(path),
        _ => Vec::new(),
    };
    facts
        .into_iter()
        .map(|(key, value)| format!("{key}\t{}\n", one_line(&value)))
        .collect()
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn probe_image(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let exif = std::fs::File::open(path).ok().and_then(|file| {
        exif::Reader::new()
            .read_from_container(&mut std::io::BufReader::new(file))
            .ok()
    });
    let field = |tag| {
        exif.as_ref()
            .and_then(|e| e.get_field(tag, exif::In::PRIMARY))
            .map(|f| &f.value)
    };
    let text = |tag| match field(tag) {
        Some(exif::Value::Ascii(parts)) => parts
            .first()
            .map(|p| String::from_utf8_lossy(p).trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    };
    let number = |tag| {
        match field(tag) {
            Some(exif::Value::Rational(v)) => v.first().map(|r| r.to_f64()),
            Some(exif::Value::SRational(v)) => v.first().map(|r| r.to_f64()),
            Some(v) => v.get_uint(0).map(f64::from),
            None => None,
        }
        .filter(|n| n.is_finite() && *n > 0.0)
    };

    // The size the picture is drawn at: turned the way the camera was held.
    if let Some((_, width, height)) = crate::gtk::gdk_pixbuf::Pixbuf::file_info(path)
        && width > 0
        && height > 0
    {
        let turned = matches!(
            field(exif::Tag::Orientation).and_then(|v| v.get_uint(0)),
            Some(5..=8)
        );
        let (width, height) = if turned {
            (height, width)
        } else {
            (width, height)
        };
        out.push(("width", width.to_string()));
        out.push(("height", height.to_string()));
    }
    let original = field(exif::Tag::DateTimeOriginal).or_else(|| field(exif::Tag::DateTime));
    if let Some(exif::Value::Ascii(parts)) = original
        && let Some(mut date) = parts
            .first()
            .and_then(|p| exif::DateTime::from_ascii(p).ok())
    {
        if let Some(exif::Value::Ascii(offset)) = field(exif::Tag::OffsetTimeOriginal)
            && let Some(offset) = offset.first()
        {
            let _ = date.parse_offset(offset);
        }
        out.push(("taken", iso8601(&date)));
    }
    let make = text(exif::Tag::Make);
    if let Some(model) = text(exif::Tag::Model) {
        out.push(("camera", camera(make.as_deref(), &model)));
    }
    if let Some(lens) = text(exif::Tag::LensModel) {
        out.push(("lens", lens));
    }
    for (key, tag) in [
        ("aperture", exif::Tag::FNumber),
        ("exposure", exif::Tag::ExposureTime),
        ("iso", exif::Tag::PhotographicSensitivity),
        ("focal", exif::Tag::FocalLength),
    ] {
        if let Some(n) = number(tag) {
            out.push((key, n.to_string()));
        }
    }
    out
}

fn iso8601(date: &exif::DateTime) -> String {
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year, date.month, date.day, date.hour, date.minute, date.second
    );
    if let Some(offset) = date.offset {
        let sign = if offset < 0 { '-' } else { '+' };
        let minutes = offset.unsigned_abs();
        out.push_str(&format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60));
    }
    out
}

/// "Canon" and "Canon EOS R6" make "Canon EOS R6", not "Canon Canon EOS R6".
fn camera(make: Option<&str>, model: &str) -> String {
    match make {
        Some(make)
            if !model
                .to_lowercase()
                .starts_with(&make.split_whitespace().next().unwrap_or("").to_lowercase()) =>
        {
            format!("{make} {model}")
        }
        _ => model.to_string(),
    }
}

fn probe_media(path: &Path) -> Vec<(&'static str, String)> {
    use crate::gst;
    use gstreamer_pbutils::prelude::*;

    let mut out = Vec::new();
    if gst::init().is_err() {
        return out;
    }
    let Ok(uri) = glib::filename_to_uri(path, None) else {
        return out;
    };
    let Ok(discoverer) =
        gstreamer_pbutils::Discoverer::new(gst::ClockTime::from_seconds(DISCOVER_TIMEOUT))
    else {
        return out;
    };
    let Ok(info) = discoverer.discover_uri(&uri) else {
        return out;
    };
    if let Some(duration) = info.duration() {
        out.push(("duration", duration.mseconds().to_string()));
    }
    let codec = |caps: Option<gst::Caps>| {
        caps.map(|c| gstreamer_pbutils::pb_utils_get_codec_description(&c).to_string())
    };
    if let Some(video) = info.video_streams().first() {
        if video.width() > 0 && video.height() > 0 {
            out.push(("width", video.width().to_string()));
            out.push(("height", video.height().to_string()));
        }
        if let Some(name) = codec(video.caps()) {
            out.push(("video", name));
        }
        let rate = video.framerate();
        if rate.numer() > 0 && rate.denom() > 0 {
            out.push((
                "fps",
                (f64::from(rate.numer()) / f64::from(rate.denom())).to_string(),
            ));
        }
    }
    if let Some(audio) = info.audio_streams().first() {
        if let Some(name) = codec(audio.caps()) {
            out.push(("audio", name));
        }
        for (key, value) in [
            ("channels", audio.channels()),
            ("rate", audio.sample_rate()),
            ("bitrate", audio.bitrate()),
        ] {
            if value > 0 {
                out.push((key, value.to_string()));
            }
        }
    }
    // The title of a song is a tag of the file, or of its one sound stream where the file has
    // no tags of its own (Ogg, FLAC). The streams of a video are titled after the track
    // ("Audio", "English"), which says nothing of the video.
    let global = info
        .stream_info()
        .and_then(|s| {
            s.downcast::<gstreamer_pbutils::DiscovererContainerInfo>()
                .ok()
        })
        .and_then(|c| c.tags());
    let own = info
        .video_streams()
        .is_empty()
        .then(|| info.audio_streams().first().and_then(|a| a.tags()))
        .flatten();
    let tags: Vec<gst::TagList> = [global, own].into_iter().flatten().collect();
    let tag = |get: fn(&gst::TagList) -> Option<String>| tags.iter().find_map(get);
    if let Some(title) = tag(|t| Some(t.get::<gst::tags::Title>()?.get().to_string())) {
        out.push(("title", title));
    }
    if let Some(artist) = tag(|t| Some(t.get::<gst::tags::Artist>()?.get().to_string())) {
        out.push(("artist", artist));
    }
    if let Some(album) = tag(|t| Some(t.get::<gst::tags::Album>()?.get().to_string())) {
        out.push(("album", album));
    }
    out
}

// ---- in the file manager --------------------------------------------------------------------

/// Read what the file of `info` says about itself, in the sandbox, or `None` where it is not
/// a local picture, recording or video, or the helper cannot be run. Kept per version of the
/// file, so going back to one is not another run.
pub async fn read(info: &gio::FileInfo) -> Option<Rc<Facts>> {
    let file = file_utils::file_of(info);
    let kind = kind_of(&file_utils::content_type_of(info)?)?;
    let path = file.path().filter(|_| file.is_native())?;
    let key = (
        file.uri().to_string(),
        info.modification_date_time()
            .map(|d| d.to_unix())
            .unwrap_or(0),
        file_utils::size_of(info),
    );
    if let Some(facts) = CACHE.with(|c| c.borrow().seen.get(&key).cloned()) {
        return Some(facts);
    }
    let out = gio::spawn_blocking(move || run_probe(kind, &path))
        .await
        .ok()
        .flatten()?;
    let facts = Rc::new(parse(&String::from_utf8_lossy(&out)));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.seen.insert(key.clone(), facts.clone()).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > CACHE_ENTRIES {
            let Some(oldest) = c.order.pop_front() else {
                break;
            };
            c.seen.remove(&oldest);
        }
    });
    Some(facts)
}

type Key = (String, i64, u64);

#[derive(Default)]
struct Cache {
    seen: HashMap<Key, Rc<Facts>>,
    order: VecDeque<Key>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// Runs on a worker thread.
fn run_probe(kind: &str, path: &Path) -> Option<Vec<u8>> {
    let helper = crate::thumbnails::own_thumbnailer()?;
    let helper_arg = helper.to_string_lossy().into_owned();
    let sandbox = crate::thumbnails::sandbox_base(&helper_arg)?;
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let inside = PathBuf::from(format!("/tmp/in{ext}"));
    let mut cmd = std::process::Command::new(&sandbox.argv[0]);
    cmd.args(&sandbox.argv[1..]);
    if kind == "media" {
        media_setup(&mut cmd);
    }
    cmd.arg("--ro-bind").arg(path).arg(&inside);
    cmd.arg("--")
        .arg(&helper)
        .arg("--probe")
        .arg(kind)
        .arg(&inside);
    // The seccomp memfd must stay open until the child has started.
    let run = crate::thumbnails::run_bounded(&mut cmd, LIMIT);
    drop(sandbox.seccomp);
    match run {
        Ok(ran) if ran.ok => Some(ran.stdout),
        Ok(ran) => {
            glib::g_debug!(
                "spiral",
                "probe of {} failed: {}",
                path.display(),
                ran.trouble
            );
            None
        }
        Err(e) => {
            glib::g_debug!("spiral", "probe of {} could not start: {e}", path.display());
            None
        }
    }
}

/// GStreamer in the sandbox reads the list of its plugins this process keeps, rather than
/// load every plugin on the system to write one of its own for each file. Starting GStreamer
/// here brings that list up to date first; the plugins are listed by a scanner process of
/// GStreamer's own, not loaded here.
fn media_setup(cmd: &mut std::process::Command) {
    static READY: std::sync::Once = std::sync::Once::new();
    READY.call_once(|| {
        let _ = crate::gst::init();
    });
    let registry = std::env::var_os("GST_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            glib::user_cache_dir()
                .join("gstreamer-1.0")
                .join(format!("registry.{}.bin", std::env::consts::ARCH))
        });
    if registry.exists() {
        cmd.arg("--ro-bind").arg(&registry).arg(&registry);
        cmd.arg("--setenv").arg("GST_REGISTRY").arg(&registry);
        cmd.args(["--setenv", "GST_REGISTRY_UPDATE", "no"]);
    }
}

/// The helper's answer, keeping what is known and well formed.
fn parse(out: &str) -> Facts {
    let mut facts = Facts::default();
    for line in out.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let text = || {
            let t: String = value
                .chars()
                .filter(|c| !c.is_control())
                .take(TEXT_MAX)
                .collect();
            Some(t.trim().to_string()).filter(|t| !t.is_empty())
        };
        let int = || value.parse::<u32>().ok().filter(|&n| n > 0);
        let real = || {
            value
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n > 0.0)
        };
        match key {
            "width" => facts.width = int(),
            "height" => facts.height = int(),
            "taken" => facts.taken = text(),
            "camera" => facts.camera = text(),
            "lens" => facts.lens = text(),
            "aperture" => facts.aperture = real(),
            "exposure" => facts.exposure = real(),
            "iso" => facts.iso = int(),
            "focal" => facts.focal = real(),
            "duration" => facts.duration = value.parse::<u64>().ok().filter(|&n| n > 0),
            "video" => facts.video = text(),
            "fps" => facts.fps = real(),
            "audio" => facts.audio = text(),
            "channels" => facts.channels = int(),
            "rate" => facts.rate = int(),
            "bitrate" => facts.bitrate = int(),
            "title" => facts.title = text(),
            "artist" => facts.artist = text(),
            "album" => facts.album = text(),
            _ => {}
        }
    }
    facts
}

// ---- words for the panel ------------------------------------------------------------------

impl Facts {
    /// The facts worth a row, in the order they are shown: title and value.
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut add = |title: String, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                rows.push((title, value));
            }
        };
        add(gettext("Title"), self.title.clone());
        add(gettext("Artist"), self.artist.clone());
        add(gettext("Album"), self.album.clone());
        add(
            gettext("Dimensions"),
            self.width
                .zip(self.height)
                .map(|(w, h)| format!("{w} × {h}")),
        );
        add(gettext("Duration"), self.duration.map(duration_text));
        add(gettext("Taken"), self.taken.as_deref().and_then(taken_text));
        add(gettext("Camera"), self.camera.clone());
        add(gettext("Lens"), self.lens.clone());
        add(gettext("Exposure"), self.exposure_text());
        add(gettext("Video"), self.video_text());
        add(gettext("Audio"), self.audio_text());
        rows
    }

    fn exposure_text(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.aperture.map(|f| format!("f/{}", trimmed(f, 1))),
            self.exposure
                .map(|t| gettext("%s s").replace("%s", &shutter(t))),
            self.iso.map(|n| format!("ISO {n}")),
            self.focal
                .map(|mm| gettext("%s mm").replace("%s", &trimmed(mm, 1))),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn video_text(&self) -> Option<String> {
        let fps = self
            .fps
            .map(|f| gettext("%s fps").replace("%s", &trimmed(f, 2)));
        join([self.video.clone(), fps])
    }

    fn audio_text(&self) -> Option<String> {
        let channels = self.channels.map(|n| match n {
            1 => gettext("Mono"),
            2 => gettext("Stereo"),
            n => ngettext("%d channel", "%d channels", n).replace("%d", &n.to_string()),
        });
        let rate = self
            .rate
            .map(|hz| gettext("%s kHz").replace("%s", &trimmed(f64::from(hz) / 1000.0, 1)));
        let bitrate = self
            .bitrate
            .map(|b| gettext("%s kbit/s").replace("%s", &(b / 1000).to_string()));
        join([self.audio.clone(), channels, rate, bitrate])
    }
}

fn join<const N: usize>(parts: [Option<String>; N]) -> Option<String> {
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `n` with at most `places` decimals and none that are zero: 2.8, 35, 29.97.
fn trimmed(n: f64, places: usize) -> String {
    let s = format!("{n:.places$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A shutter speed as photographers write it: 1/250 below a second, 2.5 above.
fn shutter(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("1/{}", (1.0 / seconds).round())
    } else {
        trimmed(seconds, 1)
    }
}

/// 3:07, or 1:02:03 past the hour.
fn duration_text(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The day and the time on the camera's clock, whatever the date preference: "2 years ago"
/// says nothing of a photo, and the hour it was taken is the hour where it was taken.
fn taken_text(iso: &str) -> Option<String> {
    let date = glib::DateTime::from_iso8601(iso, Some(&glib::TimeZone::local())).ok()?;
    Some(date.format("%x, %H:%M").ok()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_keys_and_clean_text_are_taken() {
        let facts = parse(
            "camera\tNikon\u{1b}[31m Z6\nwidth\t6048\nheight\t-4\ngps\t48.8\niso\tlots\n\
             exposure\t0.004\nno tab here\ntitle\t\n",
        );
        assert_eq!(facts.camera.as_deref(), Some("Nikon[31m Z6"));
        assert_eq!(facts.width, Some(6048));
        assert_eq!(facts.height, None);
        assert_eq!(facts.iso, None);
        assert_eq!(facts.exposure, Some(0.004));
        assert_eq!(facts.title, None);
    }

    #[test]
    fn long_text_is_cut_short() {
        let facts = parse(&format!("lens\t{}", "x".repeat(10_000)));
        assert_eq!(facts.lens.map(|l| l.len()), Some(TEXT_MAX));
    }

    #[test]
    fn numbers_read_as_photographers_write_them() {
        assert_eq!(shutter(0.004), "1/250");
        assert_eq!(shutter(2.5), "2.5");
        assert_eq!(trimmed(2.8, 1), "2.8");
        assert_eq!(trimmed(35.0, 1), "35");
        assert_eq!(trimmed(29.97, 2), "29.97");
        assert_eq!(duration_text(187_400), "3:07");
        assert_eq!(duration_text(3_723_000), "1:02:03");
        // Tokyo's clock, not the viewer's.
        assert!(
            taken_text("2024-05-17T18:42:07+09:00")
                .unwrap()
                .ends_with("18:42")
        );
    }

    #[test]
    fn the_make_is_not_said_twice() {
        assert_eq!(camera(Some("Canon"), "Canon EOS R6"), "Canon EOS R6");
        assert_eq!(camera(Some("NIKON CORPORATION"), "NIKON Z 6"), "NIKON Z 6");
        assert_eq!(camera(Some("FUJIFILM"), "X-T4"), "FUJIFILM X-T4");
        assert_eq!(camera(None, "X-T4"), "X-T4");
    }

    /// A JPEG whose EXIF has a camera, an exposure, a date and a position.
    fn jpeg_with_exif() -> Vec<u8> {
        fn entry(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
            let mut e = tag.to_be_bytes().to_vec();
            e.extend(kind.to_be_bytes());
            e.extend(count.to_be_bytes());
            e.extend(value);
            e
        }
        let at = |n: u32| n.to_be_bytes();
        // Offsets from the start of the TIFF header. IFD0 at 8 with 4 entries (54 bytes),
        // the Exif IFD at 62 with 3 entries (42 bytes), the GPS IFD at 104 with 1 entry
        // (18 bytes), then the values: model at 122, date at 142, exposure at 162.
        let mut tiff = b"MM\0*".to_vec();
        tiff.extend(at(8));
        tiff.extend(4u16.to_be_bytes());
        tiff.extend(entry(0x010f, 2, 4, *b"ACME"));
        tiff.extend(entry(0x0110, 2, 20, at(122)));
        tiff.extend(entry(0x8769, 4, 1, at(62)));
        tiff.extend(entry(0x8825, 4, 1, at(104)));
        tiff.extend(at(0));
        tiff.extend(3u16.to_be_bytes());
        tiff.extend(entry(0x829a, 5, 1, at(162)));
        tiff.extend(entry(0x8827, 3, 1, [0, 200, 0, 0]));
        tiff.extend(entry(0x9003, 2, 20, at(142)));
        tiff.extend(at(0));
        tiff.extend(1u16.to_be_bytes());
        tiff.extend(entry(0x0001, 2, 2, *b"N\0\0\0"));
        tiff.extend(at(0));
        assert_eq!(tiff.len(), 122);
        tiff.extend(b"ACME Shooter 3000\0\0\0");
        tiff.extend(b"2024:05:17 18:42:07\0");
        tiff.extend(at(1));
        tiff.extend(at(250));
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(app1);
        jpeg.extend([0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn a_photo_says_its_camera_and_exposure_but_not_where_it_was() {
        let path = std::env::temp_dir().join(format!("spiral-exif-{}.jpg", std::process::id()));
        std::fs::write(&path, jpeg_with_exif()).unwrap();
        let out = probe("image", &path);
        let _ = std::fs::remove_file(&path);
        let facts = parse(&out);
        assert_eq!(facts.camera.as_deref(), Some("ACME Shooter 3000"));
        assert_eq!(facts.taken.as_deref(), Some("2024-05-17T18:42:07"));
        assert_eq!(facts.exposure, Some(0.004));
        assert_eq!(facts.iso, Some(200));
        assert!(!out.to_lowercase().contains("gps"), "{out}");
        assert!(out.lines().all(|l| l.split('\t').count() == 2), "{out}");
    }
}
