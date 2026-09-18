//! Reading what the viewers show, away from the main loop: pictures, covers, text, and PDF
//! pages drawn in the sandbox.

use super::*;

/// Decoding happens off the main loop, and never in this process: see [`crate::picture`].
pub(super) async fn load_texture(file: &gio::File) -> Option<gdk::Texture> {
    match file.path() {
        Some(path) => gio::spawn_blocking(move || {
            if image_size(&path).is_some_and(|(w, h)| w as i64 * h as i64 > IMAGE_PIXELS) {
                return None;
            }
            crate::picture::load_file(&path, IMAGE_PIXELS)
        })
        .await
        .ok()
        .flatten(),
        None => {
            let (data, _) = file.load_bytes_future().await.ok()?;
            // Decoding is the slow part, and a picture from a share is as big as one from
            // the disk: it belongs on a worker, like the local path above.
            gio::spawn_blocking(move || crate::picture::load_bytes(&data, IMAGE_PIXELS))
                .await
                .ok()
                .flatten()
        }
    }
}

pub(super) fn texture_of(pixbuf: &gtk::gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    )
    .upcast()
}

/// The picture a sound file carries, cut to a square `side` pixels across: the middle of
/// it, since a cover is nearly square and the corners are what gets rounded off.
/// The cover comes as the file holds it, and is decoded like any picture the preview
/// shows.
pub(super) async fn cover_texture(bytes: glib::Bytes, side: i32) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || {
        let texture = crate::picture::load_bytes(&bytes, IMAGE_PIXELS)?;
        let mut downloader = gdk::TextureDownloader::new(&texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (pixels, stride) = downloader.download_bytes();
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
            &pixels,
            gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            texture.width(),
            texture.height(),
            stride as i32,
        );
        let (width, height) = (pixbuf.width(), pixbuf.height());
        let square = width.min(height);
        if square <= 0 {
            return None;
        }
        let middle =
            pixbuf.new_subpixbuf((width - square) / 2, (height - square) / 2, square, square);
        let scaled = middle.scale_simple(side, side, gtk::gdk_pixbuf::InterpType::Bilinear)?;
        Some(texture_of(&scaled))
    })
    .await
    .ok()
    .flatten()
}

/// The first `TEXT_LIMIT` bytes of `file`. Bytes that are not UTF-8 are replaced rather
/// than refused, so a file that only claims to be text still shows what it holds.
pub(super) async fn load_text(file: &gio::File) -> Option<String> {
    let stream = file.read_future(PRIO).await.ok()?;
    let mut data: Vec<u8> = Vec::new();
    while data.len() < TEXT_LIMIT {
        let read = stream
            .read_bytes_future(TEXT_LIMIT - data.len(), PRIO)
            .await
            .ok()?;
        if read.is_empty() {
            break;
        }
        data.extend_from_slice(&read);
    }
    let _ = stream.close_future(PRIO).await;
    Some(String::from_utf8_lossy(&data).into_owned())
}

/// How many pages the document has and how large one is, in points.
pub(super) async fn pdf_info(path: PathBuf) -> Option<(u32, (f64, f64))> {
    gio::spawn_blocking(move || pdf_facts(&path))
        .await
        .ok()
        .flatten()
}

pub(super) fn pdf_facts(path: &Path) -> Option<(u32, (f64, f64))> {
    crate::sandbox::run_tool(
        "pdfinfo",
        path,
        TOOL_TIMEOUT,
        |input, _| vec![input.as_os_str().to_owned()],
        |out, _| {
            let text = String::from_utf8_lossy(out);
            let pages: u32 = text
                .lines()
                .find_map(|line| line.strip_prefix("Pages:"))
                .and_then(|count| count.trim().parse().ok())?;
            // "Page size:       595.2 x 841.92 pts (A4)"
            let (width, height) = text
                .lines()
                .find_map(|line| line.strip_prefix("Page size:"))
                .and_then(|size| size.trim().split_once(" x "))
                .and_then(|(width, height)| {
                    let height = height.split_whitespace().next()?;
                    Some((
                        width.trim().parse::<f64>().ok()?,
                        height.parse::<f64>().ok()?,
                    ))
                })
                .unwrap_or(PAGE_POINTS);
            // "Page rot:        90": the page is drawn turned, so it is shown turned.
            let turned = text
                .lines()
                .find_map(|line| line.strip_prefix("Page rot:"))
                .and_then(|rot| rot.trim().parse::<i32>().ok())
                .is_some_and(|rot| rot.rem_euclid(180) == 90);
            let size = if turned {
                (height, width)
            } else {
                (width, height)
            };
            Some((pages, size))
        },
    )
}

pub(super) async fn pdf_page(path: PathBuf, page: u32) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || {
        crate::sandbox::run_tool(
            "pdftoppm",
            &path,
            TOOL_TIMEOUT,
            |input, work| {
                let page = page.to_string();
                vec![
                    "-png".into(),
                    "-singlefile".into(),
                    "-r".into(),
                    PDF_DPI.to_string().into(),
                    "-f".into(),
                    page.clone().into(),
                    "-l".into(),
                    page.into(),
                    input.as_os_str().to_owned(),
                    work.join("page").into_os_string(),
                ]
            },
            |_, work| {
                // Drawn by a tool the document could have taken over, so decoded like
                // any other picture.
                let png = crate::sandbox::read_output(&work.join("page.png"), OUTPUT_LIMIT)
                    .filter(|png| png.starts_with(crate::thumbnails::PNG_SIGNATURE))?;
                crate::picture::load_bytes(&glib::Bytes::from_owned(png), IMAGE_PIXELS)
            },
        )
    })
    .await
    .ok()
    .flatten()
}
