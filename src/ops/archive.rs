//! Extract and create archives with whatever command line tools are installed: 7-Zip,
//! bsdtar, GNU tar, unzip/zip, unrar/unar. Nothing is linked; a missing tool only removes
//! the formats it would have handled. Tools run inside the same bwrap sandbox as
//! thumbnailers when it is available. Passwords are never written down or logged. 7-Zip is
//! told them on its standard input; the other tools only take one on the command line,
//! where other processes can read it while they run, so 7-Zip is used wherever it can be.

use std::collections::HashMap;
use std::io::Seek;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::gtk::subclass::prelude::ObjectSubclassIsExt;
use crate::ops::job::{Job, JobStatus, name};
use crate::ops::manager::JobManager;
use crate::ops::walk::Fail;
use crate::{adw, gio, glib, gtk};

const PRIO: glib::Priority = glib::Priority::DEFAULT;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    SevenZip,
    Bsdtar,
    Tar,
    Unzip,
    Zip,
    Unrar,
    Unar,
}

impl Tool {
    /// Executable path, first candidate name found in PATH.
    fn path(self) -> Option<PathBuf> {
        let names: &[&str] = match self {
            Tool::SevenZip => &["7zz", "7z", "7za"],
            Tool::Bsdtar => &["bsdtar"],
            Tool::Tar => &["tar"],
            Tool::Unzip => &["unzip"],
            Tool::Zip => &["zip"],
            Tool::Unrar => &["unrar"],
            Tool::Unar => &["unar"],
        };
        names.iter().find_map(glib::find_program_in_path)
    }

    /// Package hint for the toast when nothing can open a format.
    fn hint(self) -> &'static str {
        match self {
            Tool::SevenZip => "7-Zip (7zz)",
            Tool::Bsdtar => "bsdtar",
            Tool::Tar => "tar",
            Tool::Unzip => "unzip",
            Tool::Zip => "zip",
            Tool::Unrar => "unrar",
            Tool::Unar => "unar",
        }
    }
}

/// Extractors able to read `mime`, in order of preference.
fn extractors(mime: &str) -> &'static [Tool] {
    const TAR: &[Tool] = &[Tool::Tar, Tool::Bsdtar];
    let is = |t: &str| gio::content_type_is_a(mime, t);
    if is("application/zip") {
        &[Tool::SevenZip, Tool::Bsdtar, Tool::Unzip]
    } else if is("application/x-7z-compressed") {
        &[Tool::SevenZip, Tool::Bsdtar]
    } else if is("application/vnd.rar") || is("application/x-rar") {
        &[Tool::SevenZip, Tool::Unrar, Tool::Unar, Tool::Bsdtar]
    } else if is("application/x-tar") {
        &[Tool::Tar, Tool::Bsdtar, Tool::SevenZip]
    } else if [
        "application/x-compressed-tar",
        "application/x-bzip-compressed-tar",
        "application/x-xz-compressed-tar",
        "application/x-zstd-compressed-tar",
        "application/x-lzma-compressed-tar",
        "application/x-lzip-compressed-tar",
        "application/x-lz4-compressed-tar",
    ]
    .iter()
    .any(|t| is(t))
    {
        TAR
    } else if [
        "application/gzip",
        "application/x-xz",
        "application/x-bzip2",
        "application/zstd",
        "application/x-lzma",
    ]
    .iter()
    .any(|t| is(t))
    {
        &[Tool::SevenZip]
    } else if [
        "application/x-iso9660-image",
        "application/x-cd-image",
        "application/vnd.debian.binary-package",
        "application/x-rpm",
        "application/vnd.ms-cab-compressed",
        "application/x-cpio",
        "application/x-lha",
        "application/x-arj",
        "application/x-xar",
    ]
    .iter()
    .any(|t| is(t))
    {
        &[Tool::SevenZip, Tool::Bsdtar]
    } else {
        &[]
    }
}

/// Whether `mime` is an archive format Spiral knows how to extract, tools permitting.
/// How the folders an archive is unpacked into, and packed in, start their names: beside
/// where the result goes, so it gets there by a rename.
const EXTRACT_WORK: &str = ".spiral-extract-";
const COMPRESS_WORK: &str = ".spiral-compress-";

/// Whether `info` is one of those folders, which are not worth showing, hidden files or
/// not: they come and go with the operation.
pub fn is_work_folder(info: &gio::FileInfo) -> bool {
    let name = info.name();
    let name = name.to_string_lossy();
    info.file_type() == gio::FileType::Directory
        && (name.starts_with(EXTRACT_WORK) || name.starts_with(COMPRESS_WORK))
}

pub fn is_archive(mime: &str) -> bool {
    !extractors(mime).is_empty()
}

/// An archive format that can be created with the installed tools.
#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub extension: &'static str,
    pub description: String,
}

pub fn creatable_formats() -> Vec<Format> {
    let has = |t: Tool| t.path().is_some();
    let compressor = |n: &str| glib::find_program_in_path(n).is_some();
    let mut out = Vec::new();
    if has(Tool::Zip) || has(Tool::SevenZip) {
        out.push(Format {
            extension: ".zip",
            description: gettext("Opens on any system without extra software"),
        });
    }
    if has(Tool::Tar) && compressor("xz") {
        out.push(Format {
            extension: ".tar.xz",
            description: gettext("Small archives, slower to create"),
        });
    }
    if has(Tool::Tar) && compressor("zstd") {
        out.push(Format {
            extension: ".tar.zst",
            description: gettext("Fast to create and to extract"),
        });
    }
    if has(Tool::SevenZip) {
        out.push(Format {
            extension: ".7z",
            description: gettext("Small archives, may need an extra program to open"),
        });
    }
    if has(Tool::Tar) && compressor("gzip") {
        out.push(Format {
            extension: ".tar.gz",
            description: gettext("Widely supported, larger archives"),
        });
    }
    out
}

/// The archive's name without its (possibly double) extension.
pub fn stem(file_name: &str) -> &str {
    let mut stem = file_name;
    if let Some(i) = stem.rfind('.').filter(|&i| i > 0) {
        stem = &stem[..i];
    }
    if let Some(s) = stem.strip_suffix(".tar") {
        stem = s;
    }
    stem
}

/// Name in `dir` not taken yet: `name`, then `name (2)`, `name (3)`, ...
fn unique_name(dir: &Path, name: &str, ext: &str) -> String {
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            format!("{name}{ext}")
        } else {
            format!("{name} ({n}){ext}")
        };
        if !dir.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Kills the child and removes the work directory if the job is dropped mid-way.
struct Guard {
    child: Option<gio::Subprocess>,
    work: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(child) = self.child.take()
            && !child.has_exited()
        {
            child.force_exit();
        }
        let work = self.work.clone();
        std::thread::spawn(move || std::fs::remove_dir_all(work));
    }
}

/// One child process: argv, extra sandbox binds, and working directory outside the sandbox.
struct Command {
    argv: Vec<String>,
    /// (host path, sandbox path, writable)
    binds: Vec<(PathBuf, PathBuf, bool)>,
    cwd: PathBuf,
    /// What the tool reads on its standard input: a password, for 7-Zip.
    stdin: Option<String>,
    /// A file for its standard input instead: the archive, for tar.
    input: Option<OwnedFd>,
}

/// What one of tar's checkpoints stands for, a record.
const RECORD: u64 = 10240;

/// A 7-Zip style "NN%" progress line.
fn percent(line: &str) -> Option<f64> {
    line.split_once('%')?.0.trim().parse().ok()
}

/// The number of records in one of tar's checkpoint lines, "tar: #100".
fn checkpoint(line: &str) -> Option<u64> {
    line.rsplit_once(": #")?.1.parse().ok()
}

/// Run `cmd`; every output line goes to `progress`, and those it does not claim are kept as
/// the error text.
async fn run(
    cmd: Command,
    guard: &mut Guard,
    progress: &mut dyn FnMut(&str) -> bool,
) -> Result<(), Fail> {
    let mut argv: Vec<String> = Vec::new();
    let mut flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE;
    if cmd.stdin.is_some() {
        flags |= gio::SubprocessFlags::STDIN_PIPE;
    }
    let launcher = gio::SubprocessLauncher::new(flags);
    if let Some(input) = cmd.input {
        launcher.take_stdin_fd(Some(input));
    }
    // An archive is a file from anywhere and its tool is a parser: no sandbox, no run.
    let Some(sandbox) = crate::sandbox::command(&cmd.argv[0]) else {
        return Err(Fail::Failed(gettext(
            "bubblewrap is not installed, so archive tools cannot be run safely",
        )));
    };
    argv.extend(sandbox.argv);
    for (host, inner, writable) in &cmd.binds {
        argv.push(if *writable { "--bind" } else { "--ro-bind" }.into());
        argv.push(host.to_string_lossy().into_owned());
        argv.push(inner.to_string_lossy().into_owned());
    }
    argv.push("--chdir".into());
    argv.push(cmd.cwd.to_string_lossy().into_owned());
    argv.push("--".into());
    let seccomp = OwnedFd::from(sandbox.seccomp);
    launcher.take_fd(
        seccomp
            .try_clone()
            .map_err(|e| Fail::Failed(e.to_string()))?,
        &seccomp,
    );
    argv.extend(cmd.argv);
    let os_argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let child = launcher
        .spawn(&os_argv)
        .map_err(|e| Fail::Failed(e.message().to_string()))?;
    drop(seccomp);
    guard.child = Some(child.clone());
    if let (Some(input), Some(stdin)) = (&cmd.stdin, child.stdin_pipe()) {
        let _ = stdin
            .write_all_future(input.clone().into_bytes(), glib::Priority::DEFAULT)
            .await;
        let _ = stdin.close_future(glib::Priority::DEFAULT).await;
    }

    // Merged output: progress lines, everything else kept as the error text.
    let mut tail: Vec<String> = Vec::new();
    let mut pending = String::new();
    if let Some(stdout) = child.stdout_pipe() {
        let mut buf = vec![0u8; 4096];
        loop {
            let (b, n) = match stdout.read_future(buf, PRIO).await {
                Ok(r) => r,
                Err(_) => break,
            };
            buf = b;
            if n == 0 {
                break;
            }
            // 7-Zip redraws its percentage with backspaces; treat those as line breaks.
            pending.push_str(&String::from_utf8_lossy(&buf[..n]).replace('\u{8}', "\n"));
            // The last piece may be a partial line; keep it for the next read.
            let Some(cut) = pending.rfind(['\r', '\n']) else {
                continue;
            };
            let rest = pending.split_off(cut + 1);
            for piece in pending.split(['\r', '\n']) {
                let line = piece.trim();
                if line.is_empty() {
                    continue;
                }
                if !progress(line) {
                    tail.push(line.to_string());
                    if tail.len() > 20 {
                        tail.remove(0);
                    }
                }
            }
            pending = rest;
        }
        if !pending.trim().is_empty() {
            tail.push(pending.trim().to_string());
        }
    }
    match child.wait_check_future().await {
        Ok(()) => Ok(()),
        Err(_) if child.has_signaled() => Err(Fail::Cancelled),
        Err(e) => {
            let msg = tail
                .iter()
                .rev()
                .find(|l| !l.starts_with("Scanning") && !l.chars().all(|c| c.is_ascii_digit()))
                .cloned()
                .unwrap_or_else(|| e.message().to_string());
            Err(Fail::Failed(msg))
        }
    }
}

fn no_tool(file: &gio::File, tools: &[Tool]) -> Fail {
    let hints: Vec<&str> = tools.iter().map(|t| t.hint()).collect();
    Fail::Failed(
        gettext("Cannot extract “%s”: install %t")
            .replace("%s", &name(file))
            .replace("%t", &hints.join(", ")),
    )
}

/// Ask for the password of `archive`; None if cancelled.
async fn ask_password(parent: &gtk::Window, archive: &gio::File) -> Option<String> {
    let entry = adw::PasswordEntryRow::builder()
        .title(gettext("_Password"))
        .use_underline(true)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&entry);
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Enter Password"))
        .body(gettext("“%s” is password protected").replace("%s", &name(archive)))
        .extra_child(&list)
        .close_response("cancel")
        .default_response("extract")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("extract", &gettext("_Extract")),
    ]);
    dialog.set_response_appearance("extract", adw::ResponseAppearance::Suggested);
    entry.connect_entry_activated(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.emit_by_name::<()>("response", &[&"extract"]);
            dialog.close();
        }
    ));
    if dialog.choose_future(Some(parent)).await != "extract" {
        return None;
    }
    Some(entry.text().to_string())
}

/// What tells GNU tar how an archive of `mime` is compressed, which it does not find out
/// for itself when the archive is its standard input. None where that is not known.
fn tar_filter(mime: &str) -> Option<&'static [&'static str]> {
    const FILTERS: &[(&str, &[&str])] = &[
        ("application/x-compressed-tar", &["-z"]),
        ("application/x-bzip-compressed-tar", &["-j"]),
        ("application/x-xz-compressed-tar", &["-J"]),
        ("application/x-zstd-compressed-tar", &["--zstd"]),
        ("application/x-lzma-compressed-tar", &["--lzma"]),
        ("application/x-lzip-compressed-tar", &["--lzip"]),
        ("application/x-lz4-compressed-tar", &["-I", "lz4"]),
        ("application/x-tar", &[]),
    ];
    FILTERS
        .iter()
        .find(|(t, _)| gio::content_type_is_a(mime, t))
        .map(|(_, filter)| *filter)
}

/// Whether a tool's output line says the archive wanted a password it did not get.
fn wants_password(line: &str) -> bool {
    let line = line.to_lowercase();
    ["password", "passphrase", "encrypted"]
        .iter()
        .any(|w| line.contains(w))
}

/// Extract each archive into `dest`. A single top-level entry lands as itself, anything
/// else inside a folder named after the archive; existing names are never overwritten.
/// An encrypted archive asks for its password and is retried until it opens or the user
/// gives up.
pub async fn extract(
    job: &Job,
    mgr: &JobManager,
    archives: Vec<gio::File>,
    dest: gio::File,
) -> Result<(), Fail> {
    let Some(dest_path) = dest.path() else {
        return Err(Fail::Failed(gettext(
            "Archives can only be extracted to local folders",
        )));
    };
    // Progress is by the bytes of the archives themselves, as far as the tool has read.
    let mut sizes = Vec::new();
    for archive in &archives {
        let info = archive
            .query_info_future("standard::size", gio::FileQueryInfoFlags::NONE, PRIO)
            .await;
        sizes.push(info.map_or(0, |i| crate::file_utils::size_of(&i)));
    }
    job.set_bytes_total(sizes.iter().sum::<u64>());
    job.start_clock();
    let mut before = 0u64;
    for (i, archive) in archives.iter().enumerate() {
        let size = sizes[i];
        let Some(path) = archive.path() else {
            before += size;
            continue;
        };
        let info = archive
            .query_info_future(
                "standard::content-type",
                gio::FileQueryInfoFlags::NONE,
                PRIO,
            )
            .await
            .map_err(|e| Fail::Failed(e.message().to_string()))?;
        let mime = crate::file_utils::content_type_of(&info).unwrap_or_default();
        let tools = extractors(&mime);
        let Some((tool, exe)) = tools.iter().find_map(|t| t.path().map(|p| (*t, p))) else {
            return Err(no_tool(archive, tools));
        };
        job.set_detail(gettext("Extracting “%s”").replace("%s", &name(archive)));
        // tar reads the archive on its standard input, a file of ours, and where that
        // file stands is how far tar is.
        let filter = (tool == Tool::Tar).then(|| tar_filter(&mime)).flatten();

        let work = dest_path.join(format!("{EXTRACT_WORK}{}-{i}", std::process::id()));
        on_disk({
            let work = work.clone();
            move || std::fs::create_dir(work)
        })
        .await?;
        let mut guard = Guard {
            child: None,
            work: work.clone(),
        };
        let sandboxed = glib::find_program_in_path("bwrap").is_some();
        let file_name = name(archive);
        let (input, out) = if sandboxed {
            (
                PathBuf::from("/tmp/in").join(&file_name),
                PathBuf::from("/tmp/out"),
            )
        } else {
            (path.clone(), work.clone())
        };
        let exe = exe.to_string_lossy().into_owned();
        let (input_s, out_s) = (
            input.to_string_lossy().into_owned(),
            out.to_string_lossy().into_owned(),
        );
        // Every tool is told the password up front, an empty one on the first try, so an
        // encrypted archive fails at once instead of waiting on a terminal that is not
        // there. What it says on the way out is what triggers the prompt.
        let argv = |password: Option<&str>| -> Vec<String> {
            let pass = password.unwrap_or("");
            match tool {
                // No -p: the password comes on standard input, see `stdin` below.
                Tool::SevenZip => vec![
                    exe.clone(),
                    "x".into(),
                    "-y".into(),
                    "-bso0".into(),
                    "-bsp1".into(),
                    format!("-o{out_s}"),
                    input_s.clone(),
                ],
                Tool::Tar if filter.is_some() => {
                    let mut argv = vec![exe.clone(), "-x".into()];
                    argv.extend(filter.into_iter().flatten().map(|f| f.to_string()));
                    argv.extend(["-f".into(), "-".into(), "-C".into(), out_s.clone()]);
                    argv
                }
                Tool::Bsdtar | Tool::Tar => {
                    let mut argv = vec![
                        exe.clone(),
                        "-xf".into(),
                        input_s.clone(),
                        "-C".into(),
                        out_s.clone(),
                    ];
                    if let (Tool::Bsdtar, Some(pass)) = (tool, password) {
                        argv.extend(["--passphrase".into(), pass.into()]);
                    }
                    argv
                }
                // Not -q: the "incorrect password" line is the one thing worth reading.
                Tool::Unzip => vec![
                    exe.clone(),
                    "-o".into(),
                    "-P".into(),
                    pass.into(),
                    input_s.clone(),
                    "-d".into(),
                    out_s.clone(),
                ],
                Tool::Unrar => vec![
                    exe.clone(),
                    "x".into(),
                    "-o+".into(),
                    "-idq".into(),
                    password.map_or("-p-".into(), |p| format!("-p{p}")),
                    input_s.clone(),
                    format!("{out_s}/"),
                ],
                Tool::Unar => vec![
                    exe.clone(),
                    "-q".into(),
                    "-D".into(),
                    "-p".into(),
                    pass.into(),
                    "-o".into(),
                    out_s.clone(),
                    input_s.clone(),
                ],
                Tool::Zip => unreachable!(),
            }
        };
        let mut password: Option<String> = None;
        loop {
            let reading = match filter {
                Some(_) => Some(
                    on_disk({
                        let path = path.clone();
                        move || std::fs::File::open(path)
                    })
                    .await?,
                ),
                None => None,
            };
            let handed = match &reading {
                Some(file) => Some(OwnedFd::from(
                    file.try_clone().map_err(|e| Fail::Failed(e.to_string()))?,
                )),
                None => None,
            };
            let cmd = Command {
                argv: argv(password.as_deref()),
                binds: vec![
                    (path.clone(), input.clone(), false),
                    (work.clone(), out.clone(), true),
                ],
                cwd: out.clone(),
                // 7-Zip asks for the password when it finds the archive wants one, and
                // reads it here; an empty one fails at once, as on the command line.
                stdin: (tool == Tool::SevenZip)
                    .then(|| format!("{}\n", password.as_deref().unwrap_or(""))),
                input: handed,
            };
            let mut encrypted = false;
            let result = {
                let mut progress = |line: &str| {
                    encrypted |= wants_password(line);
                    percent(line)
                        .map(|p| {
                            job.set_bytes_done(
                                before + (size as f64 * p.min(100.0) / 100.0) as u64,
                            );
                            job.report(false);
                        })
                        .is_some()
                };
                let ran = run(cmd, &mut guard, &mut progress);
                match reading.as_ref() {
                    Some(mut file) => {
                        let ticks =
                            glib::interval_stream(Duration::from_millis(100)).for_each(|()| {
                                if let Ok(at) = file.stream_position() {
                                    job.set_bytes_done(before + at.min(size));
                                    job.report(false);
                                }
                                std::future::ready(())
                            });
                        futures_util::pin_mut!(ran);
                        match select(ran, ticks).await {
                            Either::Left((result, _)) => result,
                            Either::Right(((), ran)) => ran.await,
                        }
                    }
                    None => ran.await,
                }
            };
            match result {
                Ok(()) => break,
                Err(Fail::Failed(_)) if encrypted => {}
                Err(e) => return Err(e),
            }
            guard.child = None;
            // A wrong password can leave empty or garbled files behind.
            on_disk({
                let work = work.clone();
                move || std::fs::remove_dir_all(&work).and_then(|()| std::fs::create_dir(&work))
            })
            .await?;
            job.set_status(JobStatus::WaitingUser);
            job.set_detail(gettext("Waiting for your answer"));
            let answer = ask_password(&mgr.parent_window(job), archive).await;
            job.set_status(JobStatus::Running);
            job.set_detail(gettext("Extracting “%s”").replace("%s", &name(archive)));
            password = Some(answer.ok_or(Fail::Cancelled)?);
        }
        guard.child = None;

        let created = on_disk({
            let (work, dest_path) = (work.clone(), dest_path.clone());
            move || place(&work, &dest_path, &file_name)
        })
        .await?;
        drop(guard);
        job.imp()
            .outcome
            .borrow_mut()
            .created
            .push(gio::File::for_path(created));
        job.set_files_done(i as u64 + 1);
        before += size;
        job.set_bytes_done(before);
        job.report(true);
    }
    Ok(())
}

/// Move what an archive left in `work` to its final name in `dest`: a single entry
/// keeps its own name, anything else is a folder named after the archive.
fn place(work: &Path, dest: &Path, file_name: &str) -> std::io::Result<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(work)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let target = match entries.as_slice() {
        [single] => {
            let entry_name = single.file_name().unwrap().to_string_lossy().into_owned();
            let (base, ext) = match entry_name.rfind('.').filter(|&i| i > 0 && single.is_file()) {
                Some(i) => (&entry_name[..i], &entry_name[i..]),
                None => (entry_name.as_str(), ""),
            };
            let target = dest.join(unique_name(dest, base, ext));
            std::fs::rename(single, &target)?;
            target
        }
        _ => {
            let target = dest.join(unique_name(dest, stem(file_name), ""));
            std::fs::rename(work, &target)?;
            target
        }
    };
    Ok(target)
}

/// One step on the disk, off the main thread: the destination may be a network mount,
/// where every call is a round trip the window should not wait on.
async fn on_disk<T: Send + 'static>(
    step: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, Fail> {
    gio::spawn_blocking(step)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("interrupted")))
        .map_err(|e| Fail::Failed(e.to_string()))
}

fn sizes(path: &Path, rel: String, out: &mut HashMap<String, u64>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        for e in std::fs::read_dir(path).into_iter().flatten().flatten() {
            let rel = format!("{rel}/{}", e.file_name().to_string_lossy());
            sizes(&e.path(), rel, out);
        }
        out.insert(rel, 0);
    } else {
        out.insert(rel, meta.len());
    }
}

/// What tar writes for an entry that holds `size` bytes: a header, and the file filled up
/// to whole blocks. Its checkpoints count what it writes.
fn in_tar(size: u64) -> u64 {
    512 + size.div_ceil(512) * 512
}

/// Pack `files` (siblings in one folder) into `dest/<file_name>`, encrypted with `password`
/// if there is one.
pub async fn compress(
    job: &Job,
    files: Vec<gio::File>,
    dest: gio::File,
    file_name: String,
    password: Option<String>,
) -> Result<(), Fail> {
    let (Some(dest_path), Some(first)) = (dest.path(), files.first()) else {
        return Err(Fail::Failed(gettext(
            "Archives can only be created in local folders",
        )));
    };
    let Some(src_dir) = first.parent().and_then(|p| p.path()) else {
        return Err(Fail::Failed(gettext(
            "Archives can only be created from local files",
        )));
    };
    let ext = &file_name[stem(&file_name).len()..];
    let seven = Tool::SevenZip.path();
    let (tool, exe) = match ext {
        // With a password 7-Zip is preferred: it encrypts zips with AES, zip itself only
        // knows the old PKWARE scheme.
        ".zip" => match (Tool::Zip.path(), &seven) {
            (Some(p), _) if password.is_none() => (Tool::Zip, p),
            (_, Some(p)) => (Tool::SevenZip, p.clone()),
            (Some(p), None) => (Tool::Zip, p),
            _ => {
                return Err(Fail::Failed(gettext(
                    "Creating zip archives needs zip or 7-Zip",
                )));
            }
        },
        ".7z" => match seven {
            Some(p) => (Tool::SevenZip, p),
            None => {
                return Err(Fail::Failed(gettext(
                    "7-Zip is required to create 7z archives",
                )));
            }
        },
        _ => match Tool::Tar.path() {
            Some(p) => (Tool::Tar, p),
            None => {
                return Err(Fail::Failed(gettext(
                    "tar is required to create tar archives",
                )));
            }
        },
    };
    job.set_detail(gettext("Compressing…"));
    let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
    let mut sizes = gio::spawn_blocking(move || {
        let mut out = HashMap::new();
        for p in &paths {
            sizes(
                p,
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                &mut out,
            );
        }
        out
    })
    .await
    .unwrap_or_default();
    let total: u64 = match tool {
        Tool::Tar => sizes.values().map(|size| in_tar(*size)).sum(),
        _ => sizes.values().sum(),
    };
    job.set_bytes_total(total);
    job.start_clock();

    let work = dest_path.join(format!("{COMPRESS_WORK}{}", std::process::id()));
    let final_name = on_disk({
        let (work, dest_path, file_name) = (work.clone(), dest_path.clone(), file_name.clone());
        move || {
            std::fs::create_dir(&work)?;
            let stem = stem(&file_name);
            Ok(unique_name(&dest_path, stem, &file_name[stem.len()..]))
        }
    })
    .await?;
    let mut guard = Guard {
        child: None,
        work: work.clone(),
    };
    let sandboxed = glib::find_program_in_path("bwrap").is_some();
    let (src_root, out_dir) = if sandboxed {
        (PathBuf::from("/tmp/src"), PathBuf::from("/tmp/out"))
    } else {
        (src_dir.clone(), work.clone())
    };
    let out = out_dir.join(&final_name).to_string_lossy().into_owned();
    let names: Vec<String> = files.iter().map(name).collect();
    let exe = exe.to_string_lossy().into_owned();
    let mut argv: Vec<String> = match (tool, ext) {
        (Tool::Zip, _) => vec![exe, "-r".into(), "-y".into()],
        (Tool::SevenZip, ".zip") => vec![
            exe,
            "a".into(),
            "-tzip".into(),
            "-bso0".into(),
            "-bsp1".into(),
        ],
        (Tool::SevenZip, _) => vec![exe, "a".into(), "-bso0".into(), "-bsp1".into()],
        // A line for every hundred records written, which no name in the archive can be
        // taken for.
        _ => vec![
            exe,
            "--checkpoint=100".into(),
            "--checkpoint-action=echo=#%u".into(),
            "-caf".into(),
        ],
    };
    match (&password, tool, ext) {
        (Some(pass), Tool::Zip, _) => argv.extend(["-P".into(), pass.clone()]),
        // A bare -p makes 7-Zip read the password from standard input.
        (Some(_), Tool::SevenZip, ".zip") => argv.extend(["-p".into(), "-mem=AES256".into()]),
        (Some(_), Tool::SevenZip, _) => argv.push("-p".into()),
        _ => {}
    }
    argv.push(out);
    // Names are the files' own: one that starts with a dash is a name, not an option.
    argv.push("--".into());
    argv.extend(names.iter().cloned());
    let mut binds: Vec<(PathBuf, PathBuf, bool)> = files
        .iter()
        .filter_map(|f| f.path())
        .map(|p| {
            let inner = src_root.join(p.file_name().unwrap_or_default());
            (p, inner, false)
        })
        .collect();
    binds.push((work.clone(), out_dir, true));
    let cmd = Command {
        argv,
        binds,
        cwd: src_root,
        stdin: password
            .filter(|_| tool == Tool::SevenZip)
            .map(|pass| format!("{pass}\n")),
        input: None,
    };
    // 7-Zip reports a percentage and tar the number of records written ("tar: #100"). zip
    // names each entry once it is packed ("adding: a/b (deflated 3%)"), which the size
    // table turns into bytes; the dots it can be asked for count what it writes, which
    // says nothing of how much is left to read.
    let mut done = 0u64;
    let mut progress = |line: &str| {
        let at = match tool {
            Tool::SevenZip => percent(line).map(|p| (total as f64 * p / 100.0) as u64),
            Tool::Tar => checkpoint(line).map(|records| records * RECORD),
            _ => {
                let entry = line
                    .strip_prefix("adding: ")
                    .and_then(|r| r.rsplit_once(" ("))
                    .map(|(n, _)| n.trim_end_matches('/'));
                entry.and_then(|n| sizes.remove(n)).map(|size| done + size)
            }
        };
        if let Some(at) = at {
            done = at.min(total);
            job.set_bytes_done(done);
            job.report(false);
        }
        at.is_some()
    };
    run(cmd, &mut guard, &mut progress).await?;
    guard.child = None;
    let target = dest_path.join(&final_name);
    on_disk({
        let (from, target) = (work.join(&final_name), target.clone());
        move || std::fs::rename(from, target)
    })
    .await?;
    drop(guard);
    job.imp()
        .outcome
        .borrow_mut()
        .created
        .push(gio::File::for_path(target));
    job.set_bytes_done(total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::gio;
    #[test]
    fn archive_mimes() {
        assert!(super::is_archive("application/zip"));
        assert!(super::is_archive("application/x-compressed-tar"));
        assert!(!super::is_archive("text/plain"));
        assert!(!super::is_archive("inode/directory"));
        for t in [
            "application/zip",
            "application/x-7z-compressed",
            "application/vnd.rar",
            "application/x-tar",
            "application/x-compressed-tar",
            "application/gzip",
            "application/x-iso9660-image",
            "application/vnd.debian.binary-package",
            "application/x-rpm",
            "application/vnd.ms-cab-compressed",
            "application/x-cpio",
            "application/x-lha",
            "application/x-arj",
            "application/x-xar",
            "application/x-lzma",
            "application/zstd",
            "application/x-lz4-compressed-tar",
        ] {
            assert!(!gio::content_type_is_a("inode/directory", t), "{t}");
        }
        assert_eq!(super::stem("a.tar.gz"), "a");
        assert_eq!(super::stem("b.zip"), "b");
    }

    #[test]
    fn what_tar_says_and_is_told() {
        assert_eq!(super::checkpoint("/usr/bin/tar: #300"), Some(300));
        assert_eq!(super::checkpoint("tar: a #1: Cannot open"), None);
        assert_eq!(super::in_tar(0), 512);
        assert_eq!(super::in_tar(513), 512 + 1024);
        assert_eq!(
            super::tar_filter("application/x-xz-compressed-tar"),
            Some(&["-J"][..])
        );
        assert_eq!(super::tar_filter("application/x-tar"), Some(&[][..]));
        assert_eq!(super::tar_filter("application/zip"), None);
    }
}
