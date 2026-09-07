# Installing

## What you need

To build: Rust 1.88 or newer, `blueprint-compiler`, `just`, and the development packages
of GTK 4.22, libadwaita 1.9, libseccomp and GStreamer. On Fedora that is `gtk4-devel
libadwaita-devel libseccomp-devel gstreamer1-devel blueprint-compiler just`; on Debian
`libgtk-4-dev libadwaita-1-dev libseccomp-dev libgstreamer1.0-dev blueprint-compiler
just`. `just check` also wants `desktop-file-validate` and `appstreamcli`.

To run: GTK 4.22, libadwaita 1.9, GStreamer, a session bus, and xdg-desktop-portal if you
want the file chooser. Everything else is optional and picked up from `PATH` when present:
`bwrap` for sandboxing, archive tools, a terminal emulator. Playing video and sound in the
preview needs the GTK 4 sink from gst-plugins-rs (`gstreamer1-plugin-gtk4` on Fedora,
`gstreamer1.0-gtk4` on Debian, `gst-plugin-gtk4` on Arch) and the plugins for the
formats; without the sink, media files show their icon.

## Build

    just build          # release
    just build-debug
    just check          # rustfmt, clippy, blueprint, desktop and metainfo validation
    just run ~/Music    # debug build, uninstalled

The build produces `spiral`, the portal backend `xdg-desktop-portal-spiral`, and
`spiral-thumbnailer`, a small gdk-pixbuf helper used for images that have no system
thumbnailer.

Two paths are compiled in: where the thumbnailer helper lives (`SPIRAL_LIBEXECDIR`,
default `/usr/local/libexec`) and the locale directory (`SPIRAL_LOCALEDIR`, default
`/usr/local/share/locale`). Set them in the environment of `cargo build` if you install
somewhere else. If the helper is not found at the compiled path, Spiral looks next to its
own binary, which is why an uninstalled build works.

## Install

    sudo just install
    just prefix=$HOME/.local install
    DESTDIR=/tmp/stage just install

`install` copies what `just build` produced; it never builds. The default prefix is
`/usr/local`. It puts the binaries in `bin` and `libexec`, the desktop entries, metainfo,
icon, GSettings schema, portal file, thumbnailer entry and the two D-Bus service files
under `share`, then compiles the schema and refreshes the desktop and icon caches. The
D-Bus service files get the real paths substituted, so a custom prefix works as long as
its `share` directory is in `XDG_DATA_DIRS` (`/usr/local/share` and `~/.local/share` are
on most systems).

xdg-desktop-portal older than 1.17 only reads portal files from
`/usr/share/xdg-desktop-portal/portals`; with another prefix, symlink `spiral.portal` there.

## Make it the default

    just set-default

registers Spiral for `inode/directory`, so `xdg-open` on a folder and "Show in folder"
in browsers land in Spiral.

## The file chooser portal

    just setup-portal

adds `org.freedesktop.impl.portal.FileChooser=spiral` to the `[preferred]` section of
`~/.config/xdg-desktop-portal/portals.conf` and restarts xdg-desktop-portal. If a
`<desktop>-portals.conf` exists there for one of the names in `XDG_CURRENT_DESKTOP`, that
file is edited instead, since it takes precedence.

The backend is started by D-Bus activation the first time a file dialog is requested, so
`WAYLAND_DISPLAY` has to be in the bus activation environment. Sessions that run
`dbus-update-activation-environment` at start are fine. See
[integration.md](integration.md) for what the backend does and how to check it is in use.

## Updating

Spiral stays alive while file operations run, and the portal backend is a long-running
process. After installing a new build:

    pkill -x spiral
    pkill -f xdg-desktop-portal-spiral

The second one needs `-f`: the process name is truncated to 15 characters by the kernel,
so `pkill -x` never matches it.

## Removing

    sudo just uninstall
    just unset-portal

User data stays: settings in dconf, bookmarks in `~/.config/gtk-3.0/bookmarks`, favorites
in `~/.local/share/spiral/starred`, thumbnails in `~/.cache/thumbnails`.
