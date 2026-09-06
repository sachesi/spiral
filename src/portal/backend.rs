use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    pending: Arc<Mutex<HashMap<HandleToken, oneshot::Sender<()>>>>,
}

impl SpiralChooser {
    pub fn new(tx: async_channel::Sender<Request>) -> Self {
        Self {
            tx,
            pending: Default::default(),
        }
    }

    async fn dispatch(
        &self,
        kind: Kind,
        token: HandleToken,
        parent: Option<WindowIdentifierType>,
        title: &str,
    ) -> Result<SelectedFiles> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let (close_tx, close_rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(token.clone(), close_tx);
        let req = Request {
            kind,
            title: title.to_string(),
            parent,
            token: token.clone(),
            reply: reply_tx,
            closed: close_rx,
        };
        if self.tx.send(req).await.is_err() {
            return Err(PortalError::Failed(
                "chooser main loop is not running".into(),
            ));
        }
        let result = reply_rx.await.unwrap_or_else(|_| {
            Err(PortalError::Cancelled(
                "dialog closed without a reply".into(),
            ))
        });
        self.pending.lock().unwrap().remove(&token);
        result
    }
}

#[async_trait]
impl RequestImpl for SpiralChooser {
    async fn close(&self, token: HandleToken) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&token) {
            let _ = tx.send(());
        }
    }
}

#[async_trait]
impl FileChooserImpl for SpiralChooser {
    async fn open_file(
        &self,
        token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: OpenFileOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::Open(options), token, parent, title)
            .await
    }

    async fn save_file(
        &self,
        token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: SaveFileOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::Save(options), token, parent, title)
            .await
    }

    async fn save_files(
        &self,
        token: HandleToken,
        _app_id: Option<MaybeAppID>,
        parent: Option<WindowIdentifierType>,
        title: &str,
        options: SaveFilesOptions,
    ) -> Result<SelectedFiles> {
        self.dispatch(Kind::SaveFiles(options), token, parent, title)
            .await
    }
}
