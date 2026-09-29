//! The programs Spiral installs, each run from a `main` of a few lines in `crates/spiral`.
//! They are here for those alone and are no part of the library's API.

use std::process::ExitCode;

use ashpd::zbus;
use futures_util::StreamExt;

use crate::application::SpiralApplication;
use crate::gio::prelude::*;
use crate::gtk::gdk_pixbuf::Pixbuf;
use crate::portal::backend::{BUS_NAME, SpiralChooser};
use crate::portal::chooser_window;
use crate::{glib, gtk};

/// The file manager. Run [`crate::init_early`] first.
pub fn file_manager() -> glib::ExitCode {
    SpiralApplication::new().run()
}

/// The FileChooser portal backend: serves `org.freedesktop.impl.portal.desktop.spiral` and
/// shows Spiral's chooser window for each request. Set the program name and run
/// [`crate::init_early`] first; the bus thread started here is the only other thread of the
/// program.
pub fn portal() {
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
    crate::init();

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

/// The helper run inside the thumbnail sandbox: renders one image into a PNG with
/// gdk-pixbuf, so image decoders never run in the file manager process. With `--probe`, it
/// prints what a photo, a recording, a video or a document says about itself instead, for
/// the details panel. With `--decode`, it decodes a picture whole for the preview, and with
/// `--play` it decodes a video or a recording for the preview's player.
pub fn thumbnailer() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, kind, input] = args.as_slice()
        && flag == "--probe"
    {
        print!(
            "{}",
            crate::metadata::probe(kind, std::path::Path::new(input))
        );
        return ExitCode::SUCCESS;
    }
    if let [_, flag, source, control, video, audio, formats] = args.as_slice()
        && flag == "--play"
    {
        let fd = |s: &String| s.parse().unwrap_or(-1);
        return match crate::media::serve(source, fd(control), fd(video), fd(audio), formats) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spiral-thumbnailer: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if let [_, flag, input, output, orientation] = args.as_slice()
        && flag == "--decode"
    {
        let orientation = orientation.parse().unwrap_or(1);
        return match crate::picture::decode(input.as_ref(), output.as_ref(), orientation) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spiral-thumbnailer: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let [_, input, output, size] = args.as_slice() else {
        eprintln!("usage: spiral-thumbnailer INPUT OUTPUT SIZE");
        return ExitCode::from(2);
    };
    let size: i32 = size.parse().unwrap_or(256);
    let result = Pixbuf::from_file_at_scale(input, size, size, true).and_then(|pb| {
        let pb = pb.apply_embedded_orientation().unwrap_or(pb);
        pb.savev(output, "png", &[])
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spiral-thumbnailer: {e}");
            ExitCode::FAILURE
        }
    }
}
