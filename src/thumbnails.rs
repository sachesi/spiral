//! Freedesktop thumbnails: reuse `~/.cache/thumbnails`, otherwise generate via the system
//! `.thumbnailer` entries (or the bundled gdk-pixbuf helper for images without one), with
//! bounded concurrency. Every thumbnailer runs inside bubblewrap, which is required: a
//! decoder is fed files from anywhere and there is no unconfined path for it to take.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use std::os::fd::AsRawFd;

use futures_channel::oneshot;
use gtk::prelude::*;

use crate::{gdk, gio, glib, gtk};

const SIZE: i32 = 256;

/// Thumbnailers to run at once. One core is left to the interface, which is drawing the
/// rows they are for; a folder of thousands of pictures is otherwise limited by a number
/// picked for the machines of a decade ago.
fn max_parallel() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(2, 8))
        .unwrap_or(4)
}

/// How long one thumbnailer may take before it is killed.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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
    static WAITERS: RefCell<std::collections::VecDeque<oneshot::Sender<()>>> = RefCell::new(Default::default());
    static THUMBNAILERS: RefCell<Option<Rc<Vec<Thumbnailer>>>> = const { RefCell::new(None) };
}

static SEQ: AtomicU64 = AtomicU64::new(0);

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
pub async fn load(info: &gio::FileInfo) -> Option<gdk::Texture> {
    if info.file_type() == gio::FileType::Directory {
        return None;
    }
    let file = crate::file_utils::file_of(info);
    // Checked before the cache so a preference change takes effect on the next reload.
    if !crate::prefs::thumbnails_for(&file) {
        return None;
    }
    let uri = file.uri().to_string();
    let mtime = info
        .modification_date_time()
        .map(|d| d.to_unix() as u64)
        .unwrap_or(0);
    let key = (uri.clone(), mtime, info.size().max(0) as u64);
    if let Some(cached) = CACHE.with(|c| c.borrow().seen.get(&key).cloned()) {
        return cached;
    }

    // Which thumbnailer would make one is decided here, where the list of them lives;
    // the cache the freedesktop directories already hold is looked at on the worker,
    // where a thumbnail is decoded anyway.
    // Neither a type nor a local path stops the lookup: a file on a share may still have
    // a thumbnail in the cache, made when it was somewhere else or by something else.
    let content_type = info.content_type().unwrap_or_default().to_string();
    let source = Source {
        path: file.path(),
        thumbnailer: thumbnailer_for(&content_type),
        // Images with no thumbnailer of their own go to the bundled helper.
        own: content_type.starts_with("image/"),
        // Only pictures are weighed: a video thumbnailer reads a frame, not the file, so
        // the size of the file says nothing about what it will cost. One already in the
        // cache is shown whatever the size, which is why this only stops generation.
        too_large: content_type.starts_with("image/")
            && info.size().max(0) as u64 > crate::prefs::thumbnail_limit(),
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
        glib::spawn_future_local(generate_task(key, source));
    }
    rx.await.ok().flatten()
}

/// What a thumbnail would be made from, if the cache has none.
struct Source {
    /// Only a local file can be handed to a thumbnailer.
    path: Option<PathBuf>,
    thumbnailer: Option<Box<Thumbnailer>>,
    /// Whether the bundled helper would take it: images, which most thumbnailer entries
    /// leave alone.
    own: bool,
    /// Whether the file is too big to be worth decoding.
    too_large: bool,
}

async fn generate_task(key: Key, source: Source) {
    acquire().await;
    // Rows scrolled away meanwhile: skip the work, it will be requested again if needed.
    let wanted = PENDING.with(|p| {
        p.borrow()
            .get(&key)
            .is_some_and(|w| w.iter().any(|tx| !tx.is_canceled()))
    });
    let texture = if wanted {
        let (uri, mtime) = (key.0.clone(), key.1);
        gio::spawn_blocking(move || {
            let png = match cached_thumbnail(&uri, mtime) {
                Cached::Png(png) => png,
                Cached::Failed => return None,
                Cached::Missing => {
                    let path = source.path?;
                    if source.too_large || (source.thumbnailer.is_none() && !source.own) {
                        return None;
                    }
                    let out = cache_path(&uri);
                    generate(&path, &uri, mtime, &out, source.thumbnailer.as_deref())
                        .then_some(out)?
                }
            };
            match gdk::Texture::from_filename(&png) {
                Ok(t) => Some(t),
                Err(e) => {
                    glib::g_debug!(
                        "spiral",
                        "thumbnail {uri}: cannot load {}: {e}",
                        png.display()
                    );
                    None
                }
            }
        })
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    release();
    if wanted {
        remember(key.clone(), texture.clone());
    }
    if let Some(waiters) = PENDING.with(|p| p.borrow_mut().remove(&key)) {
        for tx in waiters {
            let _ = tx.send(texture.clone());
        }
    }
}

fn remember(key: Key, texture: Option<gdk::Texture>) {
    CACHE.with(|c| c.borrow_mut().insert(key, texture));
}

async fn acquire() {
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
        WAITERS.with(|w| w.borrow_mut().push_back(tx));
        let _ = rx.await;
    }
}

fn release() {
    RUNNING.with(|r| r.set(r.get() - 1));
    // Newest request first: rows just scrolled into view beat ones scrolled past, and
    // stale entries are skipped by the `wanted` check when their turn comes.
    if let Some(tx) = WAITERS.with(|w| w.borrow_mut().pop_back()) {
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
    // Where GIO looks for other programs' failures too, so a file that has already
    // defeated a thumbnailer is not handed to one again on every start.
    if stamped_for(
        &thumbnail_dir("fail")
            .join("gnome-thumbnail-factory")
            .join(&name),
        mtime,
    ) {
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
    if !head.starts_with(b"\x89PNG\r\n\x1a\n") {
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

/// Runs on a worker thread. Writes a spec-compliant PNG (Thumb::URI / Thumb::MTime) atomically.
fn generate(
    path: &Path,
    uri: &str,
    mtime: u64,
    out: &Path,
    thumbnailer: Option<&Thumbnailer>,
) -> bool {
    let Some(dir) = out.parent() else {
        return false;
    };
    let _ = std::fs::create_dir_all(dir);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = out.with_extension(format!("{}-{seq}.tmp.png", std::process::id()));
    // The bundled helper stands in for images no system thumbnailer claims. There is no
    // third way: decoding in this process would put an untrusted file in front of a loader
    // with the whole session behind it.
    let exec = match thumbnailer {
        Some(t) => t.exec.clone(),
        None => match own_thumbnailer() {
            Some(bin) => format!("{} %i %o %s", glib::shell_quote(bin).to_string_lossy()),
            None => {
                glib::g_debug!("spiral", "thumbnail {uri}: no thumbnailer for it");
                return false;
            }
        },
    };
    // GIO only accepts a cached thumbnail that names the file it was made from, so one
    // that does not say so is written again with the words. Thumbnailers that follow the
    // spec already say it, and are left alone rather than decoded and encoded once more.
    let ok =
        run_thumbnailer(&exec, path, &tmp) && (stamped_for(&tmp, mtime) || stamp(&tmp, uri, mtime));
    if ok && std::fs::rename(&tmp, out).is_ok() {
        return true;
    }
    glib::g_debug!("spiral", "thumbnail {uri}: generation failed");
    let _ = std::fs::remove_file(&tmp);
    false
}

/// Rewrite a thumbnailer's PNG with the Thumb::URI and Thumb::MTime it was made for; without
/// them GIO reports the cached file as invalid and it is regenerated on every start.
fn stamp(png: &Path, uri: &str, mtime: u64) -> bool {
    gtk::gdk_pixbuf::Pixbuf::from_file(png)
        .and_then(|pb| {
            pb.savev(
                png,
                "png",
                &[
                    ("tEXt::Thumb::URI", uri),
                    ("tEXt::Thumb::MTime", &mtime.to_string()),
                ],
            )
        })
        .is_ok()
}

/// A `bwrap` command line up to (not including) the caller's own binds and `--`, for running
/// helpers over untrusted input. None when bwrap is not installed, and then the helper is
/// not run at all: everything this sandbox holds reads files chosen by whoever wrote them.
pub(crate) struct Sandbox {
    pub argv: Vec<String>,
    /// Inherited memfd holding the seccomp program named in `argv`.
    pub seccomp: Option<std::fs::File>,
}

/// Try the sandbox once at startup and say what is wrong with it if anything is. Nothing
/// that reads a file someone else wrote runs outside it, so thumbnails, the preview of PDFs
/// and every archive operation depend on it working; a system with user namespaces turned
/// off would otherwise simply show nothing and say nothing.
pub fn check_sandbox() {
    glib::spawn_future_local(async {
        if let Some(trouble) = gio::spawn_blocking(sandbox_trouble).await.ok().flatten() {
            glib::g_warning!(
                "spiral",
                "the bubblewrap sandbox does not work here, so thumbnails, PDF previews \
                 and archive operations are turned off: {trouble}"
            );
        }
    });
}

fn sandbox_trouble() -> Option<String> {
    let Some(sandbox) = sandbox_base("/") else {
        return Some("bwrap (bubblewrap) is not installed".into());
    };
    // Something harmless to run inside it, only to see whether the sandbox itself starts.
    let inside = glib::find_program_in_path("true")?;
    let mut cmd = std::process::Command::new(&sandbox.argv[0]);
    cmd.args(&sandbox.argv[1..]).arg("--").arg(&inside);
    let outcome = run_bounded(&mut cmd, TIMEOUT);
    drop(sandbox.seccomp);
    match outcome {
        Ok(ran) if ran.ok => None,
        Ok(ran) => Some(ran.trouble),
        Err(e) => Some(e.to_string()),
    }
}

pub(crate) fn sandbox_base(program: &str) -> Option<Sandbox> {
    let bwrap = glib::find_program_in_path("bwrap")?;
    let seccomp = seccomp_filter();
    let mut argv = vec![bwrap.to_string_lossy().into_owned()];
    if let Some(fd) = &seccomp {
        argv.push("--seccomp".into());
        argv.push(fd.as_raw_fd().to_string());
    }
    argv.extend(
        [
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--symlink",
            "usr/bin",
            "/bin",
            "--symlink",
            "usr/sbin",
            "/sbin",
            "--ro-bind-try",
            "/etc/ld.so.cache",
            "/etc/ld.so.cache",
            "--ro-bind-try",
            "/etc/alternatives",
            "/etc/alternatives",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--chdir",
            "/",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "PATH",
            "/usr/bin:/usr/sbin",
            "--setenv",
            "GIO_USE_VFS",
            "local",
        ]
        .into_iter()
        .map(String::from),
    );
    // Tools outside /usr (uninstalled builds, /opt) must be mapped in as well.
    if program.starts_with('/') && !program.starts_with("/usr/") {
        argv.extend(
            ["--ro-bind", program, program]
                .into_iter()
                .map(String::from),
        );
    }
    Some(Sandbox { argv, seccomp })
}

/// BPF program denying the syscalls a thumbnailer has no business making (the list Flatpak
/// and gnome-desktop use), in a memfd that the child inherits for `bwrap --seccomp`.
fn seccomp_filter() -> Option<std::fs::File> {
    use libseccomp::{ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall};
    use std::os::fd::FromRawFd;

    let deny = ScmpAction::Errno(libc::EPERM);
    let mut ctx = ScmpFilterContext::new_filter(ScmpAction::Allow).ok()?;
    for name in [
        "syslog",
        "uselib",
        "acct",
        "modify_ldt",
        "quotactl",
        "add_key",
        "keyctl",
        "request_key",
        "move_pages",
        "mbind",
        "get_mempolicy",
        "set_mempolicy",
        "migrate_pages",
        "unshare",
        "mount",
        "umount2",
        "pivot_root",
        "chroot",
        "setns",
        "ptrace",
        "personality",
        "perf_event_open",
        "bpf",
        "kexec_load",
        "kexec_file_load",
        "open_by_handle_at",
        "init_module",
        "finit_module",
        "delete_module",
        "swapon",
        "swapoff",
        "sethostname",
        "setdomainname",
        "reboot",
        "vhangup",
        "userfaultfd",
        "process_vm_readv",
        "process_vm_writev",
        "io_uring_setup",
        "io_uring_enter",
        "io_uring_register",
    ] {
        // Names unknown on this architecture are not filtered.
        if let Ok(sc) = ScmpSyscall::from_name(name) {
            let _ = ctx.add_rule(deny, sc);
        }
    }
    // clone()/clone3() with CLONE_NEWUSER, and ioctl(TIOCSTI) terminal injection.
    let newuser = libc::CLONE_NEWUSER as u64;
    if let Ok(sc) = ScmpSyscall::from_name("clone") {
        let _ = ctx.add_rule_conditional(
            deny,
            sc,
            &[ScmpArgCompare::new(
                0,
                ScmpCompareOp::MaskedEqual(newuser),
                newuser,
            )],
        );
    }
    // ENOSYS, not EPERM: glibc then falls back to clone(), which the rule above screens.
    if let Ok(sc) = ScmpSyscall::from_name("clone3") {
        let _ = ctx.add_rule(ScmpAction::Errno(libc::ENOSYS), sc);
    }
    if let Ok(sc) = ScmpSyscall::from_name("ioctl") {
        let _ = ctx.add_rule_conditional(
            deny,
            sc,
            &[ScmpArgCompare::new(1, ScmpCompareOp::Equal, libc::TIOCSTI)],
        );
    }
    // No MFD_CLOEXEC on purpose: bwrap reads the program through this very fd.
    let fd = unsafe { libc::memfd_create(c"spiral-seccomp".as_ptr(), 0) };
    if fd < 0 {
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    ctx.export_bpf(&mut file).ok()?;
    use std::io::Seek;
    file.rewind().ok()?;
    Some(file)
}

/// The bundled image thumbnailer: next to the running binary when uninstalled, else in libexec.
fn own_thumbnailer() -> Option<PathBuf> {
    let name = "spiral-thumbnailer";
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .or_else(|| Some(Path::new(crate::config::LIBEXECDIR).join(name)).filter(|p| p.exists()))
}

fn run_thumbnailer(exec: &str, input: &Path, output: &Path) -> bool {
    let Ok(argv) = glib::shell_parse_argv(exec) else {
        return false;
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
    let argv: Vec<String> = argv
        .into_iter()
        .map(|a| {
            a.to_string_lossy()
                .replace("%i", &in_path.to_string_lossy())
                .replace("%u", &in_uri)
                .replace("%o", &out_path.to_string_lossy())
                .replace("%s", &SIZE.to_string())
        })
        .collect();
    let Some(sandbox) = sandbox_base(&argv[0]) else {
        return false;
    };
    if std::fs::create_dir_all(&work).is_err() {
        return false;
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
    let run = run_bounded(&mut cmd, TIMEOUT);
    drop(sandbox.seccomp);
    if run.as_ref().is_ok_and(|ran| ran.ok) {
        let _ = std::fs::rename(work.join("thumb.png"), output);
    }
    let _ = std::fs::remove_dir_all(&work);
    match run {
        Ok(ran) if ran.ok && output.exists() => true,
        Ok(ran) => {
            glib::g_debug!("spiral", "thumbnailer {argv:?} failed: {}", ran.trouble);
            false
        }
        Err(e) => {
            glib::g_debug!("spiral", "thumbnailer {argv:?} could not start: {e}");
            false
        }
    }
}

/// Run `cmd` to completion, killing it once it outstays `limit`, and return whether it
/// succeeded along with what it said on stderr. A decoder stuck on a malformed file would
/// otherwise hold one of the few generation slots for the rest of the session.
pub(crate) fn run_bounded(
    cmd: &mut std::process::Command,
    limit: std::time::Duration,
) -> std::io::Result<Ran> {
    // No rlimit is set on the child, tempting as one is: a pre-exec hook costs the
    // process its posix_spawn fast path, and forking a window's worth of address space a
    // thousand times over a folder of pictures costs more than the limit is worth. The
    // time limit below is what keeps a helper from running away.
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Each pipe is drained on a thread of its own: a child that fills one would wait for
    // a reader that is here, waiting for the child.
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = std::time::Instant::now() + limit;
    let mut nap = std::time::Duration::from_millis(1);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(std::time::Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default())
        .trim()
        .to_string();
    Ok(Ran {
        ok: status.success() && !timed_out,
        stdout,
        trouble: if timed_out {
            format!("gave up after {} s", limit.as_secs())
        } else {
            format!("{status}: {stderr}")
        },
    })
}

/// What a bounded run left behind.
pub(crate) struct Ran {
    pub ok: bool,
    pub stdout: Vec<u8>,
    /// What it said for itself when it did not succeed.
    pub trouble: String,
}

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = pipe {
            let _ = std::io::Read::read_to_end(&mut pipe.take(1024 * 1024), &mut buf);
        }
        buf
    })
}

/// System thumbnailers win over gdk-pixbuf, matching GNOME (gdk-pixbuf ships one itself).
fn thumbnailer_for(content_type: &str) -> Option<Box<Thumbnailer>> {
    let all = THUMBNAILERS.with(|t| {
        t.borrow_mut()
            .get_or_insert_with(|| Rc::new(load_thumbnailers()))
            .clone()
    });
    all.iter()
        .find(|t| {
            t.mime_types.iter().any(|m| {
                gio::content_type_equals(content_type, m) || gio::content_type_is_a(content_type, m)
            })
        })
        .map(|t| {
            Box::new(Thumbnailer {
                exec: t.exec.clone(),
                mime_types: Vec::new(),
            })
        })
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
        for e in entries.flatten() {
            let kf = glib::KeyFile::new();
            if kf
                .load_from_file(e.path(), glib::KeyFileFlags::NONE)
                .is_err()
            {
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
