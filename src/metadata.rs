//! What a file says about itself: the size and EXIF of a picture; the length, streams and
//! tags of a recording or a video; the pages, title and author of a PDF, an office document
//! or an e-book.
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

mod display;
mod document;
mod image;
mod media;

use document::*;
pub(crate) use image::*;
use media::*;

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
    /// Bits per sample, for lossless sound.
    pub depth: Option<u32>,
    /// How many sound streams, where there is more than one.
    pub audio_tracks: Option<u32>,
    /// The languages of the subtitles, or how many there are where they do not say.
    pub subtitles: Option<String>,
    pub subtitle_tracks: Option<u32>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub pages: Option<u32>,
    pub words: Option<u32>,
    pub slides: Option<u32>,
    /// In points.
    pub page_width: Option<f64>,
    pub page_height: Option<f64>,
    /// The name poppler gives the page size: A4, letter.
    pub paper: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub track: Option<u32>,
    pub tracks: Option<u32>,
    pub year: Option<u32>,
}

/// Which reading a file of `content_type` gets: "raw" for a camera's raw file, "image",
/// "media", "pdf", "document" for the zipped formats of office suites and e-books, or none.
pub fn kind_of(content_type: &str) -> Option<&'static str> {
    if content_type == "application/pdf" {
        Some("pdf")
    } else if content_type.starts_with("application/vnd.oasis.opendocument.")
        || content_type.starts_with("application/vnd.openxmlformats-officedocument.")
        || content_type == "application/epub+zip"
    {
        Some("document")
    } else if gio::content_type_is_a(content_type, "image/x-dcraw") {
        Some("raw")
    } else if content_type.starts_with("image/") {
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
        "image" => probe_image(path, false),
        "raw" => probe_image(path, true),
        "media" => probe_media(path),
        "pdf" => probe_pdf(path),
        "document" => probe_document(path),
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

// ---- in the file manager --------------------------------------------------------------------

/// Read what the file of `info` says about itself, in the sandbox, or `None` where it is not
/// a local file of a kind the helper reads, or the helper cannot be run; an item in the trash
/// is the file on the disk it is. Kept per version of the file, so going back to one is not
/// another run.
pub async fn read(info: &gio::FileInfo) -> Option<Rc<Facts>> {
    let file = crate::thumbnails::on_disk(info).await;
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
    let mut facts = parse(&String::from_utf8_lossy(&out));
    // A stream that does not say its bitrate (FLAC, Opus in Ogg) is given the average: the
    // size of the file over its length. With video in it the sound's share is not known.
    if facts.bitrate.is_none()
        && facts.video.is_none()
        && let Some(ms) = facts.duration
    {
        facts.bitrate = u32::try_from(file_utils::size_of(info) * 8 * 1000 / ms)
            .ok()
            .filter(|&b| b > 0);
    }
    let facts = Rc::new(facts);
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
    let sandbox = crate::sandbox::command(&helper_arg)?;
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
    let run = crate::sandbox::run_bounded(&mut cmd, LIMIT);
    drop(sandbox.seccomp);
    match run {
        Ok(ran) if ran.ok => Some(ran.stdout),
        // A file the helper fails on is one it will fail on again, and says nothing: kept
        // as such. One it ran out of time on may have been waiting for a busy disk.
        Ok(ran) => {
            glib::g_debug!(
                "spiral",
                "probe of {} failed: {}",
                path.display(),
                ran.trouble
            );
            (!ran.timed_out).then(Vec::new)
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
            "depth" => facts.depth = int(),
            "audio_tracks" => facts.audio_tracks = int(),
            "subtitles" => facts.subtitles = text(),
            "subtitle_tracks" => facts.subtitle_tracks = int(),
            "title" => facts.title = text(),
            "author" => facts.author = text(),
            "pages" => facts.pages = int(),
            "words" => facts.words = int(),
            "slides" => facts.slides = int(),
            "page_width" => facts.page_width = real(),
            "page_height" => facts.page_height = real(),
            "paper" => facts.paper = text(),
            "artist" => facts.artist = text(),
            "album" => facts.album = text(),
            "album_artist" => facts.album_artist = text(),
            "genre" => facts.genre = text(),
            "track" => facts.track = int(),
            "tracks" => facts.tracks = int(),
            "year" => facts.year = int(),
            _ => {}
        }
    }
    facts
}

// ---- words for the panel ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::display::*;
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
        assert_eq!(shutter(0.25), "1/4");
        assert_eq!(shutter(0.6), "0.6");
        assert_eq!(shutter(2.5), "2.5");
        assert_eq!(trimmed(22.05, 3), "22.05");
        assert_eq!(trimmed(44.1, 3), "44.1");
        assert_eq!(trimmed(2.8, 1), "2.8");
        assert_eq!(trimmed(35.0, 1), "35");
        assert_eq!(trimmed(29.97, 2), "29.97");
        assert_eq!(duration_text(187_400), "3:07");
        assert_eq!(duration_text(3_723_000), "1:02:03");
        assert_eq!(language_names("en, xx"), "English, xx");
        // Tokyo's clock, not the viewer's.
        assert!(
            taken_text("2024-05-17T18:42:07+09:00")
                .unwrap()
                .ends_with("18:42")
        );
    }

    #[test]
    fn a_picture_says_its_size_in_its_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(640u32.to_be_bytes());
        png.extend(480u32.to_be_bytes());
        assert_eq!(header_size(&png), Some((640, 480, false)));
        assert_eq!(
            header_size(b"GIF89a\x40\x01\xf0\x00"),
            Some((320, 240, false))
        );
        let webp = |chunk: &[u8], body: &[u8]| {
            [b"RIFF\0\0\0\0WEBP".as_slice(), chunk, &[0; 4], body].concat()
        };
        let lossy = webp(
            b"VP8 ",
            &[0, 0, 0, 0x9d, 0x01, 0x2a, 0x20, 0x03, 0x58, 0x02],
        );
        assert_eq!(header_size(&lossy), Some((800, 600, false)));
        let bits: u32 = 99 | (49 << 14);
        let lossless = webp(b"VP8L", &[&[0x2f], bits.to_le_bytes().as_slice()].concat());
        assert_eq!(header_size(&lossless), Some((100, 50, false)));
        let extended = webp(b"VP8X", &[0, 0, 0, 0, 0x7f, 0x07, 0, 0x37, 0x04, 0]);
        assert_eq!(header_size(&extended), Some((1920, 1080, false)));
        // EXIF and a Huffman table, then a fill byte, ahead of the frame header.
        let jpeg = b"\xff\xd8\xff\xe1\0\x08Exif\0\0\xff\xc4\0\x04\0\0\xff\xff\xc0\0\x11\x08\x01\xe0\x02\x80";
        assert_eq!(header_size(jpeg), Some((640, 480, true)));
        assert_eq!(header_size(&jpeg[..20]), None);
        assert_eq!(header_size(b"\xff\xd8\xff\xd9"), None);
        assert_eq!(header_size(b"BM"), None);
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

    enum V {
        A(&'static str),
        S(u16),
        R(u32, u32),
    }

    /// A TIFF block with one directory holding `entries`, which must be in tag order.
    fn tiff(big: bool, entries: &[(u16, V)]) -> Vec<u8> {
        let u16b = |n: u16| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let u32b = |n: u32| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let mut out = if big {
            b"MM\0*".to_vec()
        } else {
            b"II*\0".to_vec()
        };
        out.extend(u32b(8));
        let mut data = Vec::new();
        let data_at = 8 + 2 + 12 * entries.len() + 4;
        out.extend(u16b(entries.len() as u16));
        for (tag, value) in entries {
            out.extend(u16b(*tag));
            let (kind, count, bytes) = match value {
                V::A(text) => (
                    2u16,
                    text.len() as u32 + 1,
                    [text.as_bytes(), b"\0"].concat(),
                ),
                V::S(n) => (3, 1, [u16b(*n), [0, 0]].concat()),
                V::R(a, b) => (5, 1, [u32b(*a), u32b(*b)].concat()),
            };
            out.extend(u16b(kind));
            out.extend(u32b(count));
            if bytes.len() <= 4 {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                out.extend(inline);
            } else {
                out.extend(u32b((data_at + data.len()) as u32));
                data.extend(bytes);
            }
        }
        out.extend(u32b(0));
        out.extend(data);
        out
    }

    fn probe_bytes(kind: &str, bytes: &[u8]) -> Facts {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "spiral-probe-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        let out = probe(kind, &path);
        let _ = std::fs::remove_file(&path);
        parse(&out)
    }

    #[test]
    fn raw_files_under_another_signature_are_read_as_tiff() {
        let block = tiff(
            false,
            &[
                (MAKE, V::A("Panasonic")),
                (MODEL, V::A("DC-S5")),
                (ORIENTATION, V::S(6)),
                (PIXEL_X, V::S(6000)),
                (PIXEL_Y, V::S(4000)),
            ],
        );
        for signature in [b"IIU\0", b"IIRO"] {
            let mut raw = block.clone();
            raw[..4].copy_from_slice(signature);
            let facts = probe_bytes("raw", &raw);
            assert_eq!(facts.camera.as_deref(), Some("Panasonic DC-S5"));
            // Held upright: the picture is taller than the sensor is wide.
            assert_eq!((facts.width, facts.height), (Some(4000), Some(6000)));
        }
    }

    #[test]
    fn taken_is_when_the_shutter_went_not_when_a_program_saved() {
        let edited = tiff(true, &[(0x0132, V::A("2025:01:02 03:04:05"))]);
        assert_eq!(probe_bytes("image", &edited).taken, None);
        let scanned = tiff(
            true,
            &[
                (0x0132, V::A("2025:01:02 03:04:05")),
                (DATE_TIME_DIGITIZED, V::A("1999:12:31 23:59:00")),
            ],
        );
        assert_eq!(
            probe_bytes("image", &scanned).taken.as_deref(),
            Some("1999-12-31T23:59:00")
        );
    }

    #[test]
    fn exif_is_found_inside_containers_the_reader_does_not_know() {
        // A Fujifilm raw file: a header, then a JPEG preview carrying the EXIF.
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(160, 0);
        raf.extend(jpeg_with_exif());
        raf.extend([0u8; 64]);
        let facts = probe_bytes("raw", &raf);
        assert_eq!(facts.camera.as_deref(), Some("ACME Shooter 3000"));
        assert_eq!(facts.exposure, Some(0.004));

        // A CR3: the camera in one metadata box, the exposure in the next.
        let mut cr3 = b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom".to_vec();
        cr3.extend(b"\0\0\0\0CMT1");
        cr3.extend(tiff(
            false,
            &[(MAKE, V::A("Canon")), (MODEL, V::A("Canon EOS R5"))],
        ));
        cr3.extend(b"\0\0\0\0CMT2");
        cr3.extend(tiff(
            false,
            &[(EXPOSURE_TIME, V::R(1, 500)), (ISO, V::S(400))],
        ));
        let facts = probe_bytes("raw", &cr3);
        assert_eq!(facts.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(facts.exposure, Some(0.002));
        assert_eq!(facts.iso, Some(400));
    }

    /// A zip of `members`: name, content, and whether it is deflated or stored.
    fn zip_of(members: &[(&str, &str, bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data, deflate) in members {
            let body = if *deflate {
                miniz_oxide::deflate::compress_to_vec(data.as_bytes(), 6)
            } else {
                data.as_bytes().to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            let sizes = [
                (body.len() as u32).to_le_bytes(),
                (data.len() as u32).to_le_bytes(),
            ];
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]);
            out.extend(sizes.concat());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&body);
            central.extend(b"PK\x01\x02");
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend(sizes.concat());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]);
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        let count = (members.len() as u16).to_le_bytes();
        out.extend(&central);
        out.extend(b"PK\x05\x06");
        out.extend([0; 4]);
        out.extend(count);
        out.extend(count);
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn documents_say_their_title_author_and_counts() {
        let odt = zip_of(&[
            ("mimetype", "application/vnd.oasis.opendocument.text", false),
            (
                "meta.xml",
                "<office:meta><meta:initial-creator>Ada</meta:initial-creator>\
                 <dc:title>Notes &amp; Sketches</dc:title>\
                 <meta:document-statistic meta:page-count=\"12\" meta:word-count=\"3456\"/>\
                 </office:meta>",
                true,
            ),
        ]);
        let facts = probe_bytes("document", &odt);
        assert_eq!(facts.title.as_deref(), Some("Notes & Sketches"));
        assert_eq!(facts.author.as_deref(), Some("Ada"));
        assert_eq!((facts.pages, facts.words), (Some(12), Some(3456)));

        let docx = zip_of(&[
            (
                "docProps/core.xml",
                "<cp:coreProperties><dc:title>Report</dc:title>\
                 <dc:creator>Grace</dc:creator></cp:coreProperties>",
                true,
            ),
            (
                "docProps/app.xml",
                "<Properties><Slides>9</Slides></Properties>",
                false,
            ),
        ]);
        let facts = probe_bytes("document", &docx);
        assert_eq!(facts.author.as_deref(), Some("Grace"));
        assert_eq!(facts.slides, Some(9));

        let epub = zip_of(&[
            ("mimetype", "application/epub+zip", false),
            (
                "META-INF/container.xml",
                "<container><rootfiles><rootfile full-path=\"b/c.opf\"/></rootfiles></container>",
                true,
            ),
            (
                "b/c.opf",
                "<package><metadata><dc:title>Frankenstein</dc:title>\
                 <dc:creator opf:role=\"aut\">Mary Shelley</dc:creator></metadata></package>",
                true,
            ),
        ]);
        let facts = probe_bytes("document", &epub);
        assert_eq!(facts.title.as_deref(), Some("Frankenstein"));
        assert_eq!(facts.author.as_deref(), Some("Mary Shelley"));

        // Not a zip, or a zip without these: nothing, and no trouble.
        assert_eq!(
            probe_bytes("document", b"PK\x05\x06 but not really"),
            Facts::default()
        );
        assert_eq!(
            probe_bytes("document", &zip_of(&[("a.txt", "hello", true)])),
            Facts::default()
        );
    }

    #[test]
    fn xml_is_read_without_a_parser() {
        assert_eq!(
            unescape("&lt;a&gt; &amp;amp; &#233; &#x41; & plain &bogus;"),
            "<a> &amp; é A & plain &bogus;"
        );
        let xml = "<r><x:item x:count=\"3\" size='4'>text</x:item><empty/>\
                   <note><![CDATA[a <b> & c]]></note></r>";
        assert_eq!(element(xml, "item").as_deref(), Some("text"));
        assert_eq!(attribute(xml, "item", "count").as_deref(), Some("3"));
        assert_eq!(attribute(xml, "item", "size").as_deref(), Some("4"));
        assert_eq!(element(xml, "note").as_deref(), Some("a <b> & c"));
        assert_eq!(element(xml, "empty"), None);
        assert_eq!(element(xml, "missing"), None);
    }

    #[test]
    fn a_page_says_its_size_in_millimetres() {
        let facts = Facts {
            page_width: Some(595.276),
            page_height: Some(841.89),
            paper: Some("A4".into()),
            ..Facts::default()
        };
        assert_eq!(
            facts.page_text().as_deref(),
            Some("210\u{a0}×\u{a0}297\u{a0}mm (A4)")
        );
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
