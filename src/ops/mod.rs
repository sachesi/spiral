//! File operations: jobs running on the main loop over async GIO, with progress, conflicts and undo.

pub mod archive;
mod conflict;
mod job;
mod manager;
mod walk;

pub use job::{Job, JobKind, JobStatus, name};
pub use manager::JobManager;
pub(crate) use walk::children;

use gettextrs::gettext;

use crate::adw::prelude::*;
use crate::{adw, gio, gtk};

/// Ask before emptying the trash, then hand back the job that empties it. The trash is
/// enumerated here, so hidden items and whatever a view filtered out go with the rest;
/// None if the question was refused, or there was nothing in there.
pub async fn empty_trash_job(parent: &impl IsA<gtk::Widget>) -> Option<JobKind> {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Empty Trash?"))
        .body(gettext(
            "All items in the Trash will be permanently deleted.",
        ))
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("empty", &gettext("_Empty Trash")),
    ]);
    dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
    if dialog.choose_future(Some(parent)).await != "empty" {
        return None;
    }
    let files: Vec<gio::File> = children(&gio::File::for_uri("trash:///"), "standard::name")
        .await
        .into_iter()
        .map(|(f, _)| f)
        .collect();
    (!files.is_empty()).then_some(JobKind::Delete { files })
}
