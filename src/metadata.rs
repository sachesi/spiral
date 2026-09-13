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

fn probe_image(path: &Path, raw: bool) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let blocks = read_exif(path, raw);
    // By number, in whichever block has it: a CR3 keeps the camera and the exposure in
    // separate ones. Never the GPS block, never the thumbnail's directory.
    let field = |number: u16| {
        blocks
            .iter()
            .flat_map(|e| e.fields())
            .find(|f| {
                f.tag.number() == number
                    && f.tag.context() != exif::Context::Gps
                    && f.ifd_num == exif::In::PRIMARY
            })
            .map(|f| &f.value)
    };
    let text = |number| match field(number) {
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

    // The size the picture is drawn at: turned the way the camera was held. A raw file is
    // not a picture gdk-pixbuf can size, or it sizes the preview inside; the camera says the
    // size of the picture in the EXIF.
    // Only a JPEG, as in the preview, and a raw file are turned by their tag: HEIF and AVIF
    // carry a turn of their own in the container, and the tag beside it would turn them twice.
    let (size, follows_tag) = if raw {
        let size = number(PIXEL_X)
            .zip(number(PIXEL_Y))
            .map(|(w, h)| (w as i32, h as i32));
        (size, true)
    } else {
        match crate::gtk::gdk_pixbuf::Pixbuf::file_info(path) {
            Some((format, w, h)) => (Some((w, h)), format.name().as_deref() == Some("jpeg")),
            None => (None, false),
        }
    };
    if let Some((width, height)) = size.filter(|&(w, h)| w > 0 && h > 0) {
        let turned =
            follows_tag && matches!(field(ORIENTATION).and_then(|v| v.get_uint(0)), Some(5..=8));
        let (width, height) = if turned {
            (height, width)
        } else {
            (width, height)
        };
        out.push(("width", width.to_string()));
        out.push(("height", height.to_string()));
    }
    // When the shutter went, or failing that when the picture was scanned; never the plain
    // DateTime, which is when a program last changed the file.
    let taken = [
        (DATE_TIME_ORIGINAL, OFFSET_TIME_ORIGINAL),
        (DATE_TIME_DIGITIZED, OFFSET_TIME_DIGITIZED),
    ]
    .into_iter()
    .find_map(|(date, offset)| {
        let Some(exif::Value::Ascii(parts)) = field(date) else {
            return None;
        };
        let mut date = exif::DateTime::from_ascii(parts.first()?).ok()?;
        if let Some(exif::Value::Ascii(offset)) = field(offset)
            && let Some(offset) = offset.first()
        {
            let _ = date.parse_offset(offset);
        }
        Some(date)
    });
    if let Some(date) = taken {
        out.push(("taken", iso8601(&date)));
    }
    let make = text(MAKE);
    if let Some(model) = text(MODEL) {
        out.push(("camera", camera(make.as_deref(), &model)));
    }
    if let Some(lens) = text(LENS_MODEL) {
        out.push(("lens", lens));
    }
    for (key, tag) in [
        ("aperture", F_NUMBER),
        ("exposure", EXPOSURE_TIME),
        ("iso", ISO),
        ("focal", FOCAL_LENGTH),
    ] {
        if let Some(n) = number(tag) {
            out.push((key, n.to_string()));
        }
    }
    out
}

const MAKE: u16 = 0x010f;
const MODEL: u16 = 0x0110;
const ORIENTATION: u16 = 0x0112;
const EXPOSURE_TIME: u16 = 0x829a;
const F_NUMBER: u16 = 0x829d;
const ISO: u16 = 0x8827;
const DATE_TIME_ORIGINAL: u16 = 0x9003;
const DATE_TIME_DIGITIZED: u16 = 0x9004;
const OFFSET_TIME_ORIGINAL: u16 = 0x9011;
const OFFSET_TIME_DIGITIZED: u16 = 0x9012;
const FOCAL_LENGTH: u16 = 0x920a;
const PIXEL_X: u16 = 0xa002;
const PIXEL_Y: u16 = 0xa003;
const LENS_MODEL: u16 = 0xa434;

/// How much of a file is searched for EXIF the reader does not find on its own.
const EXIF_SCAN: usize = 4 * 1024 * 1024;
/// How much of a TIFF, or of a raw file built on one, is read. The reader would otherwise
/// take the whole file, a hundred megabytes for a raw file and more for a scan, to find a
/// directory that is nearly always at the head; one written at the end of a larger file is
/// not found.
const TIFF_MAX: usize = 64 * 1024 * 1024;
/// How much is handed to the reader from each TIFF header found in it.
const EXIF_BLOCK: usize = 512 * 1024;
/// How many TIFF headers are tried before the search gives up.
const EXIF_TRIES: usize = 32;

/// The JPEG XL container, whose Exif box the reader does not know.
const JXL: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";

/// The EXIF of the file at `path`. A TIFF, and a raw file built on one, from its head; an
/// Olympus or Panasonic raw file is a TIFF under another signature. Other containers the
/// reader knows (JPEG, HEIF and AVIF, PNG, WebP) as it reads them. Beyond those, for a raw
/// file or a JPEG XL, what can be found in the head of the file: a Fujifilm raw file, the
/// metadata boxes of a CR3 and the Exif box of a JPEG XL hold TIFF blocks the reader can
/// take one by one. A picture without EXIF is not searched for any.
fn read_exif(path: &Path, raw: bool) -> Vec<exif::Exif> {
    let reader = exif::Reader::new();
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let head_of = |most: usize| {
        let mut head = Vec::new();
        std::io::Seek::rewind(&mut &file).ok()?;
        std::io::Read::read_to_end(&mut std::io::Read::take(&file, most as u64), &mut head).ok()?;
        Some(head)
    };
    let Some(signature) = head_of(JXL.len()) else {
        return Vec::new();
    };
    let tiff_under = match signature.get(..4) {
        Some(b"II*\0" | b"MM\0*") => Some(None),
        Some(b"IIU\0" | b"IIRO" | b"IIRS") => Some(Some([0x2a, 0])),
        Some(b"MMOR") => Some(Some([0, 0x2a])),
        _ => None,
    };
    if let Some(magic) = tiff_under {
        let Some(mut head) = head_of(TIFF_MAX) else {
            return Vec::new();
        };
        if let Some(magic) = magic {
            head[2..4].copy_from_slice(&magic);
        }
        if let Ok(exif) = reader.read_raw(head) {
            return vec![exif];
        }
    } else if std::io::Seek::rewind(&mut &file).is_ok()
        && let Ok(exif) = reader.read_from_container(&mut std::io::BufReader::new(&file))
    {
        return vec![exif];
    }
    if !raw && signature != JXL {
        return Vec::new();
    }
    let Some(head) = head_of(EXIF_SCAN) else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    let mut at = 0;
    let mut tries = 0;
    while tries < EXIF_TRIES
        && let Some(found) = head[at..]
            .windows(4)
            .position(|w| w == b"II*\0" || w == b"MM\0*")
    {
        let start = at + found;
        tries += 1;
        let end = (start + EXIF_BLOCK).min(head.len());
        if let Ok(exif) = reader.read_raw(head[start..end].to_vec()) {
            blocks.push(exif);
        }
        at = start + 4;
    }
    blocks
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
    // The title of a song is a tag of the file, or of its one sound stream where the file has
    // no tags of its own (Ogg, FLAC). The streams of a video are titled after the track
    // ("Audio", "English"), which says nothing of the video.
    // A picture in the file, a cover, is listed with the video; it is not one.
    let videos: Vec<_> = info
        .video_streams()
        .into_iter()
        .filter(|v| !v.is_image())
        .collect();
    let audios = info.audio_streams();
    let global = info
        .stream_info()
        .and_then(|s| {
            s.downcast::<gstreamer_pbutils::DiscovererContainerInfo>()
                .ok()
        })
        .and_then(|c| c.tags());
    let own = videos
        .is_empty()
        .then(|| audios.first().and_then(|a| a.tags()))
        .flatten();
    let tags: Vec<gst::TagList> = [global, own].into_iter().flatten().collect();
    let text = |get: fn(&gst::TagList) -> Option<String>| {
        tags.iter().find_map(get).filter(|s| !s.trim().is_empty())
    };
    let number = |get: fn(&gst::TagList) -> Option<u32>| tags.iter().find_map(get);

    if let Some(video) = videos.first() {
        // A phone held upright records the picture on its side and says so in a tag.
        let orientation = video
            .tags()
            .into_iter()
            .chain(tags.iter().cloned())
            .find_map(|t| {
                t.get::<gst::tags::ImageOrientation>()
                    .map(|o| o.get().to_string())
            });
        let upright = orientation.is_some_and(|o| o.ends_with("-90") || o.ends_with("-270"));
        let (width, height) = if upright {
            (video.height(), video.width())
        } else {
            (video.width(), video.height())
        };
        if width > 0 && height > 0 {
            out.push(("width", width.to_string()));
            out.push(("height", height.to_string()));
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
    if let Some(audio) = audios.first() {
        if let Some(name) = codec(audio.caps()) {
            out.push(("audio", name));
        }
        // Only lossless sound keeps its bit depth; for the rest it is what the decoder would
        // hand out, 32-bit float for Vorbis or AAC.
        let lossless = audio
            .caps()
            .and_then(|c| c.structure(0).map(|s| s.name().to_string()))
            .is_some_and(|name| LOSSLESS.contains(&name.as_str()));
        for (key, value) in [
            ("channels", audio.channels()),
            ("rate", audio.sample_rate()),
            ("depth", if lossless { audio.depth() } else { 0 }),
            ("bitrate", audio.bitrate()),
        ] {
            if value > 0 {
                out.push((key, value.to_string()));
            }
        }
    }
    if audios.len() > 1 {
        out.push(("audio_tracks", audios.len().to_string()));
    }
    let subtitles = info.subtitle_streams();
    if !subtitles.is_empty() {
        let languages: Vec<String> = subtitles
            .iter()
            .filter_map(|s| s.language())
            .map(|l| l.to_string())
            .collect();
        if languages.len() == subtitles.len() {
            out.push(("subtitles", languages.join(", ")));
        } else {
            out.push(("subtitle_tracks", subtitles.len().to_string()));
        }
    }
    for (key, value) in [
        (
            "title",
            text(|t| Some(t.get::<gst::tags::Title>()?.get().to_string())),
        ),
        (
            "artist",
            text(|t| Some(t.get::<gst::tags::Artist>()?.get().to_string())),
        ),
        (
            "album",
            text(|t| Some(t.get::<gst::tags::Album>()?.get().to_string())),
        ),
        (
            "album_artist",
            text(|t| Some(t.get::<gst::tags::AlbumArtist>()?.get().to_string())),
        ),
        (
            "genre",
            text(|t| Some(t.get::<gst::tags::Genre>()?.get().to_string())),
        ),
    ] {
        if let Some(value) = value {
            out.push((key, value));
        }
    }
    for (key, value) in [
        (
            "track",
            number(|t| Some(t.get::<gst::tags::TrackNumber>()?.get())),
        ),
        (
            "tracks",
            number(|t| Some(t.get::<gst::tags::TrackCount>()?.get())),
        ),
        // The date of a song is when it came out; a video's is when it was written, which
        // the muxer fills in, and says little.
        (
            "year",
            number(|t| {
                t.get::<gst::tags::DateTime>()
                    .map(|d| d.get().year() as u32)
                    .or_else(|| {
                        t.get::<gst::tags::Date>()
                            .map(|d| u32::from(d.get().year()))
                    })
            }),
        ),
    ] {
        if let Some(value) = value.filter(|&v| v > 0)
            && (key != "year" || videos.is_empty())
        {
            out.push((key, value.to_string()));
        }
    }
    out
}

/// Caps of sound that is stored as it is played.
const LOSSLESS: [&str; 8] = [
    "audio/x-flac",
    "audio/x-alac",
    "audio/x-raw",
    "audio/x-wav",
    "audio/x-aiff",
    "audio/x-wavpack",
    "audio/x-ape",
    "audio/x-tta",
];

/// What `pdfinfo` from poppler, run here inside the sandbox, says of a PDF. Without it, as
/// for the preview, nothing.
fn probe_pdf(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let Ok(ran) = std::process::Command::new("pdfinfo")
        .args(["-enc", "UTF-8"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return out;
    };
    if !ran.status.success() {
        return out;
    }
    for line in String::from_utf8_lossy(&ran.stdout).lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "Title" => out.push(("title", value.to_string())),
            "Author" => out.push(("author", value.to_string())),
            "Pages" => out.push(("pages", value.to_string())),
            // "595.276 x 841.89 pts (A4)"
            "Page size" => {
                let mut words = value.split_whitespace();
                if let (Some(w), Some("x"), Some(h)) = (words.next(), words.next(), words.next()) {
                    out.push(("page_width", w.to_string()));
                    out.push(("page_height", h.to_string()));
                }
                if let Some(open) = value.find('(')
                    && let Some(close) = value.rfind(')')
                    && open < close
                {
                    out.push(("paper", value[open + 1..close].to_string()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The title, the author and the counts an office document or an e-book keeps in the zip
/// it is: `meta.xml` in OpenDocument, `docProps/` in Office Open XML, the package file an
/// EPUB points to.
fn probe_document(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let Some(mut zip) = Zip::open(path) else {
        return out;
    };
    let mut title = None;
    let mut author = None;
    let mut pages = None;
    let mut words = None;
    let mut slides = None;
    if let Some(meta) = zip.read("meta.xml") {
        title = element(&meta, "title");
        author = element(&meta, "initial-creator").or_else(|| element(&meta, "creator"));
        pages = attribute(&meta, "document-statistic", "page-count");
        words = attribute(&meta, "document-statistic", "word-count");
    } else if let Some(core) = zip.read("docProps/core.xml") {
        title = element(&core, "title");
        author = element(&core, "creator");
        if let Some(app) = zip.read("docProps/app.xml") {
            pages = element(&app, "Pages");
            words = element(&app, "Words");
            slides = element(&app, "Slides");
        }
    } else if let Some(package) = zip
        .read("META-INF/container.xml")
        .and_then(|c| attribute(&c, "rootfile", "full-path"))
        .and_then(|name| zip.read(&name))
    {
        title = element(&package, "title");
        author = element(&package, "creator");
    }
    for (key, value) in [
        ("title", title),
        ("author", author),
        ("pages", pages),
        ("words", words),
        ("slides", slides),
    ] {
        if let Some(value) = value {
            out.push((key, value));
        }
    }
    out
}

/// Largest member of a zip that is read, and largest central directory.
const ZIP_MEMBER_MAX: usize = 4 * 1024 * 1024;

/// Just enough of a zip to read a few small members by name: the central directory at the
/// end, and stored or deflated members.
struct Zip {
    file: std::fs::File,
    /// Name, compression method, compressed size, size, where its local header is.
    entries: Vec<(String, u16, u64, u64, u64)>,
}

impl Zip {
    fn open(path: &Path) -> Option<Self> {
        use std::io::{Read, Seek, SeekFrom};
        let le16 =
            |b: &[u8], at: usize| Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?));
        let le32 =
            |b: &[u8], at: usize| Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
        let mut file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        // The end record, after which only a comment of at most 64 KiB can come.
        let tail_len = len.min(22 + 65_535);
        file.seek(SeekFrom::Start(len - tail_len)).ok()?;
        let mut tail = vec![0; tail_len as usize];
        file.read_exact(&mut tail).ok()?;
        let end = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
        let size = le32(&tail, end + 12)? as usize;
        let offset = u64::from(le32(&tail, end + 16)?);
        if size > ZIP_MEMBER_MAX {
            return None;
        }
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut dir = vec![0; size];
        file.read_exact(&mut dir).ok()?;
        let mut entries = Vec::new();
        let mut at = 0;
        while dir.get(at..at + 4) == Some(b"PK\x01\x02") {
            let method = le16(&dir, at + 10)?;
            let compressed = u64::from(le32(&dir, at + 20)?);
            let size = u64::from(le32(&dir, at + 24)?);
            let name_len = le16(&dir, at + 28)? as usize;
            let extra_len = le16(&dir, at + 30)? as usize;
            let comment_len = le16(&dir, at + 32)? as usize;
            let local = u64::from(le32(&dir, at + 42)?);
            let name = String::from_utf8_lossy(dir.get(at + 46..at + 46 + name_len)?).into_owned();
            entries.push((name, method, compressed, size, local));
            at += 46 + name_len + extra_len + comment_len;
        }
        Some(Self { file, entries })
    }

    fn read(&mut self, name: &str) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let &(_, method, compressed, size, local) =
            self.entries.iter().find(|(n, ..)| n == name)?;
        if size as usize > ZIP_MEMBER_MAX || compressed as usize > ZIP_MEMBER_MAX {
            return None;
        }
        self.file.seek(SeekFrom::Start(local)).ok()?;
        let mut header = [0u8; 30];
        self.file.read_exact(&mut header).ok()?;
        if &header[..4] != b"PK\x03\x04" {
            return None;
        }
        let skip = u16::from_le_bytes([header[26], header[27]]) as i64
            + u16::from_le_bytes([header[28], header[29]]) as i64;
        self.file.seek(SeekFrom::Current(skip)).ok()?;
        let mut data = vec![0; compressed as usize];
        self.file.read_exact(&mut data).ok()?;
        let data = match method {
            0 => data,
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&data, ZIP_MEMBER_MAX).ok()?,
            _ => return None,
        };
        Some(String::from_utf8_lossy(&data).into_owned())
    }
}

/// The text of the first element called `name` in `xml`, whatever its namespace prefix.
fn element(xml: &str, name: &str) -> Option<String> {
    let (tag, rest) = open_tag(xml, name)?;
    if tag.ends_with('/') {
        return None;
    }
    let text = rest.trim_start();
    if let Some(cdata) = text.strip_prefix("<![CDATA[") {
        let text = &cdata[..cdata.find("]]>")?];
        return Some(text.trim().to_string()).filter(|t| !t.is_empty());
    }
    let text = &text[..text.find("</")?];
    Some(unescape(text.trim())).filter(|t| !t.is_empty())
}

/// The value of the attribute `attr` of the first element called `name`, prefixes aside.
fn attribute(xml: &str, name: &str, attr: &str) -> Option<String> {
    let (tag, _) = open_tag(xml, name)?;
    let mut rest = tag;
    while let Some(at) = rest.find(attr) {
        let before = rest[..at].chars().next_back();
        let after = &rest[at + attr.len()..];
        if matches!(
            before,
            Some(':') | Some(' ') | Some('\t') | Some('\n') | Some('\r')
        ) && let Some(quote) = after.strip_prefix('=').and_then(|a| a.chars().next())
            && (quote == '"' || quote == '\'')
        {
            let value = &after[2..];
            return Some(unescape(&value[..value.find(quote)?])).filter(|v| !v.is_empty());
        }
        rest = after;
    }
    None
}

/// The inside of the first opening tag called `name`, and what follows it.
fn open_tag<'a>(xml: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let mut rest = xml;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let end = rest.find(|c: char| c == '>' || c == '/' || c.is_whitespace())?;
        let tag_name = &rest[..end];
        if tag_name.rsplit(':').next() == Some(name) {
            let close = rest.find('>')?;
            return Some((&rest[..close], &rest[close + 1..]));
        }
    }
    None
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---- in the file manager --------------------------------------------------------------------

/// Read what the file of `info` says about itself, in the sandbox, or `None` where it is not
/// a local file of a kind the helper reads, or the helper cannot be run. Kept per version of
/// the file, so going back to one is not another run.
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
        add(gettext("Author"), self.author.clone());
        add(gettext("Pages"), self.pages.map(|n| n.to_string()));
        add(gettext("Page Size"), self.page_text());
        add(gettext("Words"), self.words.map(|n| n.to_string()));
        add(gettext("Slides"), self.slides.map(|n| n.to_string()));
        add(gettext("Artist"), self.artist.clone());
        add(gettext("Album"), self.album.clone());
        // Said only where it is not the artist again.
        add(
            gettext("Album Artist"),
            self.album_artist
                .clone()
                .filter(|a| Some(a) != self.artist.as_ref()),
        );
        add(
            gettext("Track"),
            self.track.map(|n| match self.tracks {
                Some(of) if of >= n => gettext("%s of %s")
                    .replacen("%s", &n.to_string(), 1)
                    .replacen("%s", &of.to_string(), 1),
                _ => n.to_string(),
            }),
        );
        add(gettext("Year"), self.year.map(|y| y.to_string()));
        add(gettext("Genre"), self.genre.clone());
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
        add(
            gettext("Audio Tracks"),
            self.audio_tracks.map(|n| n.to_string()),
        );
        add(
            gettext("Subtitles"),
            self.subtitles
                .as_deref()
                .map(language_names)
                .or_else(|| self.subtitle_tracks.map(|n| n.to_string())),
        );
        rows
    }

    /// "210 × 297 mm (A4)"
    fn page_text(&self) -> Option<String> {
        let (w, h) = self.page_width.zip(self.page_height)?;
        let mm = |points: f64| (points * 25.4 / 72.0).round().to_string();
        let size = unit(gettext("%s mm").replace("%s", &format!("{} × {}", mm(w), mm(h))));
        Some(match &self.paper {
            Some(paper) => {
                let mut name = paper.chars();
                let first = name.next().map(|c| c.to_uppercase().collect::<String>());
                format!("{size} ({}{})", first.unwrap_or_default(), name.as_str())
            }
            None => size,
        })
    }

    fn exposure_text(&self) -> Option<String> {
        let parts: Vec<String> = [
            self.aperture.map(|f| format!("f/{}", trimmed(f, 1))),
            self.exposure
                .map(|t| unit(gettext("%s s").replace("%s", &shutter(t)))),
            self.iso.map(|n| unit(format!("ISO {n}"))),
            self.focal
                .map(|mm| unit(gettext("%s mm").replace("%s", &trimmed(mm, 1)))),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn video_text(&self) -> Option<String> {
        let fps = self
            .fps
            .map(|f| unit(gettext("%s fps").replace("%s", &trimmed(f, 2))));
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
            .map(|hz| unit(gettext("%s kHz").replace("%s", &trimmed(f64::from(hz) / 1000.0, 3))));
        let bitrate = self
            .bitrate
            .map(|b| unit(gettext("%s kbit/s").replace("%s", &(b / 1000).to_string())));
        let depth = self
            .depth
            .map(|d| unit(gettext("%s-bit").replace("%s", &d.to_string())));
        join([self.audio.clone(), channels, rate, depth, bitrate])
    }
}

/// "en, de" as "English, German", in the language of the interface where iso-codes has it.
fn language_names(codes: &str) -> String {
    codes
        .split(", ")
        .map(|code| {
            gstreamer_tag::language_codes::language_name(code)
                .map(|name| name.to_string())
                .unwrap_or_else(|| code.to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A number and its unit, kept on one line when the panel wraps the rest.
fn unit(text: String) -> String {
    text.replace(' ', "\u{a0}")
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

/// A shutter speed as photographers write it: 1/250 up to a quarter of a second, 0.6 and
/// 2.5 above.
fn shutter(seconds: f64) -> String {
    if seconds <= 0.25 {
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
