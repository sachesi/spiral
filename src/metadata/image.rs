//! What a picture says: its size, from the header where it can, and its EXIF.

use super::*;

pub(super) fn probe_image(path: &Path, raw: bool) -> Vec<(&'static str, String)> {
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
        match picture_size(path) {
            Some((w, h, jpeg)) => (Some((w, h)), jpeg),
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

/// Width and height of the picture at `path` as its header says, and whether it is a JPEG,
/// whose EXIF tag may turn it.
pub(crate) fn picture_size(path: &Path) -> Option<(i32, i32, bool)> {
    let mut head = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(std::fs::File::open(path).ok()?, IMAGE_HEAD as u64),
        &mut head,
    )
    .ok()?;
    header_size(&head).or_else(|| {
        let (format, width, height) = crate::gtk::gdk_pixbuf::Pixbuf::file_info(path)?;
        Some((width, height, format.name().as_deref() == Some("jpeg")))
    })
}

/// How much of a picture is read for its size: a JPEG may keep a thumbnail and a colour
/// profile ahead of the frame header that says it.
pub(super) const IMAGE_HEAD: usize = 256 * 1024;

/// Width and height from the header of a PNG, a JPEG, a GIF or a WebP, and whether it is a
/// JPEG. gdk-pixbuf asks a loader in a process of its own for the size of any picture,
/// which for a large one takes as long as decoding it.
pub(super) fn header_size(head: &[u8]) -> Option<(i32, i32, bool)> {
    // The number in the `len` bytes at `at`, the most significant first when `big`.
    let int = |at: usize, len: usize, big: bool| {
        let bytes = head.get(at..at + len)?;
        let add = |n: i64, b: &u8| (n << 8) | i64::from(*b);
        Some(if big {
            bytes.iter().fold(0, add)
        } else {
            bytes.iter().rev().fold(0, add)
        })
    };
    let size = |width: i64, height: i64, jpeg: bool| {
        Some((
            i32::try_from(width).ok()?,
            i32::try_from(height).ok()?,
            jpeg,
        ))
    };
    if head.starts_with(b"\x89PNG\r\n\x1a\n") && head.get(12..16) == Some(b"IHDR") {
        return size(int(16, 4, true)?, int(20, 4, true)?, false);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return size(int(6, 2, false)?, int(8, 2, false)?, false);
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return match head.get(12..16)? {
            b"VP8 " if head.get(23..26) == Some(&[0x9d, 0x01, 0x2a]) => size(
                int(26, 2, false)? & 0x3fff,
                int(28, 2, false)? & 0x3fff,
                false,
            ),
            b"VP8L" if head.get(20) == Some(&0x2f) => {
                let bits = int(21, 4, false)?;
                size((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1, false)
            }
            b"VP8X" => size(int(24, 3, false)? + 1, int(27, 3, false)? + 1, false),
            _ => None,
        };
    }
    if !head.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    // A JPEG: segments, each a marker and most with a length, up to the frame header.
    let mut at = 2;
    loop {
        if *head.get(at)? != 0xff {
            return None;
        }
        while *head.get(at)? == 0xff {
            at += 1;
        }
        let marker = head[at];
        at += 1;
        match marker {
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                return size(int(at + 5, 2, true)?, int(at + 3, 2, true)?, true);
            }
            0xd9 => return None,
            0x01 | 0xd0..=0xd8 => {}
            _ => at += int(at, 2, true)? as usize,
        }
    }
}

pub(super) const MAKE: u16 = 0x010f;

pub(super) const MODEL: u16 = 0x0110;

pub(super) const ORIENTATION: u16 = 0x0112;

pub(super) const EXPOSURE_TIME: u16 = 0x829a;

pub(super) const F_NUMBER: u16 = 0x829d;

pub(super) const ISO: u16 = 0x8827;

pub(super) const DATE_TIME_ORIGINAL: u16 = 0x9003;

pub(super) const DATE_TIME_DIGITIZED: u16 = 0x9004;

pub(super) const OFFSET_TIME_ORIGINAL: u16 = 0x9011;

pub(super) const OFFSET_TIME_DIGITIZED: u16 = 0x9012;

pub(super) const FOCAL_LENGTH: u16 = 0x920a;

pub(super) const PIXEL_X: u16 = 0xa002;

pub(super) const PIXEL_Y: u16 = 0xa003;

pub(super) const LENS_MODEL: u16 = 0xa434;

/// How much of a file is searched for EXIF the reader does not find on its own.
pub(super) const EXIF_SCAN: usize = 4 * 1024 * 1024;

/// How much of a TIFF, or of a raw file built on one, is read. The reader would otherwise
/// take the whole file, a hundred megabytes for a raw file and more for a scan, to find a
/// directory that is nearly always at the head; one written at the end of a larger file is
/// not found.
pub(super) const TIFF_MAX: usize = 64 * 1024 * 1024;

/// How much is handed to the reader from each TIFF header found in it.
pub(super) const EXIF_BLOCK: usize = 512 * 1024;

/// How many TIFF headers are tried before the search gives up.
pub(super) const EXIF_TRIES: usize = 32;

/// The JPEG XL container, whose Exif box the reader does not know.
pub(super) const JXL: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";

/// The EXIF of the file at `path`. A TIFF, and a raw file built on one, from its head; an
/// Olympus or Panasonic raw file is a TIFF under another signature. Other containers the
/// reader knows (JPEG, HEIF and AVIF, PNG, WebP) as it reads them. Beyond those, for a raw
/// file or a JPEG XL, what can be found in the head of the file: a Fujifilm raw file, the
/// metadata boxes of a CR3 and the Exif box of a JPEG XL hold TIFF blocks the reader can
/// take one by one. A picture without EXIF is not searched for any.
pub(super) fn read_exif(path: &Path, raw: bool) -> Vec<exif::Exif> {
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

pub(super) fn iso8601(date: &exif::DateTime) -> String {
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
pub(super) fn camera(make: Option<&str>, model: &str) -> String {
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
