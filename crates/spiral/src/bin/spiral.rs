use spiral_core::application::SpiralApplication;
use spiral_core::gio::prelude::*;

fn main() -> spiral_core::glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started.
    unsafe { spiral_core::init_early() };
    SpiralApplication::new().run()
}
