//! Owns running jobs, keeps the app alive while they run, and holds the single undo/redo slot.

use std::cell::{Cell, RefCell};

use futures_util::future::{Aborted, abortable};
use gettextrs::gettext;
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
        mgr
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
                mgr.finish(&job, result, record_undo).await;
            }
        ));
        job
    }

    async fn finish(
        &self,
        job: &Job,
        result: Result<Result<(), Fail>, Aborted>,
        record_undo: bool,
    ) {
        let imp = self.imp();
        let status = match result {
            Ok(Ok(())) => JobStatus::Done,
            Ok(Err(Fail::Cancelled)) => JobStatus::Cancelled,
            Ok(Err(Fail::Failed(msg))) => {
                self.show_toast(&msg, false);
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
            JobStatus::Done => job.set_description(job.kind().done_message()),
            JobStatus::Cancelled => job.set_detail(gettext("Cancelled")),
            JobStatus::Failed => job.set_detail(gettext("Failed")),
            _ => {}
        }
        job.set_status(status);
        job.imp().hold.take();
        imp.running.set(imp.running.get().saturating_sub(1));
        self.notify_running();

        if status == JobStatus::Done {
            let kind = job.kind();
            if record_undo {
                let undo = undo_for(job);
                self.set_undo(undo, None);
            }
            // The operations list already shows what finished; like Nautilus, only trashing
            // gets a toast, so Undo is one click away.
            if matches!(kind, JobKind::Trash { .. }) {
                let undoable = record_undo && imp.undo.borrow().is_some();
                self.show_toast(&kind.done_message(), undoable);
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
            let redo = redo_for(&kind);
            mgr.set_undo(None, redo);
            mgr.submit_inner(kind, false);
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

/// The job that reverses `job`, derived from what it actually did.
fn undo_for(job: &Job) -> Option<JobKind> {
    let out = job.imp().outcome.borrow();
    match job.kind() {
        JobKind::Transfer { is_move: false, .. }
        | JobKind::CreateFolder { .. }
        | JobKind::CreateFile { .. }
        | JobKind::Extract { .. }
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
        JobKind::Rename { .. } => out.renamed.as_ref().map(|(file, old)| JobKind::Rename {
            file: file.clone(),
            new_name: old.clone(),
        }),
        JobKind::Restore { .. } => {
            let files: Vec<_> = out.moved.iter().map(|(_, orig)| orig.clone()).collect();
            (!files.is_empty()).then_some(JobKind::Trash { files })
        }
        JobKind::Delete { .. } => None,
    }
}

/// What redo should run after undoing with `undo_kind`.
fn redo_for(undo_kind: &JobKind) -> Option<JobKind> {
    match undo_kind {
        JobKind::Transfer {
            pairs,
            is_move: true,
        } => Some(JobKind::Transfer {
            pairs: pairs
                .iter()
                .filter_map(|(f, _)| f.parent().map(|p| (f.clone(), p)))
                .collect(),
            is_move: true,
        }),
        JobKind::Rename { file, new_name } => file.parent().map(|p| JobKind::Rename {
            file: p.child(new_name),
            new_name: super::job::name(file),
        }),
        JobKind::Restore { pairs } => Some(JobKind::Trash {
            files: pairs.iter().map(|(_, o)| o.clone()).collect(),
        }),
        // Redoing a restore would need another trash lookup; not offered.
        JobKind::Trash { .. } => None,
        // Re-copying after deleting the copy, or re-creating, is not offered.
        JobKind::Delete { .. }
        | JobKind::Transfer { is_move: false, .. }
        | JobKind::CreateFolder { .. }
        | JobKind::CreateFile { .. }
        | JobKind::Extract { .. }
        | JobKind::Compress { .. } => None,
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
