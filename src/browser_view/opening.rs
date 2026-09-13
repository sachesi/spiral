//! Opening what is activated: files in their applications, folders, links, and locations
//! that have to be mounted first.

use super::*;

/// Where mounting the location of a view stands: nothing asked, a server being reached, or
/// one that answered with the reason it kept in `Failed`.
#[derive(Default)]
pub enum Mounting {
    #[default]
    Idle,
    Underway,
    Failed(String),
}

/// The application a file would open in, which is what decides whether a terminal is needed.
pub(super) async fn default_app(file: &gio::File) -> Option<gio::AppInfo> {
    let info = file
        .query_info_future(
            "standard::content-type",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .ok()?;
    gio::AppInfo::default_for_type(&file_utils::content_type_of(&info)?, !file.is_native())
}

impl BrowserView {
    /// Open the item at `pos`: descend into folders, launch files.
    pub(crate) fn activate_position(&self, pos: u32) {
        let Some(info) = self.imp().model.info_at(pos) else {
            return;
        };
        let file = file_utils::file_of(&info);
        if file_utils::is_dir(&info) {
            self.go_to(&file);
        } else if let Some(target) = file_utils::target_of(&info) {
            self.open_target(target, file_utils::listed_as(&info));
        } else if self.chooser_mode() {
            self.emit_by_name::<()>("file-activated", &[&file]);
        } else {
            self.launch(&file);
        }
    }

    /// Follow an entry that stands for somewhere else, as everything in `network:///` and
    /// `computer:///` does: the entry is not a folder and listing it says as much, so what
    /// it points at is opened instead. A share that is not mounted answers nothing about
    /// itself yet, and going there is what mounts it. `name` is what the entry was listed
    /// as, for a target that has no name of its own.
    pub(crate) fn open_target(&self, target: gio::File, name: Option<String>) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let folder = match target
                    .query_info_future(
                        "standard::type",
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT,
                    )
                    .await
                {
                    Ok(info) => info.file_type() == gio::FileType::Directory,
                    // Not mounted, or not there any more: go anyway and let the folder say so.
                    Err(_) => true,
                };
                if folder {
                    if let Some(name) = name {
                        view.imp().given_name.replace(Some((target.clone(), name)));
                    }
                    view.go_to(&target);
                } else if view.chooser_mode() {
                    view.emit_by_name::<()>("file-activated", &[&target]);
                } else {
                    view.launch(&target);
                }
            }
        ));
    }

    pub fn launch(&self, file: &gio::File) {
        let ctx = self.display().app_launch_context();
        let file = file.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                // An application that asks for a terminal has to be given one; GIO will not
                // go looking for a terminal emulator and refuses the launch instead.
                if let Some(app) = default_app(&file).await
                    && let Some(result) =
                        crate::terminal::launch_if_wanted(&app, std::slice::from_ref(&file))
                {
                    if let Err(e) = result {
                        view.show_error(&gettext("Could Not Open"), e.message());
                    }
                    return;
                }
                let uri = file.uri();
                if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await
                {
                    view.show_error(&gettext("Could Not Open"), e.message());
                }
            }
        ));
    }

    /// A folder on a share that is not mounted yet: mount it, then list it again. This is
    /// how a bookmark or an address typed into the bar reaches a server, without the
    /// connect dialog having to be opened first.
    pub(super) fn mount_location(&self) {
        let imp = self.imp();
        if !crate::prefs::use_network() {
            return;
        }
        let Some(error) = imp.model.error() else {
            return;
        };
        if !error.matches(gio::IOErrorEnum::NotMounted) {
            return;
        }
        let Some(file) = self.location() else {
            return;
        };
        if !matches!(*imp.mounting.borrow(), Mounting::Idle) {
            return;
        }
        imp.mounting.replace(Mounting::Underway);
        self.update_stack();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let result = crate::network::mount(&file, &view, &gio::Cancellable::new()).await;
                // The answer is about the location asked for; the view may have moved on.
                if view.location().is_none_or(|now| !now.equal(&file)) {
                    return;
                }
                match result {
                    Ok(()) => {
                        // Mounted: the address is worth trying again if it is ever lost,
                        // and the location has a mount to be named after now.
                        view.imp().mounting.replace(Mounting::Idle);
                        view.notify_location();
                        view.reload();
                    }
                    Err(e) => {
                        let why = if e.matches(gio::IOErrorEnum::FailedHandled) {
                            gettext("The password dialog was dismissed.")
                        } else {
                            e.message().to_string()
                        };
                        view.imp().mounting.replace(Mounting::Failed(why));
                        view.update_stack();
                    }
                }
            }
        ));
    }

    pub(crate) fn show_error(&self, heading: &str, message: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(message)
            .build();
        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self));
    }

    /// The machine the location is on, for a page that names it.
    pub(super) fn host(&self) -> String {
        let uri = self.location().map(|f| f.uri()).unwrap_or_default();
        glib::Uri::parse(&uri, glib::UriFlags::NONE)
            .ok()
            .and_then(|parsed| parsed.host())
            .map_or_else(|| uri.to_string(), |host| host.to_string())
    }

    /// What to say under "No Servers Found": where servers would come from, and the ones
    /// found and left out because nothing installed here can open them.
    pub(super) fn why_no_servers(&self) -> String {
        let mut text = gettext(
            "Machines that announce themselves on the network appear here. Others are reached with “Connect to Server…”.",
        );
        let left_out = self.imp().model.unreachable_schemes();
        if !left_out.is_empty() {
            let mut schemes: Vec<String> = left_out.iter().map(|s| format!("{s}://")).collect();
            schemes.dedup();
            text.push_str("\n\n");
            text.push_str(
                &ngettext(
                    "One server was found but left out: no gvfs backend for %s addresses is installed.",
                    "Some servers were found but left out: no gvfs backend for %s addresses is installed.",
                    left_out.len() as u32,
                )
                .replace("%s", &schemes.join(", ")),
            );
        }
        text
    }

    /// What to say under "Could Not Open Folder". The listing's own message as a rule, but
    /// a location whose scheme has no backend answers that it is not supported, which
    /// tells the reader nothing about what is missing.
    pub(super) fn why_not_opened(&self, message: &str) -> String {
        let Some(scheme) = self.location().and_then(|f| f.uri_scheme()) else {
            return message.to_string();
        };
        if crate::network::supports(&scheme) {
            return message.to_string();
        }
        gettext("No gvfs backend for %s addresses is installed.")
            .replace("%s", &format!("{scheme}://"))
    }
}
