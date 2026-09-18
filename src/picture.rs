//! Pictures, decoded anywhere but in this process. Where glycin is installed and its
//! sandbox starts, it decodes them (see [`crate::glycin`]); elsewhere `spiral-thumbnailer
//! --decode` does, in Spiral's own sandbox, with GTK's decoders and gdk-pixbuf's, and hands
//! back the pixels as they came out: their depth and their colour space, which a PNG
//! encoded again would not carry.
//!
//! What the helper hands back is a small header and the rows:
//!
//! ```text
//! "SPIRLPIX"  width u32  height u32  stride u32  format u32
//! colour primaries u8  transfer function u8  matrix coefficients u8  full range u8
//! rows: stride × height bytes
//! ```
//!
//! all in little-endian, the colour space as the CICP numbers of H.273.

use std::path::Path;

use gtk::prelude::*;

use crate::{gdk, gio, glib, gtk};

const MAGIC: &[u8; 8] = b"SPIRLPIX";
const HEADER: usize = 8 + 4 * 4 + 4;

/// The formats the rows travel in, and the bytes a pixel takes in each.
const FORMATS: [(gdk::MemoryFormat, usize); 3] = [
    (gdk::MemoryFormat::R8g8b8a8, 4),
    (gdk::MemoryFormat::R16g16b16a16, 8),
    (gdk::MemoryFormat::R16g16b16a16Float, 8),
];

/// How much of a file is read looking for its EXIF orientation.
pub(crate) const EXIF_SCAN: usize = 64 * 1024;

/// The picture at `path`, of at most `max_pixels`, turned the way it says, or `None` where
/// it cannot be decoded. Blocks: for a worker thread.
pub(crate) fn load_file(path: &Path, max_pixels: i64) -> Option<gdk::Texture> {
    match crate::glycin::get() {
        Some(glycin) => glycin
            .load_file(&gio::File::for_path(path), max_pixels, None)
            .inspect_err(|e| glib::g_debug!("spiral", "glycin: {}: {e:?}", path.display()))
            .ok(),
        None => sandboxed(path, orientation_of(path), max_pixels),
    }
}

/// As [`load_file`], for a picture that is not in a file of its own.
pub(crate) fn load_bytes(bytes: &glib::Bytes, max_pixels: i64) -> Option<gdk::Texture> {
    if let Some(glycin) = crate::glycin::get() {
        return glycin
            .load_bytes(bytes, max_pixels, None)
            .inspect_err(|e| glib::g_debug!("spiral", "glycin: {e:?}"))
            .ok();
    }
    // The sandbox takes a file, so the helper gets a private copy.
    let dir = crate::sandbox::private_dir("spiral-picture")?;
    let copy = dir.join("picture");
    let texture = std::fs::write(&copy, bytes)
        .ok()
        .and_then(|()| sandboxed(&copy, exif_orientation(bytes), max_pixels));
    let _ = std::fs::remove_dir_all(&dir);
    texture
}

/// The picture at `path` decoded by the helper in Spiral's sandbox, and turned there.
fn sandboxed(path: &Path, orientation: u16, max_pixels: i64) -> Option<gdk::Texture> {
    let helper = crate::thumbnails::own_thumbnailer()?;
    // A picture at the limit, eight bytes a pixel at most, and its header.
    let limit = max_pixels as u64 * 8 + HEADER as u64;
    crate::sandbox::run_tool(
        &helper.to_string_lossy(),
        path,
        crate::thumbnails::TIMEOUT,
        |input, work| {
            vec![
                "--decode".into(),
                input.as_os_str().to_owned(),
                work.join("picture").into_os_string(),
                orientation.to_string().into(),
            ]
        },
        |_, work| {
            texture(
                crate::sandbox::read_output(&work.join("picture"), limit)?,
                max_pixels,
            )
        },
    )
}

/// The EXIF orientation of the picture at `path`, 1 (upright) where it has none.
fn orientation_of(path: &Path) -> u16 {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(EXIF_SCAN as u64).read_to_end(&mut head))
        .map_or(1, |_| exif_orientation(&head))
}

/// The EXIF orientation in `head`, 1 (upright) where there is none: the APP1 segment
/// holds a TIFF header, and the first directory of that holds the tag.
pub(crate) fn exif_orientation(head: &[u8]) -> u16 {
    fn read(head: &[u8], at: usize, big: bool, len: usize) -> Option<u32> {
        let bytes = head.get(at..at + len)?;
        let value = bytes.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b));
        Some(if big {
            value
        } else {
            value.swap_bytes() >> (32 - 8 * len as u32)
        })
    }
    let orientation = || {
        let tiff = head.windows(6).position(|w| w == b"Exif\0\0")? + 6;
        let big = match head.get(tiff..tiff + 2)? {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        let directory = tiff + read(head, tiff + 4, big, 4)? as usize;
        let entries = read(head, directory, big, 2)?;
        (0..entries as usize)
            .map(|n| directory + 2 + n * 12)
            .find(|&entry| read(head, entry, big, 2) == Some(0x0112))
            .and_then(|entry| read(head, entry + 8, big, 2))
            .map(|value| value as u16)
    };
    orientation().unwrap_or(1)
}

/// `texture` no larger than `side` either way, in eight bits a channel, as thumbnails and
/// icons are.
pub(crate) fn shrunk(texture: &gdk::Texture, side: i32) -> Option<gdk::Texture> {
    let (width, height) = (texture.width(), texture.height());
    let scale = (f64::from(side) / f64::from(width.max(height))).min(1.0);
    let size = |n: i32| ((f64::from(n) * scale).round() as i32).max(1);
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (pixels, stride) = downloader.download_bytes();
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
        &pixels,
        gtk::gdk_pixbuf::Colorspace::Rgb,
        true,
        8,
        width,
        height,
        stride as i32,
    )
    .scale_simple(
        size(width),
        size(height),
        gtk::gdk_pixbuf::InterpType::Bilinear,
    )?;
    Some(
        gdk::MemoryTexture::new(
            pixbuf.width(),
            pixbuf.height(),
            gdk::MemoryFormat::R8g8b8a8,
            &pixbuf.read_pixel_bytes(),
            pixbuf.rowstride() as usize,
        )
        .upcast(),
    )
}

/// In the helper: decode `input`, turn it the way EXIF `orientation` says, and write the
/// rows to `output`.
pub fn decode(input: &Path, output: &Path, orientation: u16) -> Result<(), String> {
    let texture = gdk::Texture::from_filename(input).map_err(|e| e.to_string())?;
    let (format, bpp) = FORMATS[match texture.format() {
        gdk::MemoryFormat::G16
        | gdk::MemoryFormat::G16a16
        | gdk::MemoryFormat::G16a16Premultiplied
        | gdk::MemoryFormat::A16
        | gdk::MemoryFormat::R16g16b16
        | gdk::MemoryFormat::R16g16b16a16
        | gdk::MemoryFormat::R16g16b16a16Premultiplied => 1,
        gdk::MemoryFormat::A16Float
        | gdk::MemoryFormat::A32Float
        | gdk::MemoryFormat::R16g16b16Float
        | gdk::MemoryFormat::R16g16b16a16Float
        | gdk::MemoryFormat::R16g16b16a16FloatPremultiplied
        | gdk::MemoryFormat::R32g32b32Float
        | gdk::MemoryFormat::R32g32b32a32Float
        | gdk::MemoryFormat::R32g32b32a32FloatPremultiplied => 2,
        _ => 0,
    }];
    // A colour space without CICP numbers is carried as sRGB, converted on the way out.
    let (color_state, cicp) = match texture.color_state().create_cicp_params() {
        Some(cicp) => (texture.color_state(), cicp),
        None => {
            let srgb = gdk::ColorState::srgb();
            let cicp = srgb
                .create_cicp_params()
                .ok_or("sRGB has no CICP numbers")?;
            (srgb, cicp)
        }
    };
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(format);
    downloader.set_color_state(&color_state);
    let (rows, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let (rows, width, height, stride) = turn(&rows, width, height, stride, bpp, orientation);

    let mut out = Vec::with_capacity(HEADER + rows.len());
    out.extend_from_slice(MAGIC);
    for n in [width, height, stride] {
        out.extend_from_slice(&u32::try_from(n).map_err(|e| e.to_string())?.to_le_bytes());
    }
    let index = FORMATS.iter().position(|(f, _)| *f == format).unwrap_or(0);
    out.extend_from_slice(&(index as u32).to_le_bytes());
    for n in [
        cicp.color_primaries(),
        cicp.transfer_function(),
        cicp.matrix_coefficients(),
    ] {
        out.push(u8::try_from(n).map_err(|e| e.to_string())?);
    }
    out.push(u8::from(cicp.range() == gdk::CicpRange::Full));
    out.extend_from_slice(&rows);
    std::fs::write(output, out).map_err(|e| e.to_string())
}

/// `rows` of a picture turned the way EXIF `orientation` says, with its new width, height
/// and stride; as it was for 1 or a value EXIF does not have.
fn turn(
    rows: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    bpp: usize,
    orientation: u16,
) -> (Vec<u8>, usize, usize, usize) {
    if !(2..=8).contains(&orientation) {
        return (rows.to_vec(), width, height, stride);
    }
    let across = orientation >= 5;
    let (w, h) = if across {
        (height, width)
    } else {
        (width, height)
    };
    let mut out = vec![0u8; w * h * bpp];
    for y in 0..h {
        for x in 0..w {
            // Where in the picture as stored this pixel of the turned one comes from.
            let (sx, sy) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (y, height - 1 - x),
                7 => (width - 1 - y, height - 1 - x),
                _ => (width - 1 - y, x),
            };
            let from = sy * stride + sx * bpp;
            let to = (y * w + x) * bpp;
            out[to..to + bpp].copy_from_slice(&rows[from..from + bpp]);
        }
    }
    (out, w, h, w * bpp)
}

/// In the file manager: the texture `bytes`, written by [`decode`] in the sandbox, stand
/// for. None unless every number in the header agrees with the rest, since the helper
/// that wrote them may have been taken over by the file it read.
pub(crate) fn texture(bytes: Vec<u8>, max_pixels: i64) -> Option<gdk::Texture> {
    if bytes.len() < HEADER || &bytes[..8] != MAGIC {
        return None;
    }
    let number = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let (width, height, stride) = (number(8), number(12), number(16));
    let (format, bpp) = *FORMATS.get(number(20))?;
    let pixels = width.checked_mul(height)?;
    if pixels == 0 || pixels as u64 > max_pixels as u64 {
        return None;
    }
    let size = stride.checked_mul(height)?;
    if stride < width.checked_mul(bpp)? || bytes.len() != HEADER.checked_add(size)? {
        return None;
    }
    let cicp = gdk::CicpParams::new();
    cicp.set_color_primaries(bytes[24].into());
    cicp.set_transfer_function(bytes[25].into());
    cicp.set_matrix_coefficients(bytes[26].into());
    cicp.set_range(if bytes[27] != 0 {
        gdk::CicpRange::Full
    } else {
        gdk::CicpRange::Narrow
    });
    let color_state = cicp.build_color_state().ok()?;
    let all = glib::Bytes::from_owned(bytes);
    let rows = glib::Bytes::from_bytes(&all, HEADER..);
    Some(
        gdk::MemoryTextureBuilder::new()
            .set_bytes(Some(&rows))
            .set_width(width as i32)
            .set_height(height as i32)
            .set_stride(stride)
            .set_format(format)
            .set_color_state(&color_state)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each orientation puts the corner that was top left where EXIF says it belongs.
    #[test]
    fn orientations_turn_the_picture() {
        // 3 wide, 2 high, one byte a pixel:  a b c / d e f
        let rows = b"abcdef";
        let turned = |o| {
            let (out, w, h, _) = turn(rows, 3, 2, 3, 1, o);
            (String::from_utf8(out).unwrap(), w, h)
        };
        assert_eq!(turned(1), ("abcdef".into(), 3, 2));
        assert_eq!(turned(2), ("cbafed".into(), 3, 2));
        assert_eq!(turned(3), ("fedcba".into(), 3, 2));
        assert_eq!(turned(4), ("defabc".into(), 3, 2));
        assert_eq!(turned(5), ("adbecf".into(), 2, 3));
        assert_eq!(turned(6), ("daebfc".into(), 2, 3));
        assert_eq!(turned(7), ("fcebda".into(), 2, 3));
        assert_eq!(turned(8), ("cfbead".into(), 2, 3));
    }

    /// Rows that do not add up to what the header says are refused.
    #[test]
    fn a_header_that_does_not_agree_is_refused() {
        let mut bytes = MAGIC.to_vec();
        for n in [2u32, 2, 8, 0] {
            bytes.extend_from_slice(&n.to_le_bytes());
        }
        bytes.extend_from_slice(&[1, 13, 0, 1]);
        bytes.extend_from_slice(&[0x80; 16]);
        assert!(texture(bytes.clone(), 100).is_some());
        assert!(
            texture(bytes.clone(), 3).is_none(),
            "more pixels than allowed"
        );
        let mut short = bytes.clone();
        short.pop();
        assert!(texture(short, 100).is_none());
        let mut narrow = bytes.clone();
        narrow[16] = 4; // stride under width × 4
        assert!(texture(narrow, 100).is_none());
        let mut format = bytes.clone();
        format[20] = 9;
        assert!(texture(format, 100).is_none());
        let mut huge = bytes;
        huge[8..16].copy_from_slice(&[0xff; 8]); // width × height past any integer
        assert!(texture(huge, i64::MAX).is_none());
    }
}
