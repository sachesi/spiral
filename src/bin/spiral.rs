use spiral::application::SpiralApplication;
use spiral::gio::prelude::*;

fn main() -> spiral::glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started.
    unsafe { spiral::init_early() };
    SpiralApplication::new().run()
}
