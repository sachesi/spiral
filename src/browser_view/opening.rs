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

/// What runs a file as a program: the application a desktop entry names, a script in a
/// terminal, or the file itself.
pub(crate) enum Program {
    Launcher(gio_unix::DesktopAppInfo),
    Script(std::path::PathBuf),
    Binary(std::path::PathBuf),
}

impl Program {
    /// Of a local file; none for a desktop entry that is a link or does not parse.
    pub(crate) fn of(info: &gio::FileInfo) -> Option<Self> {
        let path = file_utils::file_of(info).path()?;
        if file_utils::is_launcher(info) {
            return gio_unix::DesktopAppInfo::from_filename(&path).map(Self::Launcher);
        }
        let script = file_utils::content_type_of(info)
            .is_some_and(|ct| gio::content_type_is_a(&ct, "text/plain"));
        Some(if script {
            Self::Script(path)
        } else {
            Self::Binary(path)
        })
    }

    fn path(&self) -> std::path::PathBuf {
        match self {
            Self::Launcher(app) => app.filename().unwrap_or_default(),
            Self::Script(path) | Self::Binary(path) => path.clone(),
        }
    }
}

/// Executable for every class that can read it, as the switch in Properties sets it.
fn mark_executable(path: std::path::PathBuf) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path)?.permissions().mode();
    std::fs::set_permissions(
        &path,
        std::fs::Permissions::from_mode(mode | ((mode & 0o444) >> 2)),
    )
}

impl BrowserView {
    /// Open the item at `pos`: descend into folders, launch files.
    pub(crate) fn activate_position(&self, pos: u32) {
        let Some(info) = self.imp().model.info_at(pos) else {
            return;
        };
        let file = file_utils::file_of(&info);
        if file_utils::is_dir(&info) && !self.desktop_mode() {
            self.go_to(&file);
        } else if let Some(target) = file_utils::target_of(&info) {
            self.open_target(target, file_utils::listed_as(&info));
        } else if self.chooser_mode() {
            self.emit_by_name::<()>("file-activated", &[&file]);
        } else {
            self.open_file(&info);
        }
    }

    /// Start the program or launcher `info` stands for, or open the file in its application.
    pub(crate) fn open_file(&self, info: &gio::FileInfo) {
        if !file_utils::starts_on_click(info) {
            self.launch(&file_utils::file_of(info));
            return;
        }
        let Some(program) = Program::of(info) else {
            // A link, or an entry that does not parse: its text is what there is to open.
            self.launch(&file_utils::file_of(info));
            return;
        };
        if file_utils::allows(info, "access::can-execute") {
            self.run_program(&program);
            return;
        }
        // Nothing downloaded or sent comes marked executable, so the mark is what lets a
        // click start a file without asking.
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if !view.confirm_run(&program).await {
                    return;
                }
                if let Err(e) = mark_executable(program.path()) {
                    view.show_error(&gettext("Could Not Mark as Executable"), &e.to_string());
                    return;
                }
                view.run_program(&program);
            }
        ));
    }

    async fn confirm_run(&self, program: &Program) -> bool {
        let name = program
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut body = gettext(
            "It is not marked as executable, as files that were downloaded or sent are not. Only run programs from sources you trust.",
        );
        if let Program::Launcher(app) = program
            && let Some(line) = app.commandline()
        {
            body.push_str("\n\n");
            body.push_str(&gettext("It runs: %s").replace("%s", &line.to_string_lossy()));
        }
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Run “%s”?").replace("%s", &name))
            .body(body)
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("run", &gettext("_Mark as Executable and Run")),
        ]);
        dialog.set_response_appearance("run", adw::ResponseAppearance::Suggested);
        dialog.choose_future(Some(self)).await == "run"
    }

    /// Scripts run in the terminal so their output can be seen; binaries and the
    /// applications of desktop entries start directly.
    pub(crate) fn run_program(&self, program: &Program) {
        let context = self.display().app_launch_context();
        let result = match program {
            Program::Launcher(app) => crate::terminal::launch_if_wanted(app.upcast_ref(), &[])
                .unwrap_or_else(|| app.launch(&[], Some(&context))),
            Program::Script(path) => {
                let dir = path
                    .parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_default();
                crate::terminal::run(&dir, path)
            }
            Program::Binary(path) => gio::AppInfo::create_from_commandline(
                glib::shell_quote(path).to_string_lossy().as_ref(),
                None,
                gio::AppInfoCreateFlags::NONE,
            )
            .and_then(|app| app.launch(&[], Some(&context))),
        };
        if let Err(e) = result {
            self.show_error(&gettext("Could Not Open"), e.message());
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
        // Held weakly while the server is reached, which can take a while: gvfs's questions
        // are for a view that still shows the location, not one gone elsewhere or closed.
        let parent: gtk::Widget = self
            .root()
            .map_or_else(|| self.clone().upcast(), Cast::upcast);
        let weak = self.downgrade();
        let showing = {
            let (weak, file) = (weak.clone(), file.clone());
            move || {
                weak.upgrade()
                    .and_then(|view| view.location())
                    .is_some_and(|now| now.equal(&file))
            }
        };
        glib::spawn_future_local(async move {
            let mounted =
                crate::network::mount(&file, &parent, &gio::Cancellable::new(), showing).await;
            let result = match mounted {
                Ok(()) => crate::network::mounted_at(&file).await,
                Err(e) => Err(e),
            };
            // The answer is about the location asked for; the view may have moved on.
            let Some(view) = weak.upgrade() else { return };
            if view.location().is_none_or(|now| !now.equal(&file)) {
                return;
            }
            match result {
                Ok(mounted) if !mounted.equal(&file) => view.replace_location(&mounted),
                Ok(_) => {
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
        });
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
