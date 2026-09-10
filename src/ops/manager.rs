//! Owns running jobs, keeps the app alive while they run, and holds the single undo/redo slot.

use std::cell::{Cell, RefCell};

use futures_util::future::{Aborted, abortable};
use gettextrs::{gettext, ngettext};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::application::SpiralApplication;
use crate::ops::job::{Job, JobKind, JobStatus};
use crate::ops::walk::{self, Fail};
use crate::window::SpiralWindow;
use crate::{gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::JobManager)]
    pub struct JobManager {
        #[property(get)]
        pub jobs: gio::ListStore,
        #[property(get)]
        pub running: Cell<u32>,
        #[property(get)]
        pub can_undo: Cell<bool>,
        #[property(get)]
        pub can_redo: Cell<bool>,
        pub app: glib::WeakRef<SpiralApplication>,
        pub undo: RefCell<Option<JobKind>>,
        pub redo: RefCell<Option<JobKind>>,
    }

    impl Default for JobManager {
        fn default() -> Self {
            Self {
                jobs: gio::ListStore::new::<Job>(),
                running: Default::default(),
                can_undo: Default::default(),
                can_redo: Default::default(),
                app: Default::default(),
                undo: Default::default(),
                redo: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for JobManager {
        const NAME: &'static str = "SpiralJobManager";
        type Type = super::JobManager;
    }

    #[glib::derived_properties]
    impl ObjectImpl for JobManager {}
}

glib::wrapper! {
    pub struct JobManager(ObjectSubclass<imp::JobManager>);
}

impl JobManager {
    pub fn new(app: &SpiralApplication) -> Self {
        let mgr: Self = glib::Object::new();
        mgr.imp().app.set(Some(app));
        // The operations list goes with the last window; a notification stands in for it
        // until a window is back.
        app.connect_window_removed(glib::clone!(
            #[weak]
            mgr,
            move |_, _| mgr.tell_running()
        ));
        app.connect_window_added(|app, _| app.withdraw_notification(RUNNING));
        mgr
    }

    fn has_window(&self) -> bool {
        self.app().windows().iter().any(|w| w.is::<SpiralWindow>())
    }

    /// With no window open, say how many operations are running, or withdraw the word once
    /// none are.
    fn tell_running(&self) {
        let app = self.app();
        let running = self.imp().running.get();
        if self.has_window() || running == 0 {
            app.withdraw_notification(RUNNING);
            return;
        }
        let n = gio::Notification::new(&gettext("File Operations"));
        n.set_body(Some(
            &ngettext(
                "%d file operation running",
                "%d file operations running",
                running,
            )
            .replace("%d", &running.to_string()),
        ));
        n.set_category(Some("transfer"));
        n.set_default_action("app.show-operations");
        n.add_button(&gettext("Show Details"), "app.show-operations");
        app.send_notification(Some(RUNNING), &n);
    }

    /// With no window open, say how an operation ended: what went wrong with one that
    /// failed, and that they are all done once the last one is.
    fn tell_end(&self, failure: Option<&str>) {
        if self.has_window() {
            return;
        }
        let app = self.app();
        if let Some(message) = failure {
            let n = gio::Notification::new(&gettext("File Operation Failed"));
            n.set_body(Some(message));
            n.set_category(Some("transfer.error"));
            n.set_default_action("app.new-window");
            app.send_notification(None, &n);
        }
        self.tell_running();
        if self.imp().running.get() == 0 && failure.is_none() {
            let n = gio::Notification::new(&gettext("File Operations"));
            n.set_body(Some(&gettext("All file operations are done")));
            n.set_category(Some("transfer.complete"));
            n.set_default_action("app.new-window");
            app.send_notification(Some(DONE), &n);
        }
    }

    fn app(&self) -> SpiralApplication {
        self.imp().app.upgrade().expect("application gone")
    }

    /// Window to parent dialogs on: the active one, or a fresh one if all were closed.
    pub fn parent_window(&self) -> gtk::Window {
        let app = self.app();
        if let Some(w) = app.active_window() {
            return w;
        }
        let win = SpiralWindow::new(&app);
        win.open_location(&gio::File::for_path(glib::home_dir()));
        win.present();
        win.upcast()
    }

    /// Start a job. Returns immediately; the job runs on the main loop.
    pub fn submit(&self, kind: JobKind) -> Job {
        self.submit_inner(kind, true)
    }

    fn submit_inner(&self, kind: JobKind, record_undo: bool) -> Job {
        let imp = self.imp();
        let job = Job::new(kind);
        job.imp().hold.replace(Some(self.app().hold()));
        imp.jobs.append(&job);
        imp.running.set(imp.running.get() + 1);
        self.notify_running();
        job.set_status(JobStatus::Running);

        let started = std::time::Instant::now();
        let (fut, handle) = abortable(glib::clone!(
            #[strong]
            job,
            #[strong(rename_to = mgr)]
            self,
            async move { walk::run(&job, &mgr).await }
        ));
        job.imp().abort.replace(Some(handle));

        glib::spawn_future_local(glib::clone!(
            #[strong]
            job,
            #[strong(rename_to = mgr)]
            self,
            async move {
                let result = fut.await;
                mgr.finish(&job, result, record_undo, started.elapsed())
                    .await;
            }
        ));
        job
    }

    async fn finish(
        &self,
        job: &Job,
        result: Result<Result<(), Fail>, Aborted>,
        record_undo: bool,
        took: std::time::Duration,
    ) {
        let imp = self.imp();
        let mut failure = None;
        let status = match result {
            Ok(Ok(())) => JobStatus::Done,
            Ok(Err(Fail::Cancelled)) => JobStatus::Cancelled,
            Ok(Err(Fail::Failed(msg))) => {
                self.show_toast(&msg, false);
                failure = Some(msg);
                JobStatus::Failed
            }
            Err(Aborted) => {
                if let Some(partial) = job.imp().in_flight.take() {
                    let _ = partial.delete_future(glib::Priority::DEFAULT).await;
                }
                JobStatus::Cancelled
            }
        };
        match status {
            JobStatus::Done => job.set_description(job.done_message()),
            JobStatus::Cancelled => job.set_detail(gettext("Cancelled")),
            JobStatus::Failed => job.set_detail(gettext("Failed")),
            _ => {}
        }
        job.set_status(status);
        job.imp().hold.take();
        imp.running.set(imp.running.get().saturating_sub(1));
        self.notify_running();
        self.tell_end(failure.as_deref());

        if status == JobStatus::Done {
            let kind = job.kind();
            if record_undo {
                let undo = undo_for(job);
                self.set_undo(undo, None);
            }
            // The operations list already shows what finished. Trashing gets a toast, so
            // Undo is one click away, and so does what lands in a folder other than the
            // one on screen, with the way there; or in that one, when it took long enough
            // to have been forgotten.
            if matches!(kind, JobKind::Trash { .. }) {
                let undoable = record_undo && imp.undo.borrow().is_some();
                self.show_toast(&job.done_message(), undoable);
            } else if let Some(folder) = kind.destination()
                && let Some(win) = self.app().active_window().and_downcast::<SpiralWindow>()
            {
                win.show_done_toast(&job.done_message(), &folder, job.landed(), took >= SLOW);
            }
        }

        // Keep the finished row visible briefly so the user sees it complete.
        glib::timeout_future_seconds(3).await;
        if let Some(pos) = imp.jobs.find(job) {
            imp.jobs.remove(pos);
        }
    }

    fn set_undo(&self, undo: Option<JobKind>, redo: Option<JobKind>) {
        let imp = self.imp();
        imp.undo.replace(undo);
        imp.redo.replace(redo);
        imp.can_undo.set(imp.undo.borrow().is_some());
        imp.can_redo.set(imp.redo.borrow().is_some());
        self.notify_can_undo();
        self.notify_can_redo();
    }

    /// Stop every running job; partial files are cleaned up as on a manual stop.
    pub fn cancel_all(&self) {
        for job in self.imp().jobs.iter::<Job>().flatten() {
            job.cancel();
        }
    }

    pub fn undo(&self) {
        let Some(kind) = self.imp().undo.borrow_mut().take() else {
            return;
        };
        // Undoing a trash needs the trash contents resolved first.
        let mgr = self.clone();
        glib::spawn_future_local(async move {
            let kind = match kind {
                JobKind::Trash { files } => match find_in_trash(&files).await {
                    pairs if pairs.is_empty() => {
                        mgr.set_undo(None, None);
                        return;
                    }
                    pairs => JobKind::Restore { pairs },
                },
                other => other,
            };
            mgr.set_undo(None, None);
            let job = mgr.submit_inner(kind, false);
            // Redo reverses the undo, read from what the undo did: where a move put things
            // back, and what a restore landed as. Not when something else has been done
            // since, and not for an undone restore, which would need the trash looked up
            // again.
            job.connect_status_notify(glib::clone!(
                #[weak]
                mgr,
                move |job| {
                    if job.status() == JobStatus::Done
                        && mgr.imp().undo.borrow().is_none()
                        && !matches!(job.kind(), JobKind::Trash { .. })
                    {
                        mgr.set_undo(None, undo_for(job));
                    }
                }
            ));
        });
    }

    pub fn redo(&self) {
        let Some(kind) = self.imp().redo.borrow_mut().take() else {
            return;
        };
        self.set_undo(None, None);
        self.submit_inner(kind, true);
    }

    fn show_toast(&self, message: &str, undoable: bool) {
        if let Some(win) = self.app().active_window().and_downcast::<SpiralWindow>() {
            win.show_toast(message, undoable);
        }
    }
}

/// Ids of the notifications about operations: the one counting those running, and the one
/// saying they are done.
const RUNNING: &str = "operations";
const DONE: &str = "operations-done";

/// How long an operation takes before its end is worth a toast in the folder it was for.
const SLOW: std::time::Duration = std::time::Duration::from_secs(3);

/// The job that reverses `job`, derived from what it actually did.
fn undo_for(job: &Job) -> Option<JobKind> {
    let out = job.imp().outcome.borrow();
    match job.kind() {
        JobKind::Transfer { is_move: false, .. }
        | JobKind::CreateFolder { .. }
        | JobKind::CreateFile { .. }
        | JobKind::SaveImage { .. }
        | JobKind::Extract { .. }
        | JobKind::Link { .. }
        | JobKind::Compress { .. } => (!out.created.is_empty()).then(|| JobKind::Delete {
            files: out.created.clone(),
        }),
        JobKind::Transfer { is_move: true, .. } => {
            let pairs: Vec<_> = out
                .moved
                .iter()
                .filter_map(|(src, dest)| src.parent().map(|p| (dest.clone(), p)))
                .collect();
            (!pairs.is_empty()).then_some(JobKind::Transfer {
                pairs,
                is_move: true,
            })
        }
        JobKind::Trash { .. } => (!out.trashed.is_empty()).then(|| JobKind::Trash {
            files: out.trashed.clone(),
        }),
        JobKind::Rename { .. } => (!out.renamed.is_empty()).then(|| JobKind::Rename {
            renames: out.renamed.clone(),
        }),
        JobKind::Restore { .. } => {
            let files: Vec<_> = out.moved.iter().map(|(_, orig)| orig.clone()).collect();
            (!files.is_empty()).then_some(JobKind::Trash { files })
        }
        JobKind::NewFolderWith { .. } => {
            let folder = out.created.first()?.clone();
            let pairs: Vec<_> = out
                .moved
                .iter()
                .filter_map(|(src, dest)| src.parent().map(|p| (dest.clone(), p)))
                .collect();
            Some(JobKind::Unfold { folder, pairs })
        }
        JobKind::Unfold { folder, .. } => {
            let files: Vec<_> = out.moved.iter().map(|(_, dest)| dest.clone()).collect();
            if files.is_empty() {
                return None;
            }
            Some(JobKind::NewFolderWith {
                parent: folder.parent()?,
                name: crate::ops::name(&folder),
                files,
            })
        }
        JobKind::Delete { .. } => None,
    }
}

/// Locate `originals` in trash:/// by original path, newest deletion first.
async fn find_in_trash(originals: &[gio::File]) -> Vec<(gio::File, gio::File)> {
    let trash = gio::File::for_uri("trash:///");
    let Ok(en) = trash
        .enumerate_children_future(
            "standard::name,trash::orig-path,trash::deletion-date",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        )
        .await
    else {
        return Vec::new();
    };
    let mut items: Vec<(String, String, gio::File)> = Vec::new();
    loop {
        match en.next_files_future(64, glib::Priority::DEFAULT).await {
            Ok(infos) if infos.is_empty() => break,
            Ok(infos) => {
                for info in infos {
                    if let Some(orig) = info.attribute_byte_string("trash::orig-path") {
                        let date = info
                            .attribute_string("trash::deletion-date")
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        items.push((orig.to_string(), date, en.child(&info)));
                    }
                }
            }
            Err(_) => break,
        }
    }
    let mut pairs = Vec::new();
    for orig in originals {
        let Some(path) = orig.path() else { continue };
        let path = path.to_string_lossy().into_owned();
        if let Some(best) = items
            .iter()
            .filter(|(p, _, _)| *p == path)
            .max_by(|a, b| a.1.cmp(&b.1))
        {
            pairs.push((best.2.clone(), orig.clone()));
        }
    }
    pairs
}
