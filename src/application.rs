use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;

use crate::config;
use crate::ops::JobManager;
use crate::window::SpiralWindow;
use crate::{adw, gio, glib};
use std::ops::ControlFlow;
use std::os::unix::ffi::OsStrExt;

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

        /// D-Bus activation without arguments: a window at the home folder.
        fn activate(&self) {
            self.obj().open_window(&[]);
        }

        fn handle_local_options(&self, options: &glib::VariantDict) -> ControlFlow<glib::ExitCode> {
            if options.contains("version") {
                println!("spiral {}", config::VERSION);
                return ControlFlow::Break(glib::ExitCode::SUCCESS);
            }
            ControlFlow::Continue(())
        }

        /// Every invocation reaches the running instance here, like Nautilus: `-q` quits it,
        /// anything else opens new windows.
        fn command_line(&self, cmdline: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let app = self.obj();
            let options = cmdline.options_dict();
            if options.contains("quit") {
                app.quit_after_jobs();
                return glib::ExitCode::SUCCESS;
            }
            let files: Vec<gio::File> = options
                .lookup_value("", Some(glib::VariantTy::BYTE_STRING_ARRAY))
                .and_then(|v| v.get::<Vec<Vec<u8>>>())
                .unwrap_or_default()
                .iter()
                .map(|arg| {
                    let arg = arg.strip_suffix(&[0]).unwrap_or(arg);
                    cmdline.create_file_for_arg(std::ffi::OsStr::from_bytes(arg))
                })
                .collect();
            glib::g_debug!(
                "spiral",
                "command line: {} files, new-window={}, select={}",
                files.len(),
                options.contains("new-window"),
                options.contains("select")
            );
            let first = if options.contains("select") {
                app.select_in_windows(&files)
            } else if options.contains("new-window") && files.len() > 1 {
                let windows: Vec<SpiralWindow> = files
                    .iter()
                    .map(|f| app.open_window(std::slice::from_ref(f)))
                    .collect();
                windows.into_iter().next()
            } else {
                Some(app.open_window(&files))
            };
            if app.job_manager().running() > 0
                && let Some(win) = first
            {
                win.show_progress();
            }
            glib::ExitCode::SUCCESS
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

fn group_by_parent(files: &[gio::File]) -> Vec<(gio::File, Vec<gio::File>)> {
    let mut groups: Vec<(gio::File, Vec<gio::File>)> = Vec::new();
    for f in files {
        let Some(parent) = f.parent() else { continue };
        match groups.iter_mut().find(|(p, _)| p.equal(&parent)) {
            Some((_, items)) => items.push(f.clone()),
            None => groups.push((parent, vec![f.clone()])),
        }
    }
    groups
}

impl Default for SpiralApplication {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiralApplication {
    pub fn new() -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .property("resource-base-path", config::RESOURCE_PATH)
            .build();
        let flag = |long: &str, short: u8, description: String| {
            app.add_main_option(
                long,
                glib::Char::from(short),
                glib::OptionFlags::NONE,
                glib::OptionArg::None,
                &description,
                None,
            );
        };
        flag(
            "new-window",
            b'w',
            gettext("Open each location in its own window"),
        );
        flag(
            "select",
            b's',
            gettext("Select the given files in their folders"),
        );
        flag("quit", b'q', gettext("Close every window and quit"));
        flag("version", 0, gettext("Print the version and exit"));
        app.add_main_option(
            "",
            glib::Char::from(0),
            glib::OptionFlags::NONE,
            glib::OptionArg::FilenameArray,
            "",
            Some(&gettext("[FILE…]")),
        );
        app
    }

    /// `-q`: cancel running jobs, let their cleanup finish, then quit. Ten seconds is the
    /// most a stuck job gets.
    fn quit_after_jobs(&self) {
        let mgr = self.job_manager();
        if mgr.running() == 0 {
            self.quit();
            return;
        }
        mgr.cancel_all();
        mgr.connect_running_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |m| {
                if m.running() == 0 {
                    app.quit();
                }
            }
        ));
        glib::timeout_add_seconds_local_once(
            10,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || app.quit()
            ),
        );
    }

    /// A new window showing `files` as tabs, or the home folder when there are none.
    pub fn open_window(&self, files: &[gio::File]) -> SpiralWindow {
        let win = SpiralWindow::new(self);
        if files.is_empty() {
            win.open_location(&gio::File::for_path(glib::home_dir()));
        }
        for f in files {
            win.open_location(f);
        }
        win.present();
        win
    }

    /// `--select`: one window per parent folder with the given files selected.
    fn select_in_windows(&self, files: &[gio::File]) -> Option<SpiralWindow> {
        let mut first = None;
        for (parent, items) in group_by_parent(files) {
            let win = self.open_window(std::slice::from_ref(&parent));
            if let Some(view) = win.current_view() {
                view.select_files_when_loaded(items);
            }
            first.get_or_insert(win);
        }
        first
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
        for (parent, items) in group_by_parent(files) {
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
            // Translators: put your name here, one per line, optionally with an email address.
            .translator_credits(gettext("translator-credits"))
            .build();
        about.present(self.active_window().as_ref());
    }
}
