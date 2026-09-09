//! The actual I/O of a job: counting, recursive transfer/delete, trash, rename, create, restore.

use futures_util::StreamExt;
use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::gtk::subclass::prelude::ObjectSubclassIsExt;
use crate::ops::conflict::{ErrorChoice, ask_conflict, ask_error};
use crate::ops::job::{Job, JobKind, Resolution, name};
use crate::ops::manager::JobManager;
use crate::{adw, gio, glib};

const PRIO: glib::Priority = glib::Priority::DEFAULT;
const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

pub enum Fail {
    Cancelled,
    Failed(String),
}

type Res<T> = Result<T, Fail>;

pub async fn run(job: &Job, mgr: &JobManager) -> Res<()> {
    match job.kind() {
        JobKind::Transfer { pairs, is_move } => {
            count(job, pairs.iter().map(|(s, _)| s.clone()).collect()).await;
            let mut warned_recursive = false;
            for (src, dest_dir) in pairs {
                let same_parent = src.parent().is_some_and(|p| p.equal(&dest_dir));
                if same_parent && is_move {
                    continue;
                }
                if dest_dir.equal(&src) || dest_dir.has_prefix(&src) {
                    if !warned_recursive {
                        warned_recursive = true;
                        let dialog = adw::AlertDialog::builder()
                            .heading(if is_move {
                                gettext("You cannot move a folder into itself.")
                            } else {
                                gettext("You cannot copy a folder into itself.")
                            })
                            .body(gettext(
                                "The destination folder is inside the source folder.",
                            ))
                            .build();
                        dialog.add_response("ok", &gettext("_OK"));
                        dialog.choose_future(Some(&mgr.parent_window())).await;
                    }
                    continue;
                }
                let dest_name = if same_parent {
                    unique_copy_name(&dest_dir, &name(&src)).await
                } else {
                    name(&src)
                };
                let dest =
                    transfer_one(job, mgr, &src, &dest_dir, &dest_name, is_move, true).await?;
                if let Some(dest) = dest {
                    let mut out = job.imp().outcome.borrow_mut();
                    if is_move {
                        out.moved.push((src.clone(), dest));
                    } else {
                        out.created.push(dest);
                    }
                }
            }
        }
        JobKind::Trash { files } => {
            job.set_files_total(files.len() as u64);
            let mut delete_instead = false;
            for f in files {
                loop {
                    let r = if delete_instead {
                        delete_recursive(job, mgr, &f).await.map(|_| ())
                    } else {
                        Ok(())
                    };
                    r?;
                    if delete_instead {
                        break;
                    }
                    match f.trash_future(PRIO).await {
                        Ok(()) => {
                            job.imp().outcome.borrow_mut().trashed.push(f.clone());
                            break;
                        }
                        Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => {
                            if !confirm_permanent_delete(mgr, &f).await {
                                return Err(Fail::Cancelled);
                            }
                            delete_instead = true;
                        }
                        Err(e) => {
                            // Translators: fills %v in “Error While %v “%s””.
                            match ask_error(&mgr.parent_window(), &gettext("Trashing"), &f, &e)
                                .await
                            {
                                ErrorChoice::Skip => break,
                                ErrorChoice::Retry => continue,
                                ErrorChoice::Cancel => return Err(Fail::Cancelled),
                            }
                        }
                    }
                }
                job.set_files_done(job.files_done() + 1);
                job.report(true);
            }
        }
        JobKind::Delete { files } => {
            count(job, files.clone()).await;
            for f in files {
                delete_recursive(job, mgr, &f).await?;
            }
        }
        JobKind::Rename { renames } => {
            let single = renames.len() == 1;
            job.set_files_total(renames.len() as u64);
            for (file, new_name) in renames {
                let old = name(&file);
                loop {
                    match file.set_display_name_future(&new_name, PRIO).await {
                        Ok(new_file) => {
                            crate::tags::relocate(&file, &new_file);
                            job.imp().outcome.borrow_mut().renamed.push((new_file, old));
                            break;
                        }
                        // One name was typed for one file, so its answer is the whole
                        // job's; a batch asks, so the rest of it can go on.
                        Err(e) if single => return Err(Fail::Failed(e.message().to_string())),
                        Err(e) => {
                            // Translators: fills %v in “Error While %v “%s””.
                            match ask_error(&mgr.parent_window(), &gettext("Renaming"), &file, &e)
                                .await
                            {
                                ErrorChoice::Skip => break,
                                ErrorChoice::Retry => continue,
                                ErrorChoice::Cancel => return Err(Fail::Cancelled),
                            }
                        }
                    }
                }
                job.set_files_done(job.files_done() + 1);
                job.report(true);
            }
        }
        JobKind::CreateFolder { parent, name } => {
            let dir = parent.child(&name);
            dir.make_directory_future(PRIO)
                .await
                .map_err(|e| Fail::Failed(e.message().to_string()))?;
            job.imp().outcome.borrow_mut().created.push(dir);
        }
        JobKind::CreateFile {
            parent,
            name,
            template,
        } => {
            let file = parent.child(&name);
            match template {
                Some(template) => {
                    // Templates are documents to start from, so what the copy leaves is a
                    // file of its own: none of the source's times or permissions follow it.
                    let (copy, progress) =
                        template.copy_future(&file, gio::FileCopyFlags::NONE, PRIO);
                    let (result, ()) = futures_util::join!(copy, progress.for_each(|_| async {}));
                    result.map_err(|e| Fail::Failed(e.message().to_string()))?;
                }
                None => {
                    let stream = file
                        .create_future(gio::FileCreateFlags::NONE, PRIO)
                        .await
                        .map_err(|e| Fail::Failed(e.message().to_string()))?;
                    let _ = stream.close_future(PRIO).await;
                }
            }
            job.imp().outcome.borrow_mut().created.push(file);
        }
        JobKind::SaveImage { parent, image } => {
            let file = parent.child(&unique_image_name(&parent).await);
            let png = image.save_to_png_bytes();
            let stream = file
                .create_future(gio::FileCreateFlags::NONE, PRIO)
                .await
                .map_err(|e| Fail::Failed(e.message().to_string()))?;
            let write = stream.write_all_future(png.to_vec(), PRIO).await;
            let _ = stream.close_future(PRIO).await;
            match write {
                Ok((_, _, Some(e))) | Err((_, e)) => {
                    let _ = file.delete_future(PRIO).await;
                    return Err(Fail::Failed(e.message().to_string()));
                }
                Ok(_) => job.imp().outcome.borrow_mut().created.push(file),
            }
        }
        JobKind::Extract { archives, dest } => {
            job.set_files_total(archives.len() as u64);
            super::archive::extract(job, mgr, archives, dest).await?;
        }
        JobKind::Compress {
            files,
            dest,
            file_name,
            password,
        } => {
            super::archive::compress(job, files, dest, file_name, password).await?;
        }
        JobKind::Link { files, dest } => {
            job.set_files_total(files.len() as u64);
            for f in files {
                let Some(target) = f.path() else {
                    return Err(Fail::Failed(gettext("Links can only point at local files")));
                };
                let link = dest.child(&unique_link_name(&dest, &f).await);
                let target = target.to_string_lossy().into_owned();
                match link.make_symbolic_link_future(&target, PRIO).await {
                    Ok(()) => job.imp().outcome.borrow_mut().created.push(link),
                    Err(e) => return Err(Fail::Failed(e.message().to_string())),
                }
                job.set_files_done(job.files_done() + 1);
                job.report(true);
            }
        }
        JobKind::Restore { pairs } => {
            job.set_files_total(pairs.len() as u64);
            for (item, original) in pairs {
                if let Some(parent) = original.parent()
                    && !exists(&parent).await
                {
                    let _ = gio::spawn_blocking(move || {
                        parent.make_directory_with_parents(gio::Cancellable::NONE)
                    })
                    .await;
                }
                let Some(parent) = original.parent() else {
                    continue;
                };
                if transfer_one(job, mgr, &item, &parent, &name(&original), true, true)
                    .await?
                    .is_some()
                {
                    job.imp()
                        .outcome
                        .borrow_mut()
                        .moved
                        .push((item.clone(), original.clone()));
                }
            }
        }
    }
    job.report(true);
    Ok(())
}

async fn confirm_permanent_delete(mgr: &JobManager, file: &gio::File) -> bool {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Cannot Move “%s” to Trash").replace("%s", &name(file)))
        .body(gettext(
            "This location does not support trashing. Delete it permanently instead?",
        ))
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("delete", &gettext("_Delete Permanently")),
    ]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.choose_future(Some(&mgr.parent_window())).await == "delete"
}

/// First counting pass: total files and bytes, so progress is meaningful. Errors are ignored.
async fn count(job: &Job, files: Vec<gio::File>) {
    let mut n = 0u64;
    let mut bytes = 0u64;
    for f in files {
        count_one(job, &f, &mut n, &mut bytes).await;
    }
    job.set_files_total(n);
    job.set_bytes_total(bytes);
    job.start_clock();
    job.report(true);
}

async fn count_one(job: &Job, file: &gio::File, n: &mut u64, bytes: &mut u64) {
    let Ok(info) = file
        .query_info_future("standard::type,standard::size", NOFOLLOW, PRIO)
        .await
    else {
        return;
    };
    if info.file_type() == gio::FileType::Directory && !in_trash(file) {
        *n += 1;
        for (child, cinfo) in children(file, "standard::type,standard::size,standard::name").await {
            if cinfo.file_type() == gio::FileType::Directory {
                Box::pin(count_one(job, &child, n, bytes)).await;
            } else {
                *n += 1;
                *bytes += cinfo.size() as u64;
            }
            job.report_counting(*n);
        }
    } else {
        *n += 1;
        *bytes += info.size() as u64;
    }
}

/// Items inside trash:/// cannot be modified individually; gvfs deletes a trashed
/// directory as one unit, so never descend into one.
fn in_trash(file: &gio::File) -> bool {
    file.uri_scheme().as_deref() == Some("trash")
}

/// All direct children of `dir` with their infos. Enumeration errors yield an empty list.
pub(crate) async fn children(dir: &gio::File, attrs: &str) -> Vec<(gio::File, gio::FileInfo)> {
    let mut out = Vec::new();
    let Ok(en) = dir.enumerate_children_future(attrs, NOFOLLOW, PRIO).await else {
        return out;
    };
    loop {
        match en.next_files_future(64, PRIO).await {
            Ok(infos) if infos.is_empty() => break,
            Ok(infos) => out.extend(infos.into_iter().map(|i| (en.child(&i), i))),
            Err(_) => break,
        }
    }
    out
}

/// Copy or move `src` into `dest_dir` as `dest_name`. Returns the final destination,
/// or `None` if the item was skipped.
async fn transfer_one(
    job: &Job,
    mgr: &JobManager,
    src: &gio::File,
    dest_dir: &gio::File,
    dest_name: &str,
    is_move: bool,
    top_level: bool,
) -> Res<Option<gio::File>> {
    let mut dest = dest_dir.child(dest_name);
    let mut overwrite = false;
    let base_flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
    let verb = if is_move {
        // Translators: fills %v in “Error While %v “%s””.
        gettext("Moving")
    } else {
        // Translators: fills %v in “Error While %v “%s””.
        gettext("Copying")
    };
    loop {
        let flags = if overwrite {
            base_flags | gio::FileCopyFlags::OVERWRITE
        } else {
            base_flags
        };

        // Fast path: a native move (rename) handles whole trees at once.
        if is_move {
            let (fut, _progress) = src.move_future(&dest, flags, PRIO);
            match fut.await {
                Ok(()) => {
                    job.set_files_done(job.files_done() + 1);
                    job.report(false);
                    crate::tags::relocate(src, &dest);
                    return Ok(Some(dest));
                }
                Err(e)
                    if e.matches(gio::IOErrorEnum::WouldRecurse)
                        || e.matches(gio::IOErrorEnum::WouldMerge) => {}
                Err(e) if e.matches(gio::IOErrorEnum::Exists) => {
                    match resolve_conflict(job, mgr, src, &dest, false).await? {
                        Step::Skip => return Ok(None),
                        Step::Overwrite => overwrite = true,
                        Step::Rename(n) => dest = dest_dir.child(&n),
                    }
                    continue;
                }
                Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => return Err(Fail::Cancelled),
                Err(e) => match ask_error(&mgr.parent_window(), &verb, src, &e).await {
                    ErrorChoice::Skip => return Ok(None),
                    ErrorChoice::Retry => continue,
                    ErrorChoice::Cancel => return Err(Fail::Cancelled),
                },
            }
        }

        let info = match src
            .query_info_future("standard::type,standard::size", NOFOLLOW, PRIO)
            .await
        {
            Ok(i) => i,
            Err(e) => match ask_error(&mgr.parent_window(), &verb, src, &e).await {
                ErrorChoice::Skip => return Ok(None),
                ErrorChoice::Retry => continue,
                ErrorChoice::Cancel => return Err(Fail::Cancelled),
            },
        };

        if info.file_type() == gio::FileType::Directory {
            match dest.make_directory_future(PRIO).await {
                Ok(()) => {}
                Err(e) if e.matches(gio::IOErrorEnum::Exists) => {
                    let dest_is_dir = file_type(&dest).await == gio::FileType::Directory;
                    if !dest_is_dir || !overwrite {
                        match resolve_conflict(job, mgr, src, &dest, dest_is_dir).await? {
                            Step::Skip => return Ok(None),
                            Step::Overwrite => {
                                if !dest_is_dir {
                                    // Replacing a file with a folder: remove the file first.
                                    let _ = dest.delete_future(PRIO).await;
                                }
                                overwrite = true;
                                continue;
                            }
                            Step::Rename(n) => {
                                dest = dest_dir.child(&n);
                                continue;
                            }
                        }
                    }
                }
                Err(e) => match ask_error(&mgr.parent_window(), &verb, src, &e).await {
                    ErrorChoice::Skip => return Ok(None),
                    ErrorChoice::Retry => continue,
                    ErrorChoice::Cancel => return Err(Fail::Cancelled),
                },
            }
            job.set_files_done(job.files_done() + 1);
            let mut all_moved = true;
            for (child, cinfo) in children(src, "standard::name").await {
                let r = Box::pin(transfer_one(
                    job,
                    mgr,
                    &child,
                    &dest,
                    &cinfo.name().to_string_lossy(),
                    is_move,
                    false,
                ))
                .await?;
                all_moved &= r.is_some();
            }
            if is_move && all_moved {
                let _ = src.delete_future(PRIO).await;
                crate::tags::relocate(src, &dest);
            }
            return Ok(Some(dest));
        }

        let size = info.size() as u64;
        let base = job.bytes_done();
        if !overwrite {
            job.imp().in_flight.replace(Some(dest.clone()));
        }
        let (fut, progress) = src.copy_future(&dest, flags, PRIO);
        let progress = progress.for_each(|(cur, _total)| {
            job.set_bytes_done(base + cur.max(0) as u64);
            job.report(false);
            async {}
        });
        let (result, ()) = futures_util::join!(fut, progress);
        job.imp().in_flight.replace(None);
        match result {
            Ok(()) => {
                job.set_bytes_done(base + size);
                job.set_files_done(job.files_done() + 1);
                job.report(false);
                if is_move {
                    let _ = src.delete_future(PRIO).await;
                    crate::tags::relocate(src, &dest);
                }
                let _ = top_level;
                return Ok(Some(dest));
            }
            Err(e) if e.matches(gio::IOErrorEnum::Exists) => {
                match resolve_conflict(job, mgr, src, &dest, false).await? {
                    Step::Skip => {
                        job.set_bytes_done(base + size);
                        job.set_files_done(job.files_done() + 1);
                        return Ok(None);
                    }
                    Step::Overwrite => overwrite = true,
                    Step::Rename(n) => dest = dest_dir.child(&n),
                }
            }
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => return Err(Fail::Cancelled),
            Err(e) => {
                job.set_bytes_done(base);
                match ask_error(&mgr.parent_window(), &verb, src, &e).await {
                    ErrorChoice::Skip => return Ok(None),
                    ErrorChoice::Retry => continue,
                    ErrorChoice::Cancel => return Err(Fail::Cancelled),
                }
            }
        }
    }
}

enum Step {
    Skip,
    Overwrite,
    Rename(String),
}

async fn resolve_conflict(
    job: &Job,
    mgr: &JobManager,
    src: &gio::File,
    dest: &gio::File,
    is_dir: bool,
) -> Res<Step> {
    let remembered = job.imp().apply_all.borrow().clone();
    let resolution = match remembered {
        Some(r) => r,
        None => {
            job.set_status(super::JobStatus::WaitingUser);
            job.set_detail(gettext("Waiting for your answer"));
            let (r, all) = ask_conflict(&mgr.parent_window(), src, dest, is_dir).await;
            job.set_status(super::JobStatus::Running);
            job.report(true);
            if all && matches!(r, Resolution::Skip | Resolution::Replace) {
                job.imp().apply_all.replace(Some(r.clone()));
            }
            r
        }
    };
    Ok(match resolution {
        Resolution::Skip => Step::Skip,
        Resolution::Replace => Step::Overwrite,
        Resolution::Rename(n) => Step::Rename(n),
        Resolution::Cancel => return Err(Fail::Cancelled),
    })
}

/// Post-order recursive delete.
async fn delete_recursive(job: &Job, mgr: &JobManager, file: &gio::File) -> Res<()> {
    let ftype = file_type(file).await;
    if ftype == gio::FileType::Directory && !in_trash(file) {
        for (child, _) in children(file, "standard::name").await {
            Box::pin(delete_recursive(job, mgr, &child)).await?;
        }
    }
    loop {
        match file.delete_future(PRIO).await {
            Ok(()) => break,
            Err(e) if e.matches(gio::IOErrorEnum::NotFound) => break,
            // Translators: fills %v in “Error While %v “%s””.
            Err(e) => match ask_error(&mgr.parent_window(), &gettext("Deleting"), file, &e).await {
                ErrorChoice::Skip => break,
                ErrorChoice::Retry => continue,
                ErrorChoice::Cancel => return Err(Fail::Cancelled),
            },
        }
    }
    job.set_files_done(job.files_done() + 1);
    job.report(false);
    Ok(())
}

/// Type without following symlinks; Unknown when the file is missing or unreadable.
async fn file_type(file: &gio::File) -> gio::FileType {
    file.query_info_future("standard::type", NOFOLLOW, PRIO)
        .await
        .map(|i| i.file_type())
        .unwrap_or(gio::FileType::Unknown)
}

async fn exists(file: &gio::File) -> bool {
    file.query_info_future("standard::type", NOFOLLOW, PRIO)
        .await
        .is_ok()
}

/// Name for a link to `file` in `dir`: the file's own name elsewhere, "Link to x" beside
/// it, then numbered until one is free.
async fn unique_link_name(dir: &gio::File, file: &gio::File) -> String {
    let base = name(file);
    // Translators: name of a symbolic link, as in “Link to report.pdf”.
    let link_to = gettext("Link to %s").replace("%s", &base);
    let beside = file.parent().is_some_and(|p| p.equal(dir));
    if !beside && !exists(&dir.child(&base)).await {
        return base;
    }
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            link_to.clone()
        } else {
            format!("{link_to} ({n})")
        };
        if !exists(&dir.child(&candidate)).await {
            return candidate;
        }
        n += 1;
    }
}

/// "Pasted Image.png", then numbered, first one free in `dir`.
async fn unique_image_name(dir: &gio::File) -> String {
    // Translators: file name given to an image pasted from the clipboard.
    let base = gettext("Pasted Image");
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            format!("{base}.png")
        } else {
            format!("{base} ({n}).png")
        };
        if !exists(&dir.child(&candidate)).await {
            return candidate;
        }
        n += 1;
    }
}

/// "x.txt" -> "x (copy).txt", "x (copy 2).txt", ... first one not present in `dir`.
async fn unique_copy_name(dir: &gio::File, original: &str) -> String {
    let (stem, ext) = match original.rfind('.').filter(|&i| i > 0) {
        Some(i) => (&original[..i], &original[i..]),
        None => (original, ""),
    };
    // Translators: goes into duplicate names, as in “report (copy).pdf”.
    let copy = gettext("copy");
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            format!("{stem} ({copy}){ext}")
        } else {
            format!("{stem} ({copy} {n}){ext}")
        };
        if !exists(&dir.child(&candidate)).await {
            return candidate;
        }
        n += 1;
    }
}
