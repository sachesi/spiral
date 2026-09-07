# Spiral

Spiral is a file manager for Wayland, written in Rust with GTK 4 and libadwaita. It is
also a file chooser portal backend, so other applications open and save files in the same
dialog.

It does not depend on any desktop session. You need a Wayland compositor, a session bus,
GTK 4.22 and libadwaita 1.9.

Grid, list and column views, tabs, a places sidebar with devices and bookmarks, background file
operations with progress, conflict handling and undo, archives through whatever tools are
installed, thumbnails, drag and drop, "Open in Terminal", the `org.freedesktop.FileManager1`
service for "Show in folder" and the portal backend. Thumbnailers and archive tools run under
bubblewrap when it is installed.

## Building and installing

    just build
    sudo just install        # or: just prefix=$HOME/.local install
    just set-default         # folders open in Spiral
    just setup-portal        # file dialogs of other apps use Spiral

Build needs Rust 1.88, `blueprint-compiler`, `just` and the development packages for GTK,
libadwaita and libseccomp. Details, other prefixes and removal are in
[docs/installing.md](docs/installing.md).

After an update, quit the old processes: `pkill -x spiral; pkill -f xdg-desktop-portal-spiral`.

## Documentation

- [Installing](docs/installing.md)
- [Using Spiral](docs/usage.md), including [keyboard shortcuts](docs/keyboard-shortcuts.md) and [settings](docs/settings.md)
- [File operations](docs/file-operations.md)
- [Archives](docs/archives.md), [thumbnails](docs/thumbnails.md), [terminal](docs/terminal.md)
- [Portal and D-Bus integration](docs/integration.md)
- [Contributing](CONTRIBUTING.md), including where things are in the code

Search walks subfolders and can look inside text files, without an index. Tabs are not
restored between runs. The interface is available in English, Russian and Ukrainian.

GPL-3.0-or-later.
