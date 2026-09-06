//! FileChooser portal backend: serves `org.freedesktop.impl.portal.desktop.spiral` and shows
//! Spiral's chooser window for each request.

use ashpd::zbus;
use futures_util::StreamExt;
use spiral::portal::backend::{BUS_NAME, SpiralChooser};
use spiral::portal::chooser_window;
use spiral::{glib, gtk};

fn main() {
    // No GtkApplication here, so GDK falls back to the program name for the Wayland
    // app_id; it must match the hidden desktop entry for icon and name lookup.
    glib::set_prgname(Some("xdg-desktop-portal-spiral"));
    let (tx, rx) = async_channel::unbounded();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();

    std::thread::Builder::new()
        .name("portal-dbus".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let result = rt.block_on(async move {
                let failed = |e: zbus::Error| ashpd::PortalError::Failed(e.to_string());
                let connection = zbus::Connection::session().await.map_err(failed)?;
                // GTK asks xdg-desktop-portal for settings during init. When that daemon
                // is the one activating us it sits in a blocking call until our name is
                // on the bus, so the main thread must not touch GTK before then.
                let mut acquired = zbus::fdo::DBusProxy::new(&connection)
                    .await
                    .map_err(failed)?
                    .receive_name_acquired()
                    .await
                    .map_err(failed)?;
                tokio::spawn(async move {
                    if acquired.next().await.is_some() {
                        let _ = ready_tx.send(());
                    }
                });
                ashpd::backend::Builder::new(BUS_NAME)?
                    .file_chooser(SpiralChooser::new(tx))
                    .with_name_lost(|| std::process::exit(0))
                    .build_with_connection(connection)
                    .await
            });
            if let Err(e) = result {
                eprintln!("xdg-desktop-portal-spiral: {e}");
                std::process::exit(1);
            }
        })
        .expect("spawn thread");

    // A closed channel means the bus thread failed and is exiting; do not start GTK meanwhile.
    if ready_rx.recv().is_err() {
        std::process::exit(1);
    }
    spiral::init();

    let main_loop = glib::MainLoop::new(None, false);
    glib::MainContext::default().spawn_local(async move {
        while let Ok(req) = rx.recv().await {
            glib::spawn_future_local(chooser_window::handle(req));
        }
    });
    // Keep the GTK settings object alive so the theme follows the session.
    let _settings = gtk::Settings::default();
    main_loop.run();
}
