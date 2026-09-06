use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;

use crate::config;
use crate::ops::JobManager;
use crate::window::SpiralWindow;
use crate::{adw, gio, glib};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SpiralApplication {
        pub job_manager: std::cell::OnceCell<JobManager>,
        pub file_manager1: std::cell::RefCell<Option<crate::dbus::file_manager1::Registration>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SpiralApplication {
        const NAME: &'static str = "SpiralApplication";
        type Type = super::SpiralApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for SpiralApplication {}

    impl ApplicationImpl for SpiralApplication {
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();
            self.job_manager.set(JobManager::new(&app)).ok();
            app.setup_actions();
        }

        fn activate(&self) {
            let app = self.obj();
            let win = app.present_window();
            if win.current_view().is_none() {
                win.open_location(&gio::File::for_path(glib::home_dir()));
            }
            if app.job_manager().running() > 0 {
                win.show_progress();
            }
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            let win = self.obj().present_window();
            for f in files {
                win.open_location(f);
            }
        }

        fn dbus_register(
            &self,
            connection: &gio::DBusConnection,
            object_path: &str,
        ) -> Result<(), glib::Error> {
            self.parent_dbus_register(connection, object_path)?;
            match crate::dbus::file_manager1::register(&self.obj(), connection) {
                Ok(reg) => {
                    self.file_manager1.replace(Some(reg));
                }
                Err(e) => glib::g_warning!("spiral", "FileManager1 registration failed: {e}"),
            }
            Ok(())
        }

        fn dbus_unregister(&self, connection: &gio::DBusConnection, object_path: &str) {
            if let Some(reg) = self.file_manager1.take() {
                let _ = connection.unregister_object(reg.object);
                gio::bus_unown_name(reg.owner);
            }
            self.parent_dbus_unregister(connection, object_path);
        }
    }

    impl GtkApplicationImpl for SpiralApplication {}
    impl AdwApplicationImpl for SpiralApplication {}
}

glib::wrapper! {
    pub struct SpiralApplication(ObjectSubclass<imp::SpiralApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

use crate::gtk;

impl Default for SpiralApplication {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiralApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", config::RESOURCE_PATH)
            .build()
    }

    /// FileManager1.ShowFolders: one tab per folder.
    pub fn show_folders(&self, files: &[gio::File]) {
        let win = self.present_window();
        for f in files {
            win.open_location(f);
        }
    }

    /// FileManager1.ShowItems: open each parent folder and select the items in it.
    pub fn show_items(&self, files: &[gio::File]) {
        let win = self.present_window();
        let mut groups: Vec<(gio::File, Vec<gio::File>)> = Vec::new();
        for f in files {
            let Some(parent) = f.parent() else { continue };
            match groups.iter_mut().find(|(p, _)| p.equal(&parent)) {
                Some((_, items)) => items.push(f.clone()),
                None => groups.push((parent, vec![f.clone()])),
            }
        }
        for (parent, items) in groups {
            win.open_location(&parent);
            if let Some(view) = win.current_view() {
                view.select_files_when_loaded(items);
            }
        }
    }

    pub fn show_item_properties(&self, files: &[gio::File]) {
        let win = self.present_window();
        if !files.is_empty() {
            crate::dialogs::PropertiesDialog::new(files).present(Some(&win));
        }
    }

    pub fn job_manager(&self) -> &JobManager {
        self.imp()
            .job_manager
            .get()
            .expect("job manager created at startup")
    }

    /// Returns the most recently active window, creating one if none exists.
    fn present_window(&self) -> SpiralWindow {
        let win = self
            .active_window()
            .and_downcast::<SpiralWindow>()
            .unwrap_or_else(|| SpiralWindow::new(self));
        win.present();
        win
    }

    fn setup_actions(&self) {
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &Self, _, _| app.quit())
            .build();
        let new_window = gio::ActionEntry::builder("new-window")
            .activate(|app: &Self, _, _| SpiralWindow::new(app).present())
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(|app: &Self, _, _| {
                crate::dialogs::preferences_dialog().present(app.active_window().as_ref());
            })
            .build();
        self.add_action_entries([quit, new_window, about, preferences]);
        self.set_accels_for_action("app.preferences", &["<Control>comma"]);

        let mgr = self.job_manager();
        let undo = gio::SimpleAction::new("undo", None);
        undo.connect_activate(glib::clone!(
            #[weak]
            mgr,
            move |_, _| mgr.undo()
        ));
        mgr.bind_property("can-undo", &undo, "enabled")
            .sync_create()
            .build();
        let redo = gio::SimpleAction::new("redo", None);
        redo.connect_activate(glib::clone!(
            #[weak]
            mgr,
            move |_, _| mgr.redo()
        ));
        mgr.bind_property("can-redo", &redo, "enabled")
            .sync_create()
            .build();
        self.add_action(&undo);
        self.add_action(&redo);
        self.set_accels_for_action("app.undo", &["<Control>z"]);
        self.set_accels_for_action("app.redo", &["<Control><Shift>z"]);

        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.new-window", &["<Control>n"]);
        self.set_accels_for_action("window.close", &["<Control><Shift>w"]);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Spiral")
            .application_icon(config::APP_ID)
            .developer_name("sachesi")
            .version(config::VERSION)
            .website("https://github.com/sachesi/spiral")
            .issue_url("https://github.com/sachesi/spiral/issues")
            .license_type(gtk::License::Gpl30)
            .comments(gettext("Browse and manage your files"))
            .build();
        about.present(self.active_window().as_ref());
    }
}
