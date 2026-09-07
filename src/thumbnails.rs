//! Freedesktop thumbnails: reuse `~/.cache/thumbnails`, otherwise generate via the system
//! `.thumbnailer` entries (or the bundled gdk-pixbuf helper for images without one), with
//! bounded concurrency. Every thumbnailer runs inside bubblewrap when it is available, so a
//! crashing or hostile decoder is confined to the sandbox.

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
const MAX_PARALLEL: usize = 4;
const CACHE_CAP: usize = 512;

/// URI, modification time and size. The size belongs in the key because a file being
/// written is seen empty first, and a verdict of "no thumbnail" taken from that snapshot
/// would otherwise outlive the write whenever both land in the same second.
type Key = (String, u64, u64);

thread_local! {
    static CACHE: RefCell<HashMap<Key, Option<gdk::Texture>>> = RefCell::new(HashMap::new());
    /// Loads in progress; later requests for the same key wait for the first one.
    static PENDING: RefCell<HashMap<Key, Vec<oneshot::Sender<Option<gdk::Texture>>>>> = RefCell::new(HashMap::new());
    static RUNNING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WAITERS: RefCell<std::collections::VecDeque<oneshot::Sender<()>>> = RefCell::new(Default::default());
    static THUMBNAILERS: RefCell<Option<Rc<Vec<Thumbnailer>>>> = const { RefCell::new(None) };
}

static SEQ: AtomicU64 = AtomicU64::new(0);

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
    if let Some(cached) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return cached;
    }

    // A thumbnail GIO already knows to be valid still has to be decoded, and that is
    // done where a fresh one is: on a worker thread, a few at a time.
    let cached = info
        .boolean("thumbnail::is-valid")
        .then(|| info.attribute_byte_string("thumbnail::path"))
        .flatten()
        .map(|path| PathBuf::from(path.as_str()));
    let source = match cached {
        Some(png) => Source::Cached(png),
        None => {
            let content_type = info.content_type()?.to_string();
            let path = file.path()?;
            let thumbnailer = thumbnailer_for(&content_type);
            if !content_type.starts_with("image/") && thumbnailer.is_none() {
                remember(key, None);
                return None;
            }
            Source::Generate {
                path,
                uri,
                mtime,
                thumbnailer,
            }
        }
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

/// Where a thumbnail comes from: the cache file GIO found valid, or a thumbnailer run.
enum Source {
    Cached(PathBuf),
    Generate {
        path: PathBuf,
        uri: String,
        mtime: u64,
        thumbnailer: Option<Box<Thumbnailer>>,
    },
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
        let uri = key.0.clone();
        gio::spawn_blocking(move || {
            let png = match source {
                Source::Cached(png) => png,
                Source::Generate {
                    path,
                    uri,
                    mtime,
                    thumbnailer,
                } => {
                    let out = cache_path(&uri);
                    generate(&path, &uri, mtime, &out, thumbnailer.as_deref()).then_some(out)?
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
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key, texture);
    });
}

async fn acquire() {
    loop {
        if RUNNING.with(|r| {
            if r.get() < MAX_PARALLEL {
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
    let md5 = glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri).unwrap_or_default();
    glib::user_cache_dir()
        .join("thumbnails")
        .join("large")
        .join(format!("{md5}.png"))
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
    let ok = match thumbnailer {
        Some(t) => run_thumbnailer(&t.exec, path, uri, &tmp) && stamp(&tmp, uri, mtime),
        None => match own_thumbnailer() {
            Some(bin) => {
                run_thumbnailer(
                    &format!("{} %i %o %s", glib::shell_quote(bin).to_string_lossy()),
                    path,
                    uri,
                    &tmp,
                ) && stamp(&tmp, uri, mtime)
            }
            // Helper not installed: decode in-process as a last resort.
            None => match gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, SIZE, SIZE, true) {
                Ok(pb) => {
                    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
                    pb.savev(
                        &tmp,
                        "png",
                        &[
                            ("tEXt::Thumb::URI", uri),
                            ("tEXt::Thumb::MTime", &mtime.to_string()),
                        ],
                    )
                    .is_ok()
                }
                Err(e) => {
                    glib::g_debug!("spiral", "thumbnail {uri}: pixbuf failed: {e}");
                    false
                }
            },
        },
    };
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
/// helpers over untrusted input. None when bwrap is not installed.
pub(crate) struct Sandbox {
    pub argv: Vec<String>,
    /// Inherited memfd holding the seccomp program named in `argv`.
    pub seccomp: Option<std::fs::File>,
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

fn run_thumbnailer(exec: &str, input: &Path, uri: &str, output: &Path) -> bool {
    let Ok(argv) = glib::shell_parse_argv(exec) else {
        return false;
    };
    let bwrap = glib::find_program_in_path("bwrap");
    // Inside the sandbox the input is /tmp/in.<ext> and the output lands in a private
    // directory bound at /tmp/out, so the thumbnailer sees nothing else of the home.
    let work = output.with_extension("d");
    let (in_path, out_path) = if bwrap.is_some() {
        let ext = input
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        (
            PathBuf::from(format!("/tmp/in{ext}")),
            PathBuf::from("/tmp/out/thumb.png"),
        )
    } else {
        (input.to_path_buf(), output.to_path_buf())
    };
    let in_uri = if bwrap.is_some() {
        gio::File::for_path(&in_path).uri().to_string()
    } else {
        uri.to_string()
    };
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
    let mut seccomp = None;
    let mut cmd = match sandbox_base(&argv[0]) {
        Some(sandbox) => {
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
            seccomp = sandbox.seccomp;
            cmd
        }
        None => {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd
        }
    };
    let run = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    drop(seccomp);
    if bwrap.is_some() && run.as_ref().is_ok_and(|o| o.status.success()) {
        let _ = std::fs::rename(work.join("thumb.png"), output);
    }
    let _ = std::fs::remove_dir_all(&work);
    match run {
        Ok(o) if o.status.success() && output.exists() => true,
        Ok(o) => {
            glib::g_debug!(
                "spiral",
                "thumbnailer {argv:?} failed ({}): {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            false
        }
        Err(e) => {
            glib::g_debug!("spiral", "thumbnailer {argv:?} could not start: {e}");
            false
        }
    }
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
