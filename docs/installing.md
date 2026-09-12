# Installing

## What you need

To build: Rust 1.88 or newer, `blueprint-compiler`, `just`, and the development packages
of GTK 4.22, libadwaita 1.9, GtkSourceView 5, libseccomp and GStreamer. On Fedora that is
`gtk4-devel libadwaita-devel gtksourceview5-devel libseccomp-devel gstreamer1-devel
blueprint-compiler just`; on Debian `libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev
libseccomp-dev libgstreamer1.0-dev blueprint-compiler just`. `just check` also wants
`desktop-file-validate` and `appstreamcli`.

To run: GTK 4.22, libadwaita 1.9, GtkSourceView 5, `bwrap` from bubblewrap, a session bus,
and xdg-desktop-portal if you want the file chooser. Bubblewrap is not optional: every
thumbnailer, the PDF previewer and every archive tool runs inside it, and without it those
are turned off rather than run unconfined. GtkSourceView colours the text files the preview shows. The
preview plays video and sound through GStreamer, which needs its base plugins and the GTK 4
sink from gst-plugins-rs: on Fedora `gstreamer1-plugins-base
gstreamer1-plugin-gtk4`, on Debian `libgstreamer-plugins-base1.0-0 gstreamer1.0-gtk4`, on
Arch `gst-plugins-base gst-plugin-gtk4`. The plugins for the formats themselves come from
`gstreamer1-plugins-good` and its siblings; a file whose format has no plugin shows its
icon instead. Network locations need gvfs and a backend for the protocol wanted: on Fedora
`gvfs` plus `gvfs-smb` or `gvfs-nfs`, on Debian `gvfs-backends`. Without gvfs Spiral opens
local files only, and says as much where it would otherwise offer a server. Everything else
is optional and picked up from `PATH` when present: archive tools, a terminal emulator.

Installing the sink after Spiral has run once may leave a stale GStreamer plugin registry
behind, and the preview then reports the sink missing until the registry is rebuilt:
`rm -f ~/.cache/gstreamer-1.0/registry.*.bin`.

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

Qt applications outside a Flatpak only go through the portal with Qt's portal platform
theme; otherwise they show Qt's own dialog, or GTK 3's on GNOME and its relatives. So the
same command writes `QT_QPA_PLATFORMTHEME=xdgdesktopportal` to
`~/.config/environment.d/60-spiral-qt-portal.conf`, which the session picks up at the next
login, unless `QT_QPA_PLATFORMTHEME` is already set to something else, such as `qt6ct`,
which it leaves alone. Where it is left at Plasma's own theme, `kde`,
`PLASMA_INTEGRATION_USE_PORTAL=1` sends that theme's dialogs to the portal instead.

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
in `~/.local/share/spiral/starred`, the tag index in `~/.local/share/spiral/tags`, the tags
on the files themselves, thumbnails in `~/.cache/thumbnails`.
