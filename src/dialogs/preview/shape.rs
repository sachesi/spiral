//! How big the preview opens: what a file says of its own size, read from as little of it as
//! will do, and the window that size makes within the room there is.

use super::*;
use crate::picture::{EXIF_SCAN, exif_orientation};

/// The first `most` bytes of the file at `path`.
pub(super) fn head_of(path: &Path, most: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut head = vec![0u8; most];
    let read = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    head.truncate(read);
    Some(head)
}

/// Width and height of a local image from its header alone, turned the way its EXIF tag
/// says, which is the way it will be drawn.
pub(super) fn image_size(path: &Path) -> Option<(i32, i32)> {
    let (width, height, jpeg) = crate::metadata::picture_size(path)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    let turned = jpeg && exif_turned(path);
    Some(if turned {
        (height, width)
    } else {
        (width, height)
    })
}

/// Width and height of a local video as it will be shown, from the container's header
/// alone: the track header of an MP4 or MOV, turned the way its matrix says a phone held
/// the camera, or the video track of a Matroska or WebM. A container the reader does not
/// know, or a header it cannot find, gives nothing.
pub(super) fn video_size(path: &Path) -> Option<(f64, f64)> {
    let head = head_of(path, 16)?;
    if head.get(4..8) == Some(b"ftyp") {
        return mp4_size(&mut std::fs::File::open(path).ok()?);
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return matroska_size(&head_of(path, MATROSKA_SCAN)?);
    }
    None
}

/// How much of a Matroska file is read looking for its track list, which sits ahead of
/// the clusters.
pub(super) const MATROSKA_SCAN: usize = 2 * 1024 * 1024;

/// The size of the first video track of an MP4, from the `tkhd` box under `moov`, which
/// a phone writes at the end of the file, after the media, so the boxes are walked by
/// seeking rather than read.
pub(super) fn mp4_size<R: std::io::Read + std::io::Seek>(file: &mut R) -> Option<(f64, f64)> {
    use std::io::{Read, SeekFrom};
    // Every box: a 32-bit size and a type, the size covering both; 1 means a 64-bit size
    // follows, 0 means the box runs to the end of the file.
    fn header<R: Read>(file: &mut R) -> Option<(u64, [u8; 4], u64)> {
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let mut size = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
        let kind: [u8; 4] = head[4..8].try_into().ok()?;
        let mut header_len = 8;
        if size == 1 {
            let mut large = [0u8; 8];
            file.read_exact(&mut large).ok()?;
            size = u64::from_be_bytes(large);
            header_len = 16;
        }
        Some((size, kind, header_len))
    }
    let end = file.seek(SeekFrom::End(0)).ok()?;
    let mut at = 0u64;
    for _ in 0..64 {
        if at >= end {
            return None;
        }
        file.seek(SeekFrom::Start(at)).ok()?;
        let (size, kind, header_len) = header(file)?;
        let size = if size == 0 { end - at } else { size };
        if size < header_len {
            return None;
        }
        if &kind == b"moov" {
            let mut moov = vec![0u8; usize::try_from(size - header_len).ok()?.min(8 << 20)];
            file.read_exact(&mut moov).ok()?;
            return moov_size(&moov);
        }
        at = at.checked_add(size)?;
    }
    None
}

/// The first `trak` under `moov` whose `tkhd` gives a size: audio tracks say 0 by 0.
pub(super) fn moov_size(moov: &[u8]) -> Option<(f64, f64)> {
    for (kind, body) in boxes(moov) {
        if kind != b"trak" {
            continue;
        }
        for (kind, body) in boxes(body) {
            if kind == b"tkhd"
                && let Some(size) = tkhd_size(body)
            {
                return Some(size);
            }
        }
    }
    None
}

/// The boxes laid end to end in `data`, as type and body.
pub(super) fn boxes(data: &[u8]) -> impl Iterator<Item = (&[u8; 4], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let head = data.get(at..at + 8)?;
        let mut size = usize::try_from(u32::from_be_bytes(head[..4].try_into().ok()?)).ok()?;
        let kind: &[u8; 4] = head[4..8].try_into().ok()?;
        let mut header_len = 8;
        if size == 1 {
            let large = data.get(at + 8..at + 16)?;
            size = usize::try_from(u64::from_be_bytes(large.try_into().ok()?)).ok()?;
            header_len = 16;
        }
        if size == 0 {
            size = data.len() - at;
        }
        if size < header_len {
            return None;
        }
        let body = data.get(at + header_len..at + size)?;
        at += size;
        Some((kind, body))
    })
}

/// Width and height from a track header, as 16.16 fixed point after the matrix, and
/// swapped when the matrix turns the picture a quarter turn either way.
pub(super) fn tkhd_size(body: &[u8]) -> Option<(f64, f64)> {
    // Version 1 has 64-bit times, which puts everything after them 12 bytes further on.
    let times = if body.first()? == &1 { 32 } else { 20 };
    // Flags, times, then reserved, layer, group, volume, reserved: the matrix is next.
    let matrix = 4 + times + 8 + 2 + 2 + 2 + 2;
    let fixed = |at: usize| -> Option<i32> {
        Some(i32::from_be_bytes(body.get(at..at + 4)?.try_into().ok()?))
    };
    let (a, b, c, d) = (
        fixed(matrix)?,
        fixed(matrix + 4)?,
        fixed(matrix + 12)?,
        fixed(matrix + 16)?,
    );
    let width = f64::from(fixed(matrix + 36)?) / 65536.0;
    let height = f64::from(fixed(matrix + 40)?) / 65536.0;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let turned = a == 0 && d == 0 && b != 0 && c != 0;
    Some(if turned {
        (height, width)
    } else {
        (width, height)
    })
}

/// The size of the first video track of a Matroska or WebM file, as it is meant to be
/// shown where the track says so, else as it is stored.
pub(super) fn matroska_size(data: &[u8]) -> Option<(f64, f64)> {
    // EBML: an element is an id, a size, both as variable-length integers, and a body.
    fn vint(data: &[u8], at: usize, keep_marker: bool) -> Option<(u64, usize)> {
        let first = *data.get(at)?;
        let len = first.leading_zeros() as usize + 1;
        if len > 8 {
            return None;
        }
        let mut value = if keep_marker {
            u64::from(first)
        } else {
            u64::from(first) & ((1u64 << (8 - len)) - 1)
        };
        for i in 1..len {
            value = (value << 8) | u64::from(*data.get(at + i)?);
        }
        Some((value, len))
    }
    fn uint(body: &[u8]) -> Option<u64> {
        (1..=8)
            .contains(&body.len())
            .then(|| body.iter().fold(0u64, |n, &b| (n << 8) | u64::from(b)))
    }
    // Elements with their bodies, from `at` to `end`; a size of all ones is unknown and
    // taken to run to the end.
    fn elements(data: &[u8], mut at: usize, end: usize) -> impl Iterator<Item = (u64, &[u8])> {
        std::iter::from_fn(move || {
            if at >= end {
                return None;
            }
            let (id, id_len) = vint(data, at, true)?;
            let (size, size_len) = vint(data, at + id_len, false)?;
            let unknown = size == (1u64 << (7 * size_len)) - 1;
            let start = at + id_len + size_len;
            let stop = if unknown {
                end
            } else {
                start.checked_add(usize::try_from(size).ok()?)?.min(end)
            };
            let body = data.get(start..stop)?;
            at = stop;
            Some((id, body))
        })
    }
    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    const TRACK_ENTRY: u64 = 0xAE;
    const TRACK_TYPE: u64 = 0x83;
    const VIDEO: u64 = 0xE0;
    const PIXEL_WIDTH: u64 = 0xB0;
    const PIXEL_HEIGHT: u64 = 0xBA;
    const DISPLAY_WIDTH: u64 = 0x54B0;
    const DISPLAY_HEIGHT: u64 = 0x54BA;
    let (_, segment) = elements(data, 0, data.len()).find(|(id, _)| *id == SEGMENT)?;
    let mut tracks = None;
    for (id, body) in elements(segment, 0, segment.len()) {
        match id {
            TRACKS => {
                tracks = Some(body);
                break;
            }
            CLUSTER => break,
            _ => {}
        }
    }
    for (id, entry) in elements(tracks?, 0, tracks?.len()) {
        if id != TRACK_ENTRY {
            continue;
        }
        let mut is_video = false;
        let mut video = None;
        for (id, body) in elements(entry, 0, entry.len()) {
            match id {
                TRACK_TYPE => is_video = uint(body) == Some(1),
                VIDEO => video = Some(body),
                _ => {}
            }
        }
        let Some(video) = video.filter(|_| is_video) else {
            continue;
        };
        let mut pixel = (None, None);
        let mut display = (None, None);
        for (id, body) in elements(video, 0, video.len()) {
            match id {
                PIXEL_WIDTH => pixel.0 = uint(body),
                PIXEL_HEIGHT => pixel.1 = uint(body),
                DISPLAY_WIDTH => display.0 = uint(body),
                DISPLAY_HEIGHT => display.1 = uint(body),
                _ => {}
            }
        }
        let (width, height) = match (display, pixel) {
            ((Some(w), Some(h)), _) | (_, (Some(w), Some(h))) => (w, h),
            _ => continue,
        };
        if width > 0 && height > 0 {
            return Some((width as f64, height as f64));
        }
    }
    None
}

/// Whether the EXIF tag of the image at `path` turns it on its side.
pub(super) fn exif_turned(path: &Path) -> bool {
    head_of(path, EXIF_SCAN).is_some_and(|head| matches!(exif_orientation(&head), 5..=8))
}

/// The size of the first page in points, read out of the file: no tool to start and no
/// page to render, so the dialog has the shape before it opens. `None` when the page
/// dictionary is compressed out of reach, which is what `pdfinfo` answers later.
pub(super) fn pdf_page_size(path: &Path) -> Option<(f64, f64)> {
    media_box(&String::from_utf8_lossy(&head_of(path, PDF_SCAN)?))
}

/// The first `/MediaBox` in `text`, as a width and a height.
pub(super) fn media_box(text: &str) -> Option<(f64, f64)> {
    let box_ = text.find("/MediaBox")?;
    let open = text[box_..].find('[')? + box_ + 1;
    let close = text[open..].find(']')? + open;
    let mut corners = text[open..close]
        .split_whitespace()
        .filter_map(|number| number.parse::<f64>().ok());
    let (left, bottom, right, top) = (
        corners.next()?,
        corners.next()?,
        corners.next()?,
        corners.next()?,
    );
    let (width, height) = ((right - left).abs(), (top - bottom).abs());
    (width > 1.0 && height > 1.0).then_some((width, height))
}

/// What is known about a file before its preview is shaped, gathered on the main thread
/// from the listing and read on another, since it means reading headers on disk and
/// looking for the PDF tool.
pub(super) struct Probe {
    is_dir: bool,
    pub(super) content_type: String,
    pub(super) path: Option<PathBuf>,
    /// What names the thumbnail the cache may already hold, which has the proportions of
    /// the file. Looking for it is a read, so it waits for the worker with the rest.
    uri: String,
    mtime: u64,
    size: u64,
}

/// The shape a file wants: at its own size, raised to the floor; filling the room there is
/// in its own proportions; or one of the fixed shapes for content whose proportions are not
/// known until it is loaded.
pub(super) enum Shape {
    Fitted(f64, f64),
    Filled(f64, f64),
    Fixed((i32, i32)),
}

/// `width` by `height` scaled to fill `room`, up or down.
pub(super) fn filled(width: f64, height: f64, room: (i32, i32)) -> Option<(i32, i32)> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = (room.0 as f64 / width).min(room.1 as f64 / height);
    Some((
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    ))
}

/// `width` by `height` as it is, enlarged to `FLOOR_AREA` when it is smaller, and reduced
/// to fit `room` when it is larger than that.
pub(super) fn fitted(width: f64, height: f64, room: (i32, i32)) -> Option<(i32, i32)> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let floor = (FLOOR_AREA / (width * height)).sqrt().max(1.0);
    let scale = (room.0 as f64 / width)
        .min(room.1 as f64 / height)
        .min(floor);
    Some((
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    ))
}

impl Probe {
    /// What is known of `info`, whose content is in `file`.
    pub(super) fn of(info: &gio::FileInfo, file: &gio::File) -> Self {
        Self {
            is_dir: file_utils::is_dir(info),
            content_type: crate::file_utils::content_type_of(info)
                .unwrap_or_default()
                .to_string(),
            path: file.path(),
            uri: file.uri().to_string(),
            mtime: info
                .modification_date_time()
                .map(|d| d.to_unix() as u64)
                .unwrap_or(0),
            size: file_utils::size_of(info),
        }
    }

    /// The thumbnail the cache already holds, to stand in for the content while it loads:
    /// not for folders, sound or text, whose pages look nothing like one.
    pub(super) fn placeholder(&self) -> Option<gdk::Texture> {
        let content_type = self.content_type.as_str();
        if self.is_dir
            || content_type.starts_with("audio/")
            || gio::content_type_is_a(content_type, "text/plain")
        {
            return None;
        }
        let png = crate::thumbnails::cached_png(&self.uri, self.mtime)?;
        crate::thumbnails::picture_of(&png)
    }

    pub(super) fn shape(&self) -> Shape {
        let content_type = self.content_type.as_str();
        if self.is_dir {
            return Shape::Fixed(INFO_SHAPE);
        }
        if content_type.starts_with("image/") {
            // Reading the header of an image is a few bytes, not a decode. One too large to
            // decode shows its thumbnail, and so opens in the thumbnail's shape; so does one
            // whose header says nothing.
            let decoded = self
                .path
                .as_deref()
                .and_then(image_size)
                .filter(|&(width, height)| {
                    self.size <= IMAGE_LIMIT as u64 && width as i64 * height as i64 <= IMAGE_PIXELS
                })
                .map(|(width, height)| (width as f64, height as f64));
            return match decoded.or_else(|| self.thumbnail_size()) {
                Some((width, height)) => Shape::Fitted(width, height),
                None => Shape::Fixed(IMAGE_SHAPE),
            };
        }
        if content_type.starts_with("video/") {
            // The thumbnail has the proportions; failing that the container says them
            // in its header, and only a file with neither opens in the shape most video
            // has and moves once the stream reports.
            let (width, height) = self
                .thumbnail_size()
                .or_else(|| video_size(self.path.as_deref()?))
                .unwrap_or((16.0, 9.0));
            return Shape::Filled(width, height);
        }
        if content_type.starts_with("audio/") {
            return Shape::Fixed(SOUND_SHAPE);
        }
        if gio::content_type_is_a(content_type, "text/plain") {
            return Shape::Fixed(TEXT_SHAPE);
        }
        if content_type == "application/pdf" && can_render_pdf() {
            // The proportions come from the thumbnail, from the page dictionary, or from
            // A4, which is what most pages are; the size comes from the room there is. A
            // thumbnail is 256 pixels tall and a page dictionary is in points, so neither
            // is a size to show a page at.
            let (width, height) = self
                .thumbnail_size()
                .or_else(|| pdf_page_size(self.path.as_deref()?))
                .unwrap_or(PAGE_POINTS);
            return Shape::Filled(width, height);
        }
        match self.thumbnail_size() {
            // What will be shown is the thumbnail, shaped like any picture.
            Some((width, height)) => Shape::Fitted(width, height),
            None => Shape::Fixed(INFO_SHAPE),
        }
    }

    /// Reading the header of the thumbnail is a few bytes and no decode.
    pub(super) fn thumbnail_size(&self) -> Option<(f64, f64)> {
        let png = crate::thumbnails::cached_png(&self.uri, self.mtime)?;
        let (_, width, height) = gtk::gdk_pixbuf::Pixbuf::file_info(png)?;
        (width > 0 && height > 0).then_some((width as f64, height as f64))
    }
}

impl PreviewDialog {
    /// Give the dialog the shape a probe found for the file, before its content is loaded,
    /// so that it opens at its size instead of growing into it a moment later.
    pub(super) fn apply_shape(&self, shape: Shape) -> bool {
        match shape {
            Shape::Fitted(width, height) => self.shape_fitted(width, height),
            Shape::Filled(width, height) => self.shape_filled(width, height),
            Shape::Fixed((width, height)) => self.shape(width, height),
        }
    }

    /// Give the dialog the proportions of what it holds, within the bounds it may take.
    /// `width` and `height` are for the content itself; the header is added on top of them,
    /// so a picture asked for in its own proportions is drawn in them and not letterboxed.
    /// Whether the dialog changed size: a pixel or two either way is left alone, as the
    /// rounding of a thumbnail's proportions against the stream's.
    pub(super) fn shape(&self, width: i32, height: i32) -> bool {
        let header = self
            .imp()
            .header
            .measure(gtk::Orientation::Vertical, -1)
            .0
            .max(HEADER_HEIGHT);
        let (most_width, most_height) = self.imp().bounds.get();
        let (width, height) = (
            width.clamp(MIN_SIDE, most_width),
            height.clamp(MIN_SIDE, most_height),
        );
        let (was_width, was_height) = self.imp().shaped.get();
        if (width - was_width).abs() <= SHAPE_JITTER && (height - was_height).abs() <= SHAPE_JITTER
        {
            return false;
        }
        self.imp().shaped.set((width, height));
        glib::g_debug!("spiral", "preview: shaped {width}x{height}");
        self.set_content_width(width);
        self.set_content_height(height + header);
        true
    }

    pub(super) fn shape_to(&self, paintable: &impl IsA<gdk::Paintable>) {
        let paintable = paintable.as_ref();
        self.shape_fitted(
            paintable.intrinsic_width() as f64,
            paintable.intrinsic_height() as f64,
        );
    }

    /// `width` by `height` scaled to fill the room there is, up as well as down: what is
    /// shown at a size of its own choosing keeps its proportions but not its pixel count.
    pub(super) fn shape_filled(&self, width: f64, height: f64) -> bool {
        filled(width, height, self.imp().bounds.get())
            .is_some_and(|(width, height)| self.shape(width, height))
    }

    /// A `width` by `height` picture at its own size, within the room there is and not
    /// below the floor: a thumbnail should not open a window the size of a wall, nor a
    /// small photograph one the size of a stamp.
    pub(super) fn shape_fitted(&self, width: f64, height: f64) -> bool {
        fitted(width, height, self.imp().bounds.get())
            .is_some_and(|(width, height)| self.shape(width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG header with one EXIF directory holding the orientation tag.
    fn jpeg_with_orientation(big: bool, orientation: u16) -> Vec<u8> {
        let word = |n: u16| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let long = |n: u32| {
            if big {
                n.to_be_bytes()
            } else {
                n.to_le_bytes()
            }
        };
        let mut tiff = Vec::new();
        tiff.extend(if big { b"MM" } else { b"II" });
        tiff.extend(word(42));
        tiff.extend(long(8));
        tiff.extend(word(1));
        tiff.extend(word(0x0112));
        tiff.extend(word(3));
        tiff.extend(long(1));
        tiff.extend(word(orientation));
        tiff.extend(word(0));
        tiff.extend(long(0));
        let mut head = vec![0xff, 0xd8, 0xff, 0xe1];
        head.extend(((tiff.len() + 8) as u16).to_be_bytes());
        head.extend(b"Exif\0\0");
        head.extend(tiff);
        head
    }

    #[test]
    fn exif_orientation_is_read_either_way_round() {
        assert_eq!(exif_orientation(&jpeg_with_orientation(false, 6)), 6);
        assert_eq!(exif_orientation(&jpeg_with_orientation(true, 8)), 8);
        assert_eq!(exif_orientation(&jpeg_with_orientation(true, 1)), 1);
        assert_eq!(exif_orientation(b"\xff\xd8no exif here"), 1);
        assert_eq!(exif_orientation(b"Exif\0\0MM"), 1);
        // A HEIF photograph carries the tag, and is turned by its own boxes instead.
        let mut heif = b"\0\0\0\x18ftypheic".to_vec();
        heif.extend(&jpeg_with_orientation(true, 6)[4..]);
        assert_eq!(exif_orientation(&heif), 1);
    }

    #[test]
    fn a_picture_opens_at_its_own_size_between_the_floor_and_the_room() {
        let room = (1148, 656);
        // Small: raised to the floor, in its own proportions.
        assert_eq!(fitted(320.0, 240.0, room), Some((560, 420)));
        assert_eq!(fitted(48.0, 48.0, room), Some((485, 485)));
        // In between: as it is.
        assert_eq!(fitted(800.0, 600.0, room), Some((800, 600)));
        // Large: reduced to the room.
        assert_eq!(fitted(1920.0, 1080.0, room), Some((1148, 646)));
        // The floor never takes it past the room.
        assert_eq!(fitted(100.0, 400.0, (400, 300)), Some((75, 300)));
        assert_eq!(fitted(0.0, 10.0, room), None);
        // Video and pages fill the room whatever their size.
        assert_eq!(filled(320.0, 240.0, room), Some((875, 656)));
        assert_eq!(filled(720.0, 1280.0, room), Some((369, 656)));
    }

    #[test]
    fn media_box_is_the_size_of_the_page() {
        assert_eq!(
            media_box("<< /Type /Page /MediaBox [0 0 612 792] >>"),
            Some((612.0, 792.0))
        );
        assert_eq!(
            media_box("/MediaBox [ 0.0 0.0 595.28 841.89 ]"),
            Some((595.28, 841.89))
        );
        assert_eq!(media_box("/MediaBox [0 0 0 0]"), None);
        assert_eq!(media_box("%PDF-1.5 nothing readable"), None);
    }

    /// An MP4 box: size, type, body.
    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    /// A version 0 track header of `width` by `height` with the given matrix corner.
    fn tkhd(width: u32, height: u32, matrix: [i32; 4]) -> Vec<u8> {
        let mut body = vec![0u8; 4 + 20 + 8 + 2 + 2 + 2 + 2];
        let [a, b, c, d] = matrix;
        for value in [a, b, 0, c, d, 0, 0, 0, 1 << 30] {
            body.extend_from_slice(&value.to_be_bytes());
        }
        body.extend_from_slice(&(width << 16).to_be_bytes());
        body.extend_from_slice(&(height << 16).to_be_bytes());
        mp4_box(b"tkhd", &body)
    }

    #[test]
    fn an_mp4_says_its_size_from_the_track_header_wherever_moov_sits() {
        let upright = [1 << 16, 0, 0, 1 << 16];
        let quarter_turn = [0, 1 << 16, -(1 << 16), 0];
        let sound = mp4_box(b"trak", &tkhd(0, 0, upright));
        let picture = mp4_box(b"trak", &tkhd(640, 360, quarter_turn));
        let moov = mp4_box(b"moov", &[sound, picture].concat());
        // Media first, as a phone writes it, with the header at the end.
        let mut file = mp4_box(b"ftyp", b"isom");
        file.extend(mp4_box(b"mdat", &[0u8; 1000]));
        file.extend(&moov);
        assert_eq!(
            mp4_size(&mut std::io::Cursor::new(file)),
            Some((360.0, 640.0))
        );
        let mut plain = mp4_box(b"ftyp", b"isom");
        plain.extend(mp4_box(
            b"moov",
            &mp4_box(b"trak", &tkhd(1920, 1080, upright)),
        ));
        assert_eq!(
            mp4_size(&mut std::io::Cursor::new(plain)),
            Some((1920.0, 1080.0))
        );
        assert_eq!(mp4_size(&mut std::io::Cursor::new(b"ftyp".to_vec())), None);
    }

    /// An EBML element with a one-byte size.
    fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
        assert!(body.len() < 127);
        let mut out = id.to_vec();
        out.push(0x80 | body.len() as u8);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_matroska_file_says_the_size_its_video_track_is_shown_at() {
        let video = [
            ebml(&[0xB0], &[0x02, 0x80]),       // stored 640
            ebml(&[0xBA], &[0x01, 0x68]),       // by 360
            ebml(&[0x54, 0xB0], &[0x03, 0x20]), // shown at 800
            ebml(&[0x54, 0xBA], &[0x01, 0x68]), // by 360
        ]
        .concat();
        let sound = ebml(&[0xAE], &ebml(&[0x83], &[2]));
        let picture = ebml(
            &[0xAE],
            &[ebml(&[0x83], &[1]), ebml(&[0xE0], &video)].concat(),
        );
        let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &[sound, picture].concat());
        // A segment of unknown size, as a live muxer leaves it, holding a void element,
        // the tracks and then a cluster.
        let mut segment = vec![
            0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ];
        segment.extend(ebml(&[0xEC], &[0; 5]));
        segment.extend(&tracks);
        segment.extend(ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0; 3]));
        let mut file = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &ebml(&[0x42, 0x82], b"webm"));
        file.extend(&segment);
        assert_eq!(matroska_size(&file), Some((800.0, 360.0)));
        assert_eq!(matroska_size(&file[..20]), None);
    }
}
