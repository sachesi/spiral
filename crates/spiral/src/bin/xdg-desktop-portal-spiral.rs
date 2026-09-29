fn main() {
    // No GtkApplication here, so GDK falls back to the program name for the Wayland
    // app_id; it must match the hidden desktop entry for icon and name lookup.
    spiral_core::glib::set_prgname(Some("xdg-desktop-portal-spiral"));
    // SAFETY: before the bus thread the portal starts, the only other thread of the program.
    unsafe { spiral_core::init_early() };
    spiral_core::programs::portal();
}
