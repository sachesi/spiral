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
    pub object: gio::RegistrationId,
    pub owner: gio::OwnerId,
}

pub fn register(
    app: &SpiralApplication,
    connection: &gio::DBusConnection,
) -> Result<Registration, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(XML)?;
    let iface = node
        .lookup_interface("org.freedesktop.FileManager1")
        .expect("interface in XML");
    let app = app.downgrade();
    let object = connection
        .register_object("/org/freedesktop/FileManager1", &iface)
        .method_call(
            move |_conn, _sender, _path, _iface, method, params, invocation| {
                let Some(app) = app.upgrade() else {
                    invocation.return_value(None);
                    return;
                };
                let (uris, _startup_id) = params.get::<(Vec<String>, String)>().unwrap_or_default();
                let files: Vec<gio::File> = uris.iter().map(|u| gio::File::for_uri(u)).collect();
                match method {
                    "ShowFolders" => app.show_folders(&files),
                    "ShowItems" => app.show_items(&files),
                    "ShowItemProperties" => app.show_item_properties(&files),
                    _ => {}
                }
                invocation.return_value(None);
            },
        )
        .build()?;
    let owner = gio::bus_own_name_on_connection(
        connection,
        "org.freedesktop.FileManager1",
        gio::BusNameOwnerFlags::NONE,
        |_, _| {},
        |_, name| glib::g_warning!("spiral", "lost D-Bus name {name}"),
    );
    Ok(Registration { object, owner })
}
