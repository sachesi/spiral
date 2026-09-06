pub const APP_ID: &str = "io.github.sachesi.spiral";
pub const RESOURCE_PATH: &str = "/io/github/sachesi/spiral";
pub const VERSION: &str = match option_env!("SPIRAL_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
pub const LOCALEDIR: &str = match option_env!("SPIRAL_LOCALEDIR") {
    Some(v) => v,
    None => "/usr/local/share/locale",
};
pub const GETTEXT_PACKAGE: &str = "spiral";
pub const LIBEXECDIR: &str = match option_env!("SPIRAL_LIBEXECDIR") {
    Some(v) => v,
    None => "/usr/local/libexec",
};
