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
                "the bubblewrap sandbox does not work here, so thumbnails, PDF previews \
                 and archive operations are turned off: {trouble}"
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
