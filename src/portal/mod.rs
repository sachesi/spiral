//! xdg-desktop-portal FileChooser backend. The D-Bus side (`backend`) runs on a tokio thread and
//! forwards each request to the GTK main loop, where `chooser_window` shows the dialog.

pub mod backend;
pub mod chooser_window;

use ashpd::WindowIdentifierType;
use ashpd::backend::Result;
use ashpd::desktop::HandleToken;
use ashpd::desktop::file_chooser::{
    OpenFileOptions, SaveFileOptions, SaveFilesOptions, SelectedFiles,
};
use futures_channel::oneshot;

pub enum Kind {
    Open(OpenFileOptions),
    Save(SaveFileOptions),
    SaveFiles(SaveFilesOptions),
}

/// One portal request, everything in it is `Send`.
pub struct Request {
    pub kind: Kind,
    pub title: String,
    pub parent: Option<WindowIdentifierType>,
    pub token: HandleToken,
    pub reply: oneshot::Sender<Result<SelectedFiles>>,
    /// Fired when the portal asks us to close this request.
    pub closed: oneshot::Receiver<()>,
}
