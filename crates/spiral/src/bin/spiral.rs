fn main() -> spiral_core::glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started.
    unsafe { spiral_core::init_early() };
    spiral_core::programs::file_manager()
}
