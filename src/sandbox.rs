//! The sandbox everything that reads a file someone else wrote runs in: bubblewrap with a
//! seccomp filter, a check that it works, and runs bounded in time.

use std::os::fd::AsRawFd;

use crate::{gio, glib};

/// A `bwrap` command line up to (not including) the caller's own binds and `--`, for running
/// helpers over untrusted input. None when bwrap is not installed or the seccomp filter
/// cannot be built, and then the helper is not run at all: everything this sandbox holds
/// reads files chosen by whoever wrote them.
pub(crate) struct Sandbox {
    pub argv: Vec<String>,
    /// Inherited memfd holding the seccomp program named in `argv`.
    pub seccomp: std::fs::File,
}

/// Try the sandbox once at startup and say what is wrong with it if anything is. Nothing
/// that reads a file someone else wrote runs outside it, so thumbnails, the preview of PDFs
/// and every archive operation depend on it working; a system with user namespaces turned
/// off would otherwise simply show nothing and say nothing.
pub fn check() {
    glib::spawn_future_local(async {
        if let Some(trouble) = gio::spawn_blocking(trouble).await.ok().flatten() {
            glib::g_warning!(
                "spiral",
                "the bubblewrap sandbox does not work here, so thumbnails, previews of \
                 PDFs, pictures and media, and archive operations are turned off: {trouble}"
            );
        }
    });
}

fn trouble() -> Option<String> {
    let Some(sandbox) = command("/") else {
        return Some(if glib::find_program_in_path("bwrap").is_none() {
            "bwrap (bubblewrap) is not installed".into()
        } else {
            "the seccomp filter could not be built".into()
        });
    };
    // Something harmless to run inside it, only to see whether the sandbox itself starts.
    let inside = glib::find_program_in_path("true")?;
    let mut cmd = std::process::Command::new(&sandbox.argv[0]);
    cmd.args(&sandbox.argv[1..]).arg("--").arg(&inside);
    let outcome = run_bounded(&mut cmd, crate::thumbnails::TIMEOUT);
    drop(sandbox.seccomp);
    match outcome {
        Ok(ran) if ran.ok => None,
        Ok(ran) => Some(ran.trouble),
        Err(e) => Some(e.to_string()),
    }
}

pub(crate) fn command(program: &str) -> Option<Sandbox> {
    let bwrap = glib::find_program_in_path("bwrap")?;
    // A helper that would run without its filter does not run.
    let seccomp = seccomp_filter()?;
    let mut argv = vec![
        bwrap.to_string_lossy().into_owned(),
        "--seccomp".into(),
        seccomp.as_raw_fd().to_string(),
    ];
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

/// BPF program denying the syscalls a thumbnailer has no business making, in a memfd that
/// the child inherits for `bwrap --seccomp`. None when any rule cannot be added: a filter
/// with holes in it is not handed out.
fn seccomp_filter() -> Option<std::fs::File> {
    use libseccomp::{ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall};
    use std::os::fd::FromRawFd;

    let deny = ScmpAction::Errno(libc::EPERM);
    let mut ctx = ScmpFilterContext::new(ScmpAction::Allow).ok()?;
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
            ctx.add_rule(deny, sc).ok()?;
        }
    }
    // clone()/clone3() with CLONE_NEWUSER, and ioctl(TIOCSTI) terminal injection.
    let newuser = libc::CLONE_NEWUSER as u64;
    if let Ok(sc) = ScmpSyscall::from_name("clone") {
        ctx.add_rule_conditional(
            deny,
            sc,
            &[ScmpArgCompare::new(
                0,
                ScmpCompareOp::MaskedEqual(newuser),
                newuser,
            )],
        )
        .ok()?;
    }
    // ENOSYS, not EPERM: glibc then falls back to clone(), which the rule above screens.
    if let Ok(sc) = ScmpSyscall::from_name("clone3") {
        ctx.add_rule(ScmpAction::Errno(libc::ENOSYS), sc).ok()?;
    }
    if let Ok(sc) = ScmpSyscall::from_name("ioctl") {
        ctx.add_rule_conditional(
            deny,
            sc,
            &[ScmpArgCompare::new(1, ScmpCompareOp::Equal, libc::TIOCSTI)],
        )
        .ok()?;
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
    stand_aside(child.id());
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
        timed_out,
        stdout,
        trouble: if timed_out {
            format!("gave up after {} s", limit.as_secs())
        } else {
            format!("{status}: {stderr}")
        },
    })
}

/// Put a helper behind the interface for both processor and disk. Several decoders at once
/// will otherwise take a machine over, and the window they are drawing into stops answering
/// while they do. Both are set after the child has started, since a pre-exec hook would cost
/// the fork-free spawn; the child inherits them for whatever it starts in turn.
fn stand_aside(pid: u32) {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, pid, 10);
        // Idle in the disk queue, which is class 3 in the top three bits of the value.
        libc::syscall(libc::SYS_ioprio_set, 1, pid, 3 << 13);
    }
}

/// How far a sandboxed tool is by what it has read: the `rchar` the kernel keeps for it,
/// which counts every byte it has taken from its input, be that the archive it unpacks or
/// the files it packs. It is what a tool that says nothing about itself can still be
/// measured by. None where the kernel does not tell: `/proc/<pid>/io` is not readable when
/// bwrap is installed setuid, and there the tool simply has no progress of its own.
pub(crate) struct ReadMeter {
    /// The bwrap process that was started here; the tool runs a level or two below it.
    root: u32,
    tool: Option<u32>,
}

impl ReadMeter {
    pub(crate) fn new(root: u32) -> Self {
        ReadMeter { root, tool: None }
    }

    /// Bytes the tool has read so far, looking it up among bwrap's descendants the first
    /// time and then reading one small file per call.
    pub(crate) fn bytes(&mut self) -> Option<u64> {
        if self.tool.is_none() {
            self.tool = descendant(self.root, 0);
        }
        rchar(&std::fs::read_to_string(format!("/proc/{}/io", self.tool?)).ok()?)
    }
}

/// The first process under `pid` that is not bwrap itself: bwrap forks once for the
/// namespace and once more for the tool.
fn descendant(pid: u32, depth: u32) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
    for child in children
        .split_ascii_whitespace()
        .filter_map(|p| p.parse().ok())
    {
        let comm = std::fs::read_to_string(format!("/proc/{child}/comm")).unwrap_or_default();
        if comm.trim() != "bwrap" {
            return Some(child);
        }
        if let Some(found) = descendant(child, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// The `rchar` line of what `/proc/<pid>/io` holds: bytes the process has read.
fn rchar(io: &str) -> Option<u64> {
    io.lines()
        .find_map(|line| line.strip_prefix("rchar:"))?
        .trim()
        .parse()
        .ok()
}

/// What a helper left at `path`, in the directory it could write to, read without taking
/// its word for anything: the name must be a regular file of at most `limit` bytes. A
/// helper that has been taken over by the file it was reading could leave a link to one of
/// the user's files there instead, for this process to read or write through, or a pipe
/// that would hold the reader for good.
pub(crate) fn read_output(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// A new directory in the temporary one that only this user can enter: what is written
/// there is drawn from the user's own files. Not `create_dir_all`: a name another process
/// got to first is refused, not adopted.
pub(crate) fn private_dir(prefix: &str) -> Option<std::path::PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(&dir)
        .ok()?;
    Some(dir)
}

/// Run `program` over `input` the way a thumbnailer runs: inside bubblewrap, with the file
/// bound read-only and one private directory to write into, for at most `limit`. `args` is
/// handed the paths as the child sees them, `result` what it printed and the directory it
/// wrote to, which is removed as soon as `result` returns. Without bubblewrap the tool is
/// not run: it is fed a file from wherever the reader got it.
pub(crate) fn run_tool<T>(
    program: &str,
    input: &std::path::Path,
    limit: std::time::Duration,
    args: impl FnOnce(&std::path::Path, &std::path::Path) -> Vec<std::ffi::OsString>,
    result: impl FnOnce(&[u8], &std::path::Path) -> Option<T>,
) -> Option<T> {
    use std::path::Path;
    let program = glib::find_program_in_path(program)?;
    let work = private_dir("spiral-tool")?;
    let sandbox = match command(&program.to_string_lossy()) {
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
    // Bounded like a thumbnailer: a file that stops the tool would otherwise leave the
    // worker thread on the tool, for good.
    let run = run_bounded(&mut cmd, limit);
    // The seccomp memfd must stay open until the child has started.
    drop(sandbox.seccomp);
    let out = match run {
        Ok(ran) if ran.ok => result(&ran.stdout, &work),
        Ok(ran) => {
            glib::g_debug!("spiral", "tool {program:?} failed: {}", ran.trouble);
            None
        }
        Err(e) => {
            glib::g_debug!("spiral", "tool {program:?} could not start: {e}");
            None
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    out
}

/// What a bounded run left behind.
pub(crate) struct Ran {
    pub ok: bool,
    /// Killed at the limit rather than finished.
    pub timed_out: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What the kernel says a process has read, and nothing taken for it.
    #[test]
    fn what_a_process_has_read() {
        assert_eq!(rchar("rchar: 4096\nwchar: 7\n"), Some(4096));
        assert_eq!(rchar("syscr: 12\nrchar: 0\n"), Some(0));
        assert_eq!(rchar("wchar: 9\n"), None);
        assert_eq!(rchar(""), None);
    }

    /// Only a regular file is read back: a link or a pipe left in its place is refused.
    #[test]
    fn output_is_read_only_from_a_regular_file() {
        let dir = std::env::temp_dir().join(format!("spiral-output-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let (plain, link, pipe) = (dir.join("plain"), dir.join("link"), dir.join("pipe"));
        std::fs::write(&plain, b"pixels").unwrap();
        std::os::unix::fs::symlink(&plain, &link).unwrap();
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        assert_eq!(read_output(&plain, 64).as_deref(), Some(&b"pixels"[..]));
        assert_eq!(read_output(&plain, 3), None, "larger than the limit");
        assert_eq!(read_output(&link, 64), None);
        assert_eq!(read_output(&pipe, 64), None);
        assert_eq!(read_output(&dir, 64), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
