use std::cell::{Cell, RefCell};

use futures_util::future::AbortHandle;
use gettextrs::{gettext, ngettext};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::{gio, glib, gtk};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "SpiralJobStatus")]
pub enum JobStatus {
    #[default]
    Pending,
    Running,
    WaitingUser,
    Done,
    Cancelled,
    Failed,
}

/// What a job does. `Transfer` pairs are (source, destination directory).
#[derive(Debug, Clone)]
pub enum JobKind {
    Transfer {
        pairs: Vec<(gio::File, gio::File)>,
        is_move: bool,
    },
    Trash {
        files: Vec<gio::File>,
    },
    Delete {
        files: Vec<gio::File>,
    },
    Rename {
        file: gio::File,
        new_name: String,
    },
    CreateFolder {
        parent: gio::File,
        name: String,
    },
    CreateFile {
        parent: gio::File,
        name: String,
    },
    /// Move trashed items back: (item inside trash:///, original location).
    Restore {
        pairs: Vec<(gio::File, gio::File)>,
    },
    Extract {
        archives: Vec<gio::File>,
        dest: gio::File,
    },
    /// Symbolic links to `files`, created in `dest`.
    Link {
        files: Vec<gio::File>,
        dest: gio::File,
    },
    /// Pack `files` into `dest/file_name`, encrypted when `password` is set.
    Compress {
        files: Vec<gio::File>,
        dest: gio::File,
        file_name: String,
        password: Option<String>,
    },
}

impl JobKind {
    pub fn description(&self) -> String {
        match self {
            JobKind::Transfer {
                pairs,
                is_move: false,
            } => match pairs.len() {
                1 => gettext("Copying “%s” to “%t”").replace("%s", &name(&pairs[0].0)),
                k => ngettext(
                    "Copying %d file to “%t”",
                    "Copying %d files to “%t”",
                    k as u32,
                )
                .replace("%d", &k.to_string()),
            }
            .replace("%t", &name(&pairs[0].1)),
            JobKind::Transfer {
                pairs,
                is_move: true,
            } => match pairs.len() {
                1 => gettext("Moving “%s” to “%t”").replace("%s", &name(&pairs[0].0)),
                k => ngettext(
                    "Moving %d file to “%t”",
                    "Moving %d files to “%t”",
                    k as u32,
                )
                .replace("%d", &k.to_string()),
            }
            .replace("%t", &name(&pairs[0].1)),
            JobKind::Trash { files } => match files.len() {
                1 => gettext("Moving “%s” to trash").replace("%s", &name(&files[0])),
                k => ngettext(
                    "Moving %d file to trash",
                    "Moving %d files to trash",
                    k as u32,
                )
                .replace("%d", &k.to_string()),
            },
            JobKind::Delete { files } => match files.len() {
                1 => gettext("Deleting “%s”").replace("%s", &name(&files[0])),
                k => ngettext("Deleting %d file", "Deleting %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Rename { file, new_name } => gettext("Renaming “%s” to “%t”")
                .replace("%s", &name(file))
                .replace("%t", new_name),
            JobKind::CreateFolder { name, .. } => {
                gettext("Creating folder “%s”").replace("%s", name)
            }
            JobKind::CreateFile { name, .. } => gettext("Creating “%s”").replace("%s", name),
            JobKind::Restore { pairs } => match pairs.len() {
                1 => gettext("Restoring “%s”").replace("%s", &name(&pairs[0].1)),
                k => ngettext("Restoring %d file", "Restoring %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Extract { archives, .. } => match archives.len() {
                1 => gettext("Extracting “%s”").replace("%s", &name(&archives[0])),
                k => ngettext("Extracting %d archive", "Extracting %d archives", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Compress { file_name, .. } => {
                gettext("Compressing to “%s”").replace("%s", file_name)
            }
            JobKind::Link { files, .. } => match files.len() {
                1 => gettext("Creating link to “%s”").replace("%s", &name(&files[0])),
                k => ngettext("Creating %d link", "Creating %d links", k as u32)
                    .replace("%d", &k.to_string()),
            },
        }
    }

    /// Text for the finished row in the operations list, and the toast after trashing.
    pub fn done_message(&self) -> String {
        match self {
            JobKind::Transfer {
                pairs,
                is_move: false,
            } => match pairs.len() {
                1 => gettext("Copied “%s”").replace("%s", &name(&pairs[0].0)),
                k => ngettext("Copied %d file", "Copied %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Transfer {
                pairs,
                is_move: true,
            } => match pairs.len() {
                1 => gettext("Moved “%s”").replace("%s", &name(&pairs[0].0)),
                k => ngettext("Moved %d file", "Moved %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Trash { files } => match files.len() {
                1 => gettext("Moved “%s” to trash").replace("%s", &name(&files[0])),
                k => ngettext(
                    "Moved %d file to trash",
                    "Moved %d files to trash",
                    k as u32,
                )
                .replace("%d", &k.to_string()),
            },
            JobKind::Delete { files } => match files.len() {
                1 => gettext("Deleted “%s”").replace("%s", &name(&files[0])),
                k => ngettext("Deleted %d file", "Deleted %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Rename { new_name, .. } => gettext("Renamed to “%s”").replace("%s", new_name),
            JobKind::CreateFolder { name, .. } => {
                gettext("Created folder “%s”").replace("%s", name)
            }
            JobKind::CreateFile { name, .. } => gettext("Created “%s”").replace("%s", name),
            JobKind::Restore { pairs } => match pairs.len() {
                1 => gettext("Restored “%s”").replace("%s", &name(&pairs[0].1)),
                k => ngettext("Restored %d file", "Restored %d files", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Extract { archives, .. } => match archives.len() {
                1 => gettext("Extracted “%s”").replace("%s", &name(&archives[0])),
                k => ngettext("Extracted %d archive", "Extracted %d archives", k as u32)
                    .replace("%d", &k.to_string()),
            },
            JobKind::Compress { file_name, .. } => gettext("Created “%s”").replace("%s", file_name),
            JobKind::Link { files, .. } => match files.len() {
                1 => gettext("Created link to “%s”").replace("%s", &name(&files[0])),
                k => ngettext("Created %d link", "Created %d links", k as u32)
                    .replace("%d", &k.to_string()),
            },
        }
    }
}

fn time_left(secs: u64) -> String {
    match secs {
        0..60 => ngettext("%d second", "%d seconds", secs as u32).replace("%d", &secs.to_string()),
        60..3600 => {
            let m = secs.div_ceil(60);
            ngettext("%d minute", "%d minutes", m as u32).replace("%d", &m.to_string())
        }
        _ => {
            let h = secs.div_ceil(3600);
            ngettext("%d hour", "%d hours", h as u32).replace("%d", &h.to_string())
        }
    }
}

pub fn name(file: &gio::File) -> String {
    file.basename()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.uri().to_string())
}

/// How to resolve a name collision. `Skip`/`Replace` may be remembered for the rest of the job.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Skip,
    Replace,
    Rename(String),
    Cancel,
}

/// What a job achieved, for undo.
#[derive(Debug, Default)]
pub struct Outcome {
    pub created: Vec<gio::File>,
    /// (original source, final destination) of successfully moved top-level items.
    pub moved: Vec<(gio::File, gio::File)>,
    pub trashed: Vec<gio::File>,
    /// (renamed file, previous name).
    pub renamed: Option<(gio::File, String)>,
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Job)]
    pub struct Job {
        #[property(get, set, builder(JobStatus::Pending))]
        pub status: Cell<JobStatus>,
        #[property(get, set)]
        pub description: RefCell<String>,
        #[property(get, set)]
        pub detail: RefCell<String>,
        #[property(get, set, minimum = 0.0, maximum = 1.0)]
        pub fraction: Cell<f64>,
        #[property(get, set)]
        pub files_done: Cell<u64>,
        #[property(get, set)]
        pub files_total: Cell<u64>,
        #[property(get, set)]
        pub bytes_done: Cell<u64>,
        #[property(get, set)]
        pub bytes_total: Cell<u64>,

        pub kind: RefCell<Option<JobKind>>,
        pub abort: RefCell<Option<AbortHandle>>,
        pub apply_all: RefCell<Option<Resolution>>,
        pub outcome: RefCell<Outcome>,
        /// Destination being written right now; removed if the job is cancelled mid-file.
        pub in_flight: RefCell<Option<gio::File>>,
        pub hold: RefCell<Option<gio::ApplicationHoldGuard>>,
        pub last_notify: Cell<i64>,
        /// Monotonic time when the transfer itself began, for speed and time left.
        pub started: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Job {
        const NAME: &'static str = "SpiralJob";
        type Type = super::Job;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Job {}
}

glib::wrapper! {
    pub struct Job(ObjectSubclass<imp::Job>);
}

impl Job {
    pub fn new(kind: JobKind) -> Self {
        let job: Self = glib::Object::new();
        job.set_description(kind.description());
        job.imp().kind.replace(Some(kind));
        job
    }

    pub fn kind(&self) -> JobKind {
        self.imp().kind.borrow().clone().expect("job without kind")
    }

    pub fn cancel(&self) {
        if let Some(h) = self.imp().abort.borrow().as_ref() {
            h.abort();
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.status(),
            JobStatus::Done | JobStatus::Cancelled | JobStatus::Failed
        )
    }

    fn throttled(&self, force: bool) -> Option<i64> {
        let imp = self.imp();
        let now = glib::monotonic_time();
        if !force && now - imp.last_notify.get() < 66_000 {
            return None;
        }
        imp.last_notify.set(now);
        Some(now)
    }

    /// Progress of the counting pass that precedes a transfer or delete.
    pub(super) fn report_counting(&self, files: u64) {
        if self.throttled(false).is_none() {
            return;
        }
        self.set_detail(
            ngettext("Preparing… %d file", "Preparing… %d files", files as u32)
                .replace("%d", &files.to_string()),
        );
    }

    /// Counting is over; the clock for speed and time left starts now.
    pub(super) fn start_clock(&self) {
        self.imp().started.set(glib::monotonic_time());
    }

    /// Throttled progress update (≤ ~15 Hz) so the UI is not flooded.
    pub(super) fn report(&self, force: bool) {
        let imp = self.imp();
        let Some(now) = self.throttled(force) else {
            return;
        };
        let fraction = if imp.bytes_total.get() > 0 {
            imp.bytes_done.get() as f64 / imp.bytes_total.get() as f64
        } else if imp.files_total.get() > 0 {
            imp.files_done.get() as f64 / imp.files_total.get() as f64
        } else {
            0.0
        };
        self.set_fraction(fraction.clamp(0.0, 1.0));
        let detail = if imp.bytes_total.get() > 0 {
            let done = imp.bytes_done.get();
            let total = imp.bytes_total.get();
            let elapsed = (now - imp.started.get()) as f64 / 1e6;
            let mut detail = gettext("%a of %b")
                .replace("%a", &crate::prefs::size(done))
                .replace("%b", &crate::prefs::size(total));
            // Rate needs a second of history before it means anything.
            if done > 0 && done < total && elapsed >= 1.0 {
                let rate = done as f64 / elapsed;
                let left = ((total - done) as f64 / rate).ceil() as u64;
                detail = gettext("%a, %t left (%r/s)")
                    .replace("%a", &detail)
                    .replace("%t", &time_left(left))
                    .replace("%r", &crate::prefs::size(rate as u64));
            }
            detail
        } else {
            ngettext(
                "%a of %b file",
                "%a of %b files",
                imp.files_total.get() as u32,
            )
            .replace("%a", &imp.files_done.get().to_string())
            .replace("%b", &imp.files_total.get().to_string())
        };
        self.set_detail(detail);
    }
}
