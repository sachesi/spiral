use ashpd::backend::Result;
use ashpd::backend::file_chooser::FileChooserImpl;
use ashpd::backend::request::RequestImpl;
use ashpd::desktop::HandleToken;
use ashpd::desktop::file_chooser::{
    OpenFileOptions, SaveFileOptions, SaveFilesOptions, SelectedFiles,
};
use ashpd::{MaybeAppID, PortalError, WindowIdentifierType};
use async_trait::async_trait;
use futures_channel::oneshot;

use super::{Kind, Request};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.spiral";

pub struct SpiralChooser {
    tx: async_channel::Sender<Request>,
}

impl SpiralChooser {
    pub fn new(tx: async_channel::Sender<Request>) -> Self {
        Self { tx }
    }

    async fn dispatch(
        &self,
        kind: Kind,
        parent: Option<WindowIdentifierType>,
        title: &str,
    ) -> Result<SelectedFiles> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = Request {
            kind,
            title: title.to_string(),
            parent,
            reply: reply_tx,
        };
        if self.tx.send(req).await.is_err() {
            return Err(PortalError::Failed(
                "chooser main loop is not running".into(),
            ));
        }
        reply_rx.await.unwrap_or_else(|_| {
            Err(PortalError::Cancelled(
                "dialog closed without a reply".into(),
            ))
        })
    }
}

#[async_trait]
impl RequestImpl for SpiralChooser {
    // ashpd aborts the request itself, which drops its reply channel, and the dialog
    // watches for that. The token is no key to a request: it is only the last part of the
    // request's path, and every application that gives none gets "t".
    async fn close(&self, _token: HandleToken) {}
}

#[async_trait]
impl FileChooserImpl for SpiralChooser {
    async fn open_file(
        &self,
        _token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: OpenFileOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::Open(options), parent, title).await
    }

    async fn save_file(
        &self,
        _token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: SaveFileOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::Save(options), parent, title).await
    }

    async fn save_files(
        &self,
        _token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: SaveFilesOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::SaveFiles(options), parent, title).await
    }
}
