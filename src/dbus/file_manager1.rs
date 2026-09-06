//! `org.freedesktop.FileManager1`: lets browsers and other apps ask us to show folders/items.

use gtk::prelude::*;

use crate::application::SpiralApplication;
use crate::{gio, glib, gtk};

const XML: &str = r#"<node>
  <interface name="org.freedesktop.FileManager1">
    <method name="ShowFolders"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
    <method name="ShowItems"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
    <method name="ShowItemProperties"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  </interface>
</node>"#;

pub struct Registration {
    main_loop: glib::MainLoop,
}

impl Registration {
    pub fn stop(&self) {
        self.main_loop.quit();
    }
}

/// Serve the interface from a thread of its own.
///
/// The caller is xdg-desktop-portal, which blocks until we answer, with a browser blocked
/// on it in turn. Our main loop cannot answer while GTK is starting up, and GTK's own
/// startup asks that same portal for its settings, so serving this on the main thread
/// deadlocks the two until D-Bus times both out. A separate main context replies whatever
/// the main thread is doing; the window itself is opened back on the main loop.
pub fn register(connection: &gio::DBusConnection) -> Registration {
    let context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&context), false);
    let connection = connection.clone();
    let thread_loop = main_loop.clone();
    std::thread::Builder::new()
        .name("filemanager1".into())
        .spawn(move || {
            let _ = context.with_thread_default(|| {
                let registration = serve(&connection);
                thread_loop.run();
                if let Some((object, owner)) = registration {
                    let _ = connection.unregister_object(object);
                    gio::bus_unown_name(owner);
                }
            });
        })
        .expect("spawn FileManager1 thread");
    Registration { main_loop }
}

fn serve(connection: &gio::DBusConnection) -> Option<(gio::RegistrationId, gio::OwnerId)> {
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            glib::g_warning!("spiral", "FileManager1 interface: {e}");
            return None;
        }
    };
    let iface = node
        .lookup_interface("org.freedesktop.FileManager1")
        .expect("interface in XML");
    let object = connection
        .register_object("/org/freedesktop/FileManager1", &iface)
        .method_call(
            move |_conn, _sender, _path, _iface, method, params, invocation| {
                // Answer before anything else; opening a window can take a moment.
                invocation.return_value(None);
                let (uris, _startup_id) = params.get::<(Vec<String>, String)>().unwrap_or_default();
                let method = method.to_string();
                glib::MainContext::default().invoke(move || {
                    let Some(app) = gio::Application::default().and_downcast::<SpiralApplication>()
                    else {
                        return;
                    };
                    let files: Vec<gio::File> =
                        uris.iter().map(|u| gio::File::for_uri(u)).collect();
                    match method.as_str() {
                        "ShowFolders" => app.show_folders(&files),
                        "ShowItems" => app.show_items(&files),
                        "ShowItemProperties" => app.show_item_properties(&files),
                        _ => {}
                    }
                });
            },
        )
        .build();
    let object = match object {
        Ok(object) => object,
        Err(e) => {
            glib::g_warning!("spiral", "FileManager1 registration failed: {e}");
            return None;
        }
    };
    // Every invocation registers on the bus, so a second `spiral` queues for the name
    // behind the running one for the moment it lives. Only losing a name we held is worth
    // a warning; the queue is normal.
    let held = std::rc::Rc::new(std::cell::Cell::new(false));
    let acquired = held.clone();
    let owner = gio::bus_own_name_on_connection(
        connection,
        "org.freedesktop.FileManager1",
        gio::BusNameOwnerFlags::NONE,
        move |_, name| {
            acquired.set(true);
            glib::g_debug!("spiral", "owning D-Bus name {name}");
        },
        move |_, name| {
            if held.replace(false) {
                glib::g_warning!("spiral", "lost D-Bus name {name}");
            } else {
                glib::g_debug!(
                    "spiral",
                    "waiting for D-Bus name {name}, another file manager holds it"
                );
            }
        },
    );
    Some((object, owner))
}
