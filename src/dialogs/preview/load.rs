//! Reading what the viewers show, away from the main loop: pictures, covers, text, and PDF
//! pages drawn in the sandbox.

use super::*;

/// Decoding happens off the main loop for local files, where a large photograph would
/// otherwise freeze the window; anything else is small enough to read whole.
pub(super) async fn load_texture(file: &gio::File) -> Option<gdk::Texture> {
    match file.path() {
        Some(path) => gio::spawn_blocking(move || {
            if image_size(&path).is_some_and(|(w, h)| w as i64 * h as i64 > IMAGE_PIXELS) {
                return None;
            }
            // GTK's own loaders leave a photograph the way the camera held it; the EXIF
            // tag that says to turn it is honoured by the pixbuf loader alone.
            if exif_turned(&path) {
                let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file(&path)
                    .ok()?
                    .apply_embedded_orientation()?;
                return Some(texture_of(&pixbuf));
            }
            gdk::Texture::from_filename(path).ok()
        })
        .await
        .ok()
        .flatten(),
        None => {
            let (data, _) = file.load_bytes_future().await.ok()?;
            // Decoding is the slow part, and a picture from a share is as big as one from
            // the disk: it belongs on a worker, like the local path above.
            gio::spawn_blocking(move || gdk::Texture::from_bytes(&data).ok())
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
pub(super) async fn cover_texture(bytes: glib::Bytes, side: i32) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || {
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&bytes).ok()?;
        loader.close().ok()?;
        let pixbuf = loader.pixbuf()?;
        let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
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
    run_tool(
        "pdfinfo",
        path,
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
        run_tool(
            "pdftoppm",
            &path,
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
            |_, work| gdk::Texture::from_filename(work.join("page.png")).ok(),
        )
    })
    .await
    .ok()
    .flatten()
}

/// Run `program` over `input` the way a thumbnailer runs: inside bubblewrap, with the file
/// bound read-only and one private directory to write into. `args` is handed the paths as
/// the child sees them, `result` what it printed and the directory it wrote to, which is
/// removed as soon as `result` returns. Without bubblewrap the tool is not run: it is fed
/// a document from wherever the reader got it.
pub(super) fn run_tool<T>(
    program: &str,
    input: &Path,
    args: impl FnOnce(&Path, &Path) -> Vec<OsString>,
    result: impl FnOnce(&[u8], &Path) -> Option<T>,
) -> Option<T> {
    let program = glib::find_program_in_path(program)?;
    let work = std::env::temp_dir().join(format!(
        "spiral-preview-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // Not create_dir_all: a name another process got to first is refused, not adopted.
    std::fs::create_dir(&work).ok()?;
    let sandbox = match crate::sandbox::command(&program.to_string_lossy()) {
        Some(sandbox) => sandbox,
        None => {
            let _ = std::fs::remove_dir(&work);
            return None;
        }
    };
    let (input_seen, work_seen) = (Path::new("/tmp/in"), Path::new("/tmp/out"));
    let argv = args(input_seen, work_seen);
    let mut cmd = std::process::Command::new(&sandbox.argv[0]);
    cmd.args(&sandbox.argv[1..]);
    // Drawing a page needs the fonts the document does not carry itself.
    let font_cache = glib::user_cache_dir().join("fontconfig");
    cmd.args(["--ro-bind-try", "/etc/fonts", "/etc/fonts"]);
    cmd.args([
        "--ro-bind-try",
        "/var/cache/fontconfig",
        "/var/cache/fontconfig",
    ]);
    cmd.arg("--ro-bind-try").arg(&font_cache).arg(&font_cache);
    cmd.arg("--ro-bind").arg(input).arg(input_seen);
    cmd.arg("--bind").arg(&work).arg(work_seen);
    cmd.arg("--").arg(&program);
    cmd.args(&argv);
    // Bounded like a thumbnailer: a document that stops the tool would otherwise leave
    // the preview on its spinner and the worker thread on the tool, for good.
    let run = crate::sandbox::run_bounded(&mut cmd, TOOL_TIMEOUT);
    // The seccomp memfd must stay open until the child has started.
    drop(sandbox.seccomp);
    let out = match run {
        Ok(ran) if ran.ok => result(&ran.stdout, &work),
        Ok(ran) => {
            glib::g_debug!("spiral", "preview {program:?} failed: {}", ran.trouble);
            None
        }
        Err(e) => {
            glib::g_debug!("spiral", "preview {program:?} could not start: {e}");
            None
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    out
}
