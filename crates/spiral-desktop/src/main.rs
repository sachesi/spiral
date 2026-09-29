fn main() -> spiral_desktop::glib::ExitCode {
    // SAFETY: first thing in main, before any thread is started.
    unsafe { spiral_core::init_early() };
    spiral_desktop::run()
}
