//! Volumes, mounts and servers: mounting them, ejecting them, and the trash they keep.

use super::*;
use crate::network::MountTable;

/// The current user's trash directory on `root`, if it holds anything.
pub(super) async fn mount_trash(root: &gio::File) -> Option<gio::File> {
    let uid = unsafe { libc::getuid() };
    let trash = root.child(format!(".Trash-{uid}"));
    let en = trash
        .child("files")
        .enumerate_children_future(
            "standard::name",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        )
        .await
        .ok()?;
    let first = en
        .next_files_future(1, glib::Priority::DEFAULT)
        .await
        .ok()?;
    (!first.is_empty()).then_some(trash)
}

pub(super) async fn wait_finished(job: &crate::ops::Job) {
    if job.is_finished() {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let id = job.connect_status_notify(move |job| {
        if job.is_finished()
            && let Some(tx) = tx.take()
        {
            let _ = tx.send(());
        }
    });
    let _ = rx.await;
    job.disconnect(id);
}

/// Calls back once the drives, volumes and mounts have settled. A disk plugged in is told
/// of as its drive, then each volume on it, then their mounts, with changes between, each a
/// signal of its own; laying the places out again for every one of them makes them flash.
pub(crate) struct DeviceWatch {
    monitor: gio::VolumeMonitor,
    handlers: Vec<glib::SignalHandlerId>,
    pending: std::rc::Rc<Cell<Option<glib::SourceId>>>,
}

impl DeviceWatch {
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

    pub(crate) fn new(changed: impl Fn() + 'static) -> Self {
        let monitor = gio::VolumeMonitor::get();
        let pending = std::rc::Rc::new(Cell::new(None::<glib::SourceId>));
        let changed = std::rc::Rc::new(changed);
        let settle = glib::clone!(
            #[strong]
            pending,
            move || {
                if let Some(id) = pending.take() {
                    id.remove();
                }
                let fired = pending.clone();
                let changed = changed.clone();
                pending.set(Some(glib::timeout_add_local_once(
                    Self::SETTLE,
                    move || {
                        fired.take();
                        changed();
                    },
                )));
            }
        );
        macro_rules! on {
            ($connect:ident) => {
                monitor.$connect(glib::clone!(
                    #[strong]
                    settle,
                    move |_, _| settle()
                ))
            };
        }
        let handlers = vec![
            on!(connect_mount_added),
            on!(connect_mount_removed),
            on!(connect_mount_changed),
            on!(connect_volume_added),
            on!(connect_volume_removed),
            on!(connect_volume_changed),
            on!(connect_drive_connected),
            on!(connect_drive_disconnected),
        ];
        Self {
            monitor,
            handlers,
            pending,
        }
    }
}

impl Drop for DeviceWatch {
    fn drop(&mut self) {
        for id in self.handlers.drain(..) {
            self.monitor.disconnect(id);
        }
        if let Some(id) = self.pending.take() {
            id.remove();
        }
    }
}

/// Whether the user can take `mount` out, and so whether it gets a button for it. A disk
/// udisks mounted for the user, a share and a FUSE filesystem of the user's own are theirs
/// to unmount. One the system mounted is not, unless fstab lets users unmount it: asking
/// ends in "must be superuser", or in a password prompt for the administrator.
pub(crate) fn may_unmount(mount: &gio::Mount, mounts: &MountTable) -> bool {
    if !(mount.can_unmount() || mount.can_eject()) {
        return false;
    }
    let root = mount.root();
    if !root.is_native() {
        return true;
    }
    let Some(path) = root.path() else {
        return true;
    };
    if path.starts_with("/run/media") || path.starts_with("/media") {
        return true;
    }
    let Some(kernel) = mounts.at(&path) else {
        return true;
    };
    let uid = unsafe { libc::getuid() };
    if kernel.fstype.starts_with("fuse")
        && kernel
            .options
            .split(',')
            .any(|o| o == format!("user_id={uid}"))
    {
        return true;
    }
    gio_unix::MountPoint::at(&path)
        .0
        .is_some_and(|point| point.is_user_mountable())
}

/// The mount `file` is the root of: a device, not a folder that merely lives on one.
pub(crate) fn mount_of(file: &gio::File) -> Option<gio::Mount> {
    gio::VolumeMonitor::get()
        .mounts()
        .into_iter()
        .find(|m| m.root().equal(file) || m.default_location().equal(file))
}

/// What a volume is hidden as: what names the disk wherever it is plugged in.
pub(crate) fn volume_hide_key(volume: &gio::Volume) -> Option<String> {
    volume.uuid().map(|u| format!("uuid:{u}")).or_else(|| {
        volume
            .identifier("unix-device")
            .map(|d| format!("device:{d}"))
    })
}

/// What a mount without a volume is hidden as: its UUID, or else its location.
pub(crate) fn mount_hide_key(mount: &gio::Mount) -> String {
    mount
        .uuid()
        .map(|u| format!("uuid:{u}"))
        .unwrap_or_else(|| mount.default_location().uri().to_string())
}

#[derive(Clone)]
pub(crate) enum EjectTarget {
    Mount(gio::Mount),
    Volume(gio::Volume),
}

impl EjectTarget {
    /// Whether it stands for a server rather than for something plugged in.
    pub(crate) fn is_network(&self, mounts: &MountTable) -> bool {
        match self {
            EjectTarget::Mount(m) => crate::network::is_network_mount(&m.root(), mounts),
            EjectTarget::Volume(_) => false,
        }
    }
}

impl PlacesSidebar {
    pub(super) fn volume_row(&self, volume: &gio::Volume, mounts: &MountTable) -> gtk::ListBoxRow {
        let (row, content) = make_row(&volume.symbolic_icon(), &volume.name(), SECTION_DEVICES);
        if let Some(key) = volume_hide_key(volume) {
            HIDE_KEY.set(&row, key);
        }
        if let Some(mount) = volume.get_mount() {
            ROW_FILE.set(&row, mount.default_location());
            add_drop_target(&row, &mount.default_location());
            if may_unmount(&mount, mounts) {
                content.append(&self.eject_button(&row, EjectTarget::Mount(mount), mounts));
            }
        } else {
            VOLUME.set(&row, volume.clone());
            if volume.can_eject() {
                content.append(&self.eject_button(
                    &row,
                    EjectTarget::Volume(volume.clone()),
                    mounts,
                ));
            }
        }
        row
    }

    pub(super) fn mount_row(
        &self,
        mount: &gio::Mount,
        section: u8,
        mounts: &MountTable,
    ) -> gtk::ListBoxRow {
        let (row, content) = make_row(&mount.symbolic_icon(), &mount.name(), section);
        HIDE_KEY.set(&row, mount_hide_key(mount));
        ROW_FILE.set(&row, mount.default_location());
        add_drop_target(&row, &mount.default_location());
        if may_unmount(mount, mounts) {
            content.append(&self.eject_button(&row, EjectTarget::Mount(mount.clone()), mounts));
        }
        row
    }

    pub(super) fn eject_button(
        &self,
        row: &gtk::ListBoxRow,
        target: EjectTarget,
        mounts: &MountTable,
    ) -> gtk::Button {
        EJECT.set(row, target.clone());
        let button = gtk::Button::builder()
            .icon_name("media-eject-symbolic")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .margin_start(4)
            .tooltip_text(if target.is_network(mounts) {
                gettext("Disconnect")
            } else {
                gettext("Eject")
            })
            .css_classes(["flat"])
            .build();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |_| sidebar.eject(target.clone())
        ));
        button
    }

    pub(super) fn mount_operation(&self) -> gtk::MountOperation {
        let win = self.root().and_downcast::<gtk::Window>();
        gtk::MountOperation::new(win.as_ref())
    }

    pub(super) fn mount_and_open(&self, volume: gio::Volume) {
        let op = self.mount_operation();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volume
                    .mount_future(gio::MountMountFlags::NONE, Some(&op))
                    .await
                {
                    Ok(()) => {
                        if let Some(mount) = volume.get_mount() {
                            sidebar.emit_by_name::<()>(
                                "open-location",
                                &[&mount.default_location(), &false],
                            );
                        }
                    }
                    // The password or passphrase dialog was dismissed: nothing to report.
                    Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                    Err(e) => sidebar.show_error(&gettext("Could Not Mount"), &e),
                }
            }
        ));
    }

    /// Unmount or eject the device `file` is the root of, if it is the root of one.
    /// Returns whether there was anything to unmount.
    pub(crate) fn eject_file(&self, file: &gio::File) -> bool {
        let Some(mount) = mount_of(file) else {
            return false;
        };
        self.eject(EjectTarget::Mount(mount));
        true
    }

    pub(crate) fn eject(&self, target: EjectTarget) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move { sidebar.unmount(target).await }
        ));
    }

    /// Eject or unmount `target`, and return once it is out or could not be taken out.
    pub(crate) async fn unmount(&self, target: EjectTarget) {
        let op = self.mount_operation();
        let flags = gio::MountUnmountFlags::NONE;
        // A volume's own trash is lost once it is unplugged; offer to empty it first.
        // Not for a share: gvfs trashes nothing onto one, so a trash folder there is
        // the server's own, and emptying it would delete what someone trashed there.
        if let EjectTarget::Mount(m) = &target
            && !target.is_network(&MountTable::read())
            && let Some(trash) = mount_trash(&m.root()).await
            && !self.offer_empty_trash(trash).await
        {
            return;
        }
        // Removable media: warn while cached writes flush, then say when it is safe.
        let (name, drive) = match &target {
            EjectTarget::Mount(m) => (m.name(), m.drive()),
            EjectTarget::Volume(v) => (v.name(), v.drive()),
        };
        let removable = drive.is_some_and(|d| d.is_removable() || d.is_media_removable());
        let app = gio::Application::default();
        if removable && let Some(app) = &app {
            let n = gio::Notification::new(&gettext("Writing data to “%s”").replace("%s", &name));
            n.set_body(Some(&gettext("Don’t unplug until finished")));
            app.send_notification(Some("unmount"), &n);
        }
        let result = match target {
            EjectTarget::Mount(m) if m.can_eject() => {
                m.eject_with_operation_future(flags, Some(&op)).await
            }
            EjectTarget::Mount(m) => m.unmount_with_operation_future(flags, Some(&op)).await,
            EjectTarget::Volume(v) => v.eject_with_operation_future(flags, Some(&op)).await,
        };
        if removable && let Some(app) = &app {
            app.withdraw_notification("unmount");
        }
        match result {
            Ok(()) if removable => {
                if let Some(app) = &app {
                    let n = gio::Notification::new(
                        &gettext("You can now unplug “%s”").replace("%s", &name),
                    );
                    app.send_notification(Some("unmount-done"), &n);
                }
            }
            Err(e) if !e.matches(gio::IOErrorEnum::FailedHandled) => {
                self.show_error(&gettext("Could Not Eject"), &e);
            }
            _ => {}
        }
    }

    /// Asks about the trash on a volume about to be unmounted. Returns false to abort.
    pub(super) async fn offer_empty_trash(&self, trash: gio::File) -> bool {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Empty Trash Before Unmounting?"))
            .body(gettext(
                "To regain the free space on this volume the trash must be emptied. All trashed items on the volume will be permanently lost.",
            ))
            .build();
        dialog.add_response("cancel", &gettext("_Cancel"));
        dialog.add_response("keep", &gettext("Do _Not Empty"));
        dialog.add_response("empty", &gettext("_Empty Trash"));
        dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("cancel");
        match dialog.choose_future(Some(self)).await.as_str() {
            "empty" => {
                let Some(app) = gio::Application::default()
                    .and_downcast::<crate::application::SpiralApplication>()
                else {
                    return true;
                };
                let job = app
                    .job_manager()
                    .submit(crate::ops::JobKind::Delete { files: vec![trash] });
                wait_finished(&job).await;
                true
            }
            "keep" => true,
            _ => false,
        }
    }

    pub(super) fn show_error(&self, heading: &str, error: &glib::Error) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(error.message())
            .build();
        dialog.add_response("ok", &gettext("_OK"));
        dialog.present(Some(self));
    }
}
