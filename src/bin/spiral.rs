use spiral::application::SpiralApplication;
use spiral::gio::prelude::*;

fn main() -> spiral::glib::ExitCode {
    spiral::init();
    SpiralApplication::new().run()
}
