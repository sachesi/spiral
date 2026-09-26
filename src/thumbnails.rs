//! Freedesktop thumbnails: reuse `~/.cache/thumbnails`, otherwise generate via glycin for
//! the pictures it reads, then the system `.thumbnailer` entries (or the bundled gdk-pixbuf
//! helper for images without one), with bounded concurrency. Glycin decodes in a sandbox
//! of its own, and every thumbnailer runs inside bubblewrap, which is required: a decoder
//! is fed files from anywhere and there is no unconfined path for it to take. A thumbnail
//! found in the cache is decoded the same way, since a thumbnailer taken over by the file
//! it read could have left anything there.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_channel::oneshot;
use gtk::prelude::*;

use crate::{gdk, gio, glib, gtk};

const SIZE: i32 = 256;

/// The eight bytes every PNG starts with.
pub(crate) const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Largest thumbnail taken from a thumbnailer. One at the size asked for is a few hundred
/// kilobytes; some draw larger than asked, none this large.
const OUTPUT_LIMIT: u64 = 32 << 20;
/// Largest thumbnail decoded from the cache; see [`OUTPUT_LIMIT`].
const THUMBNAIL_PIXELS: i64 = 4096 * 4096;
/// Largest picture glycin is asked to make a thumbnail of.
const SOURCE_PIXELS: i64 = 150_000_000;

/// Thumbnailers to run at once. One core is left to the interface, which is drawing the
/// rows they are for; a folder of thousands of pictures is otherwise limited by a number
/// picked for the machines of a decade ago.
fn max_parallel() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(2, 8))
        .unwrap_or(4)
}

/// How long one thumbnailer may take before it is killed.
pub(crate) const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Decoded thumbnails kept in memory, by count and by bytes. Scrolling a folder of
/// thousands of pictures otherwise either throws the cache away wholesale or grows it
/// until it is a second copy of the folder.
const CACHE_ENTRIES: usize = 2048;
const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// URI, modification time and size. The size belongs in the key because a file being
/// written is seen empty first, and a verdict of "no thumbnail" taken from that snapshot
/// would otherwise outlive the write whenever both land in the same second.
type Key = (String, u64, u64);

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
    /// Loads in progress; later requests for the same key wait for the first one.
    static PENDING: RefCell<HashMap<Key, Vec<oneshot::Sender<Option<gdk::Texture>>>>> = RefCell::new(HashMap::new());
    static RUNNING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Requests waiting for a slot, with the row each is for. Never more than a screen or
    /// two of them, so the one to serve next is found by looking at all of them.
    static WAITERS: RefCell<Vec<(u32, oneshot::Sender<()>)>> = const { RefCell::new(Vec::new()) };
}

static SEQ: AtomicU64 = AtomicU64::new(0);
static THUMBNAILERS: OnceLock<Vec<Thumbnailer>> = OnceLock::new();

/// What has been loaded, oldest first. Answers stay until the room they take is wanted by
/// newer ones; a folder walked through end to end must not cost the memory of every
/// picture in it, and must not lose the last screen either.
#[derive(Default)]
struct Cache {
    seen: HashMap<Key, Option<gdk::Texture>>,
    order: std::collections::VecDeque<Key>,
    bytes: usize,
}

fn texture_bytes(texture: &Option<gdk::Texture>) -> usize {
    texture.as_ref().map_or(0, |t| {
        t.width().max(0) as usize * t.height().max(0) as usize * 4
    })
}

impl Cache {
    fn insert(&mut self, key: Key, texture: Option<gdk::Texture>) {
        if let Some(old) = self.seen.insert(key.clone(), texture.clone()) {
            self.bytes -= texture_bytes(&old);
        } else {
            self.order.push_back(key);
        }
        self.bytes += texture_bytes(&texture);
        while self.order.len() > CACHE_ENTRIES || self.bytes > CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.seen.remove(&oldest) {
                self.bytes -= texture_bytes(&gone);
            }
        }
    }
}

struct Thumbnailer {
    exec: String,
    mime_types: Vec<String>,
}

/// Thumbnail for `info`, or `None` if the type has no thumbnailer or generation failed.
/// `at` is the row it is for, counted from the top of the folder: where several are
/// waiting, the one nearest the top of what is on screen is made first, so a screenful
/// fills in the order it is read rather than in the order the rows happened to be bound.
pub async fn load(info: &gio::FileInfo, at: u32) -> Option<gdk::Texture> {
    if info.file_type() == gio::FileType::Directory {
        return None;
    }
    // The thumbnail of an item in the trash is that of the file on the disk, allowed and
    // made as for any other local file.
    let file = on_disk(info).await;
    let uri = file.uri().to_string();
    // Checked before the cache so a preference change takes effect on the next reload.
    if !crate::prefs::thumbnails_for(&file) {
        glib::g_debug!(
            "spiral",
            "thumbnail {uri}: off by preference for this location"
        );
        return None;
    }
    let mtime = info
        .modification_date_time()
        .map(|d| d.to_unix() as u64)
        .unwrap_or(0);
    let key = (uri.clone(), mtime, crate::file_utils::size_of(info));
    if let Some(cached) = CACHE.with(|c| c.borrow().seen.get(&key).cloned()) {
        return cached;
    }

    // Neither a type nor a local path stops the lookup: a file on a share may still have
    // a thumbnail in the cache, made when it was somewhere else or by something else.
    let content_type = crate::file_utils::content_type_of(info)
        .unwrap_or_default()
        .to_string();
    let source = Source {
        path: file.path(),
        content_type: content_type.clone(),
        // Images with no thumbnailer of their own go to the bundled helper.
        own: content_type.starts_with("image/"),
        // Only pictures are weighed: a video thumbnailer reads a frame, not the file, so
        // the size of the file says nothing about what it will cost. One already in the
        // cache is shown whatever the size, which is why this only stops generation.
        too_large: content_type.starts_with("image/")
            && crate::file_utils::size_of(info) > crate::prefs::thumbnail_limit(),
    };
    // Every caller waits on a detached generation task, so a row being unbound mid-way
    // (scrolling) drops only its receiver and can never strand a concurrency slot.
    let (tx, rx) = oneshot::channel();
    let first = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        match p.get_mut(&key) {
            Some(list) => {
                list.push(tx);
                false
            }
            None => {
                p.insert(key.clone(), vec![tx]);
                true
            }
        }
    });
    if first {
        glib::g_debug!("spiral", "thumbnail {uri}: asked for row {at}");
        glib::spawn_future_local(generate_task(key, source, at));
    }
    rx.await.ok().flatten()
}

/// The file of `info` on the disk: an item in the trash is a local file under another
/// name, and what is read of it is read from that file.
pub(crate) async fn on_disk(info: &gio::FileInfo) -> gio::File {
    let file = crate::file_utils::file_of(info);
    if !file.has_uri_scheme("trash") {
        return file;
    }
    match info.attribute_string("standard::target-uri") {
        Some(uri) => Some(gio::File::for_uri(&uri)),
        None => in_trashed_folder(&file).await,
    }
    .filter(|target| target.is_native())
    .unwrap_or(file)
}

/// Where a file inside a folder in the trash is on the disk. The trash says so only for
/// what is at its top, so the way there is that folder's, asked once per folder.
async fn in_trashed_folder(file: &gio::File) -> Option<gio::File> {
    thread_local! {
        static TARGETS: RefCell<HashMap<glib::GString, gio::File>> = RefCell::new(HashMap::new());
    }
    let mut top = file.parent().filter(|p| p.parent().is_some())?;
    while let Some(parent) = top.parent().filter(|p| p.parent().is_some()) {
        top = parent;
    }
    let rest = top.relative_path(file)?;
    let key = top.uri();
    let target = match TARGETS.with(|t| t.borrow().get(&key).cloned()) {
        Some(target) => target,
        None => {
            let uri = top
                .query_info_future(
                    "standard::target-uri",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
                .ok()?
                .attribute_string("standard::target-uri")?;
            let target = gio::File::for_uri(&uri);
            TARGETS.with(|t| t.borrow_mut().insert(key, target.clone()));
            target
        }
    };
    Some(target.resolve_relative_path(rest))
}

/// What a thumbnail would be made from, if the cache has none.
struct Source {
    /// Only a local file can be handed to a thumbnailer.
    path: Option<PathBuf>,
    content_type: String,
    /// Whether the bundled helper would take it: images, which most thumbnailer entries
    /// leave alone.
    own: bool,
    /// Whether the file is too big to be worth decoding.
    too_large: bool,
}

async fn generate_task(key: Key, source: Source, at: u32) {
    let (uri, mtime, size) = (key.0.clone(), key.1, key.2);
    // Asking the cache is a handful of stats, so it is asked before queueing for a
    // generation slot: a thumbnail that is already on disk must not wait behind a video
    // being decoded. Only what has to be made, and the decoding of what is found, waits.
    let found = {
        let uri = uri.clone();
        gio::spawn_blocking(move || cached_thumbnail(&uri, mtime))
            .await
            .unwrap_or(Cached::Missing)
    };
    if matches!(found, Cached::Failed) {
        glib::g_debug!(
            "spiral",
            "thumbnail {uri}: noted as failed before, not tried"
        );
        finish(key, None);
        return;
    }
    acquire(at).await;
    // Rows scrolled away meanwhile: skip the work, it will be requested again if needed.
    let wanted = PENDING.with(|p| {
        p.borrow()
            .get(&key)
            .is_some_and(|w| w.iter().any(|tx| !tx.is_canceled()))
    });
    if !wanted {
        glib::g_debug!(
            "spiral",
            "thumbnail {uri}: nobody waiting any more, dropped"
        );
    }
    let texture = if wanted {
        gio::spawn_blocking(move || {
            let png = match found {
                Cached::Png(png) => {
                    glib::g_debug!(
                        "spiral",
                        "thumbnail {uri}: in the cache as {}",
                        png.display()
                    );
                    png
                }
                Cached::Failed => return None,
                Cached::Missing => {
                    let path = source.path?;
                    if source.too_large {
                        return None;
                    }
                    let thumbnailers = thumbnailers_for(&source.content_type);
                    if thumbnailers.is_empty() && !source.own {
                        glib::g_debug!(
                            "spiral",
                            "thumbnail {uri}: no thumbnailer claims {}",
                            source.content_type
                        );
                        return None;
                    }
                    let out = cache_path(&uri);
                    let made = generate(
                        &path,
                        &uri,
                        mtime,
                        &out,
                        &source.content_type,
                        &thumbnailers,
                    );
                    // A file written this second or the one before may still be being
                    // written, pausing between writes: what was drawn of it, or the note
                    // that it could not be, would name the second the finished file most
                    // likely keeps, and stand for it from then on. Nothing of it is kept
                    // on disk. What was drawn is shown if the file is as its row was read;
                    // the row asks again once the file is read again.
                    let settled = still(&path, mtime, size);
                    if !settled || recent(mtime) {
                        let drawn = match made {
                            Ok(Some(texture)) => Some(texture),
                            Ok(None) => picture_of(&out),
                            Err(_) => return None,
                        };
                        let _ = std::fs::remove_file(&out);
                        return drawn.filter(|_| settled);
                    }
                    match made {
                        Ok(Some(texture)) => return Some(texture),
                        Ok(None) => out,
                        Err(Failure::OfTheFile) => {
                            remember_failure(&uri, mtime);
                            return None;
                        }
                        Err(Failure::OfTheMoment) => return None,
                    }
                }
            };
            let texture = picture_of(&png);
            if texture.is_none() {
                glib::g_debug!("spiral", "thumbnail: cannot load {}", png.display());
            }
            texture
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    release();
    if !wanted {
        // Nobody is waiting any more, and nothing was worked out: leave the list alone so
        // the next request for it starts afresh.
        PENDING.with(|p| p.borrow_mut().remove(&key));
        return;
    }
    finish(key, texture);
}

/// Hand the answer to everyone who asked for it and take the request off the list.
fn finish(key: Key, texture: Option<gdk::Texture>) {
    remember(key.clone(), texture.clone());
    if let Some(waiters) = PENDING.with(|p| p.borrow_mut().remove(&key)) {
        for tx in waiters {
            let _ = tx.send(texture.clone());
        }
    }
}

fn remember(key: Key, texture: Option<gdk::Texture>) {
    CACHE.with(|c| c.borrow_mut().insert(key, texture));
}

async fn acquire(at: u32) {
    thread_local! {
        static LIMIT: usize = max_parallel();
    }
    let limit = LIMIT.with(|l| *l);
    loop {
        if RUNNING.with(|r| {
            if r.get() < limit {
                r.set(r.get() + 1);
                true
            } else {
                false
            }
        }) {
            return;
        }
        let (tx, rx) = oneshot::channel();
        WAITERS.with(|w| w.borrow_mut().push((at, tx)));
        let _ = rx.await;
    }
}

fn release() {
    RUNNING.with(|r| r.set(r.get() - 1));
    // The row nearest the top of the folder goes next, so a screenful fills the way it is
    // read. Only rows that were on screen a moment ago are ever in here, so the lowest of
    // them is the top of what is being looked at now; rows left behind by scrolling are
    // still woken, and drop out at the `wanted` check without doing any work.
    let next = WAITERS.with(|w| {
        let mut waiters = w.borrow_mut();
        let first = waiters
            .iter()
            .enumerate()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(i, _)| i)?;
        Some(waiters.swap_remove(first).1)
    });
    if let Some(tx) = next {
        let _ = tx.send(());
    }
}

fn cache_path(uri: &str) -> PathBuf {
    thumbnail_dir("large").join(cache_name(uri))
}

fn thumbnail_dir(size: &str) -> PathBuf {
    glib::user_cache_dir().join("thumbnails").join(size)
}

fn cache_name(uri: &str) -> String {
    let md5 = glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri).unwrap_or_default();
    format!("{md5}.png")
}

/// What the freedesktop cache holds for a file: a thumbnail made for this version of it,
/// a note that making one failed, or nothing.
enum Cached {
    Png(PathBuf),
    Failed,
    Missing,
}

/// Look `uri` up in the freedesktop cache. GIO answers this as well, through the
/// `thumbnail::` attributes, but only by hashing the name and looking in three directories
/// for every file a folder holds, listed or not, which is two fifths of the time a large
/// folder takes to appear. Here the question is asked for the rows actually shown.
fn cached_thumbnail(uri: &str, mtime: u64) -> Cached {
    let name = cache_name(uri);
    // A file that has already defeated a thumbnailer is not handed to one again on every
    // start. Only Spiral's own notes count: the spec keeps them per program because
    // programs draw with different tools, and a file another program gave up on before a
    // codec was installed is not one this one cannot draw.
    if stamped_for(&fail_path("spiral", &name), mtime) {
        return Cached::Failed;
    }
    for size in ["large", "normal"] {
        let png = thumbnail_dir(size).join(&name);
        if stamped_for(&png, mtime) {
            return Cached::Png(png);
        }
    }
    Cached::Missing
}

/// Whether `mtime` is this second or the one before, as the clock has it now.
fn recent(mtime: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    now <= mtime.saturating_add(1)
}

/// Whether the file at `path` is still as it was when its row was read: written in the
/// same second, and as long.
fn still(path: &Path, mtime: u64, size: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    // Counted the way the row's time is, a time before 1970 included.
    std::fs::metadata(path).is_ok_and(|meta| meta.mtime() as u64 == mtime && meta.len() == size)
}

fn fail_path(by: &str, name: &str) -> PathBuf {
    thumbnail_dir("fail").join(by).join(name)
}

/// Leave a note that this file cannot be drawn, so the next run does not try again. A
/// folder of files no thumbnailer claims otherwise costs the same fruitless work at every
/// start. The note is a one-pixel PNG carrying the name and time of the file, which is what
/// makes it stale when the file is written again.
fn remember_failure(uri: &str, mtime: u64) {
    let png = fail_path("spiral", &cache_name(uri));
    let Some(dir) = png.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Some(blank) = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 1, 1)
    else {
        return;
    };
    blank.fill(0);
    let _ = blank.savev(
        &png,
        "png",
        &[
            ("tEXt::Thumb::URI", uri),
            ("tEXt::Thumb::MTime", &mtime.to_string()),
        ],
    );
}

/// The picture in the thumbnail `png`, decoded like any picture: see [`crate::picture`].
pub(crate) fn picture_of(png: &Path) -> Option<gdk::Texture> {
    crate::picture::load_file(png, THUMBNAIL_PIXELS)
}

/// The thumbnail the cache holds for `uri`, for callers that only want the picture.
pub(crate) fn cached_png(uri: &str, mtime: u64) -> Option<PathBuf> {
    match cached_thumbnail(uri, mtime) {
        Cached::Png(png) => Some(png),
        _ => None,
    }
}

/// Whether `png` is a thumbnail carrying the `Thumb::MTime` of a file last changed at
/// `mtime`, which is what the spec calls valid. The text chunks sit at the head of the
/// file, so this reads a few kilobytes and decodes nothing.
fn stamped_for(png: &Path, mtime: u64) -> bool {
    thumb_mtime(png) == Some(mtime)
}

fn thumb_mtime(png: &Path) -> Option<u64> {
    let mut head = [0u8; 8192];
    let mut file = std::fs::File::open(png).ok()?;
    let read = std::io::Read::read(&mut file, &mut head).ok()?;
    let head = &head[..read];
    if !head.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = 8;
    while at + 8 <= head.len() {
        let len = u32::from_be_bytes(head[at..at + 4].try_into().ok()?) as usize;
        let kind = &head[at + 4..at + 8];
        // Everything a thumbnail says about itself comes before the pixels.
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        let start = at + 8;
        let end = start.checked_add(len)?;
        if end > head.len() {
            return None;
        }
        if kind == b"tEXt"
            && let Some(stamp) = head[start..end].strip_prefix(b"Thumb::MTime\0")
        {
            return std::str::from_utf8(stamp).ok()?.trim().parse().ok();
        }
        at = end + 4;
    }
    None
}

/// Why a thumbnail was not made.
enum Failure {
    /// The thumbnailer looked at the file and could not draw it. Worth a note, so the
    /// next start does not ask again.
    OfTheFile,
    /// The thumbnailer ran out of time or could not be started. A machine busy with
    /// seven other decoders, a long film decoded in software, or a helper that is not
    /// installed yet say nothing about the file, and a note would keep it from ever
    /// being asked about again once things are quieter or the helper is there.
    OfTheMoment,
}

/// Runs on a worker thread. Writes a spec-compliant PNG (Thumb::URI / Thumb::MTime)
/// atomically; the picture as well where it was drawn here, so it is not decoded again.
fn generate(
    path: &Path,
    uri: &str,
    mtime: u64,
    out: &Path,
    content_type: &str,
    thumbnailers: &[Thumbnailer],
) -> Result<Option<gdk::Texture>, Failure> {
    let Some(dir) = out.parent() else {
        return Err(Failure::OfTheMoment);
    };
    let _ = std::fs::create_dir_all(dir);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = out.with_extension(format!("{}-{seq}.tmp.png", std::process::id()));
    let mut worst = Failure::OfTheFile;
    // Glycin first, for the pictures it reads: its loaders wait in its sandbox from one
    // file to the next, where a thumbnailer's sandbox is started for each. Where it cannot
    // draw one, the system's thumbnailers are asked; the bundled helper, which would ask
    // glycin again through gdk-pixbuf, is not.
    let glycin = crate::glycin::get().filter(|glycin| glycin.handles(content_type));
    if let Some(glycin) = glycin {
        let started = std::time::Instant::now();
        match glycin_thumbnail(glycin, path, uri, mtime, &tmp) {
            Ok(texture) if std::fs::rename(&tmp, out).is_ok() => {
                glib::g_debug!(
                    "spiral",
                    "thumbnail {uri}: made in {} ms by glycin",
                    started.elapsed().as_millis()
                );
                return Ok(Some(texture));
            }
            Ok(_) => worst = Failure::OfTheMoment,
            Err(crate::glycin::Refused::Time) => worst = Failure::OfTheMoment,
            Err(crate::glycin::Refused::File(e)) => {
                glib::g_debug!("spiral", "thumbnail {uri}: glycin: {e}");
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
    // The bundled helper stands in for images no system thumbnailer claims. There is no
    // third way: decoding in this process would put an untrusted file in front of a loader
    // with the whole session behind it.
    let execs: Vec<String> = if thumbnailers.is_empty() && glycin.is_none() {
        match own_thumbnailer() {
            Some(bin) => vec![format!(
                "{} %i %o %s",
                glib::shell_quote(bin).to_string_lossy()
            )],
            None => {
                glib::g_debug!("spiral", "thumbnail {uri}: no thumbnailer for it");
                return Err(Failure::OfTheMoment);
            }
        }
    } else {
        thumbnailers.iter().map(|t| t.exec.clone()).collect()
    };
    // Where several entries claim a type they are tried in turn: one that lacks a codec
    // the next one has, or hangs on a file, does not decide for the others.
    for exec in execs {
        let started = std::time::Instant::now();
        let run = run_thumbnailer(&exec, path, &tmp);
        // GIO only accepts a cached thumbnail that names the file it was made from, so
        // one that does not say so is written again with the words. Thumbnailers that
        // follow the spec already say it, and are left alone rather than decoded and
        // encoded once more.
        if run.is_ok()
            && (stamped_for(&tmp, mtime) || stamp(&tmp, uri, mtime))
            && std::fs::rename(&tmp, out).is_ok()
        {
            glib::g_debug!(
                "spiral",
                "thumbnail {uri}: made in {} ms by {exec}",
                started.elapsed().as_millis()
            );
            return Ok(None);
        }
        let _ = std::fs::remove_file(&tmp);
        if matches!(run, Ok(()) | Err(Failure::OfTheMoment)) {
            worst = Failure::OfTheMoment;
        }
    }
    glib::g_debug!("spiral", "thumbnail {uri}: generation failed");
    Err(worst)
}

/// A thumbnail of the picture at `path`, drawn by glycin at the size asked for and
/// written to `tmp` with the words the spec asks for.
fn glycin_thumbnail(
    glycin: &crate::glycin::Glycin,
    path: &Path,
    uri: &str,
    mtime: u64,
    tmp: &Path,
) -> Result<gdk::Texture, crate::glycin::Refused> {
    use crate::glycin::Refused;
    let thumbnail =
        glycin.load_file(&gio::File::for_path(path), SOURCE_PIXELS, Some(SIZE as u32))?;
    let png = with_text(&thumbnail.save_to_png_bytes(), uri, mtime)
        .ok_or(Refused::File("cannot stamp".into()))?;
    std::fs::write(tmp, png).map_err(|e| Refused::File(e.to_string()))?;
    Ok(thumbnail)
}

/// Write the Thumb::URI and Thumb::MTime a thumbnailer's PNG was made for; without them a
/// cached thumbnail counts as invalid and everything is made again on the next start.
///
/// The words go straight into the file as PNG text chunks, which is a read and a write of a
/// few tens of kilobytes. Decoding the picture and encoding it again, which is what asking
/// gdk-pixbuf to save it with them costs, was a fifth of the work of making a thumbnail.
fn stamp(png: &Path, uri: &str, mtime: u64) -> bool {
    let Ok(bytes) = std::fs::read(png) else {
        return false;
    };
    match with_text(&bytes, uri, mtime) {
        Some(stamped) => std::fs::write(png, stamped).is_ok(),
        // Not a PNG the chunks can be put into: fall back to saving it again as one, from
        // the picture decoded like any other.
        None => picture_of(png)
            .and_then(|texture| with_text(&texture.save_to_png_bytes(), uri, mtime))
            .is_some_and(|stamped| std::fs::write(png, stamped).is_ok()),
    }
}

/// `bytes` with the two text chunks put in after the header, or None if it does not begin
/// like a PNG whose header is where the format says it is.
fn with_text(bytes: &[u8], uri: &str, mtime: u64) -> Option<Vec<u8>> {
    // Signature, then IHDR: length, type, thirteen bytes of it, and the checksum.
    const AFTER_HEADER: usize = 8 + 4 + 4 + 13 + 4;
    if bytes.len() < AFTER_HEADER
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || &bytes[12..16] != b"IHDR"
    {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() + 128);
    out.extend_from_slice(&bytes[..AFTER_HEADER]);
    for (key, value) in [("Thumb::URI", uri), ("Thumb::MTime", &mtime.to_string())] {
        let mut data = Vec::with_capacity(key.len() + 1 + value.len());
        data.extend_from_slice(key.as_bytes());
        data.push(0);
        data.extend_from_slice(value.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(b"tEXt");
        out.extend_from_slice(&data);
        let mut crc = b"tEXt".to_vec();
        crc.extend_from_slice(&data);
        out.extend_from_slice(&crc32(&crc).to_be_bytes());
    }
    out.extend_from_slice(&bytes[AFTER_HEADER..]);
    Some(out)
}

/// The checksum PNG puts after every chunk.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The bundled image thumbnailer: next to the running binary when uninstalled, else in libexec.
pub(crate) fn own_thumbnailer() -> Option<PathBuf> {
    let name = "spiral-thumbnailer";
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .or_else(|| Some(Path::new(crate::config::LIBEXECDIR).join(name)).filter(|p| p.exists()))
}

fn run_thumbnailer(exec: &str, input: &Path, output: &Path) -> Result<(), Failure> {
    let Ok(argv) = glib::shell_parse_argv(exec) else {
        return Err(Failure::OfTheMoment);
    };
    // Inside the sandbox the input is /tmp/in.<ext> and the output lands in a private
    // directory bound at /tmp/out, so the thumbnailer sees nothing else of the home.
    let work = output.with_extension("d");
    let ext = input
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let in_path = PathBuf::from(format!("/tmp/in{ext}"));
    let out_path = PathBuf::from("/tmp/out/thumb.png");
    let in_uri = gio::File::for_path(&in_path).uri().to_string();
    let (in_s, out_s, size_s) = (
        in_path.to_string_lossy(),
        out_path.to_string_lossy(),
        SIZE.to_string(),
    );
    let argv: Vec<String> = argv
        .into_iter()
        .map(|a| {
            expand(
                &a.to_string_lossy(),
                &[('i', &in_s), ('u', &in_uri), ('o', &out_s), ('s', &size_s)],
            )
        })
        .collect();
    let Some(sandbox) = crate::sandbox::command(&argv[0]) else {
        return Err(Failure::OfTheMoment);
    };
    if std::fs::create_dir_all(&work).is_err() {
        return Err(Failure::OfTheMoment);
    }
    let mut cmd = std::process::Command::new(&sandbox.argv[0]);
    cmd.args(&sandbox.argv[1..]);
    let font_cache = glib::user_cache_dir().join("fontconfig");
    cmd.args(["--ro-bind-try", "/etc/fonts", "/etc/fonts"]);
    cmd.args([
        "--ro-bind-try",
        "/var/cache/fontconfig",
        "/var/cache/fontconfig",
    ]);
    cmd.arg("--ro-bind-try").arg(&font_cache).arg(&font_cache);
    cmd.arg("--ro-bind").arg(input).arg(&in_path);
    cmd.arg("--bind").arg(&work).arg("/tmp/out");
    cmd.arg("--");
    cmd.args(&argv);
    // The seccomp memfd must stay open until the child has started.
    let run = crate::sandbox::run_bounded(&mut cmd, TIMEOUT);
    drop(sandbox.seccomp);
    // What the thumbnailer left is copied out, not moved: see `read_output`.
    if run.as_ref().is_ok_and(|ran| ran.ok)
        && let Some(png) = crate::sandbox::read_output(&work.join("thumb.png"), OUTPUT_LIMIT)
            .filter(|png| png.starts_with(PNG_SIGNATURE))
        && !write_new(output, &png)
    {
        let _ = std::fs::remove_file(output);
    }
    let _ = std::fs::remove_dir_all(&work);
    match run {
        Ok(ran) if ran.ok && output.exists() => Ok(()),
        Ok(ran) => {
            glib::g_debug!("spiral", "thumbnailer {argv:?} failed: {}", ran.trouble);
            Err(if ran.timed_out {
                Failure::OfTheMoment
            } else {
                Failure::OfTheFile
            })
        }
        Err(e) => {
            glib::g_debug!("spiral", "thumbnailer {argv:?} could not start: {e}");
            Err(Failure::OfTheMoment)
        }
    }
}

/// `arg` with each `%` field code in `codes` replaced, in one pass: what a code is replaced
/// with is not looked at again, so a file whose name holds `%o` stays the file it is.
fn expand(arg: &str, codes: &[(char, &str)]) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let next = chars.clone().next();
        match codes.iter().find(|(code, _)| Some(*code) == next) {
            Some((_, value)) => {
                out.push_str(value);
                chars.next();
            }
            None => out.push(c),
        }
    }
    out
}

/// Write `bytes` to `path`, which must not exist yet.
fn write_new(path: &Path, bytes: &[u8]) -> bool {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .is_ok()
}

/// Every system entry claiming the type, in the order they are tried. System thumbnailers
/// win over gdk-pixbuf (gdk-pixbuf ships one itself). Runs on a worker thread: the entries
/// are read from disk the first time.
fn thumbnailers_for(content_type: &str) -> Vec<Thumbnailer> {
    THUMBNAILERS
        .get_or_init(load_thumbnailers)
        .iter()
        .filter(|t| {
            t.mime_types.iter().any(|m| {
                gio::content_type_equals(content_type, m) || gio::content_type_is_a(content_type, m)
            })
        })
        .map(|t| Thumbnailer {
            exec: t.exec.clone(),
            mime_types: Vec::new(),
        })
        .collect()
}

fn load_thumbnailers() -> Vec<Thumbnailer> {
    let mut dirs: Vec<PathBuf> = vec![glib::user_data_dir().join("thumbnailers")];
    dirs.extend(
        glib::system_data_dirs()
            .into_iter()
            .map(|d| d.join("thumbnailers")),
    );
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // The order of a directory listing is whatever the filesystem makes it, and it
        // decides which of two entries claiming a type is tried first; by name it is the
        // same on every machine, and a user's own entries come before the system's.
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let kf = glib::KeyFile::new();
            if kf.load_from_file(&path, glib::KeyFileFlags::NONE).is_err() {
                continue;
            }
            let Ok(exec) = kf.string("Thumbnailer Entry", "Exec") else {
                continue;
            };
            if let Ok(try_exec) = kf.string("Thumbnailer Entry", "TryExec")
                && glib::find_program_in_path(&try_exec).is_none()
            {
                continue;
            }
            let Ok(types) = kf.string_list("Thumbnailer Entry", "MimeType") else {
                continue;
            };
            out.push(Thumbnailer {
                exec: exec.to_string(),
                mime_types: types.iter().map(|s| s.to_string()).collect(),
            });
        }
    }
    glib::g_debug!("spiral", "loaded {} thumbnailers", out.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words written into a PNG have to be the words read back out of it, or every
    /// thumbnail is made again on the next start.
    #[test]
    fn a_stamped_png_says_when_its_file_was_written() {
        let dir = std::path::PathBuf::from(
            std::env::var("CARGO_TARGET_TMPDIR")
                .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
        );
        let png = dir.join(format!("spiral-stamp-{}.png", std::process::id()));
        let pixbuf =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 4, 4).unwrap();
        pixbuf.fill(0x336699ff);
        pixbuf.savev(&png, "png", &[]).unwrap();

        assert_eq!(thumb_mtime(&png), None, "no words in it yet");
        assert!(stamp(&png, "file:///x/y.png", 1_700_000_000));
        assert_eq!(thumb_mtime(&png), Some(1_700_000_000));
        // Still a picture afterwards.
        assert!(gtk::gdk_pixbuf::Pixbuf::from_file(&png).is_ok());
        let _ = std::fs::remove_file(&png);
    }

    /// A name is what the thumbnailer is told, whatever field codes it spells.
    #[test]
    fn field_codes_are_expanded_once() {
        let codes = [
            ('i', "/tmp/in.%o"),
            ('o', "/tmp/out/thumb.png"),
            ('s', "256"),
        ];
        assert_eq!(expand("%i", &codes), "/tmp/in.%o");
        assert_eq!(
            expand("-s %s %i %o", &codes),
            "-s 256 /tmp/in.%o /tmp/out/thumb.png"
        );
        assert_eq!(expand("100%", &codes), "100%");
        assert_eq!(expand("%x%", &codes), "%x%");
    }
}
