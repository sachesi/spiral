# Hacking

## Where things are

    build.rs               runs blueprint-compiler, bundles the GResource
    data/ui/*.blp          the window, browser view, path bar, sidebar, shortcuts dialog
    data/style.css         structural CSS, colours come from libadwaita
    src/bin/               the three executables, thin mains
    src/application.rs     GtkApplication subclass, app actions, FileManager1 registration
    src/window.rs          tabs, header bar, location entry, win.* actions
    src/browser_view.rs    one tab: grid and list, selection, view mode, drag and drop
    src/browser_actions.rs view.* actions, context menus, rename and new-folder flows
    src/miller.rs          column view: the folder chain as a strip of lists
    src/folder_model.rs    list model of a folder: monitoring, sorting, filtering
    src/places_sidebar.rs  sidebar, mounts, eject, bookmark drag and drop
    src/path_bar.rs        breadcrumbs
    src/location_entry.rs  inline completion, and the path bar the choosers type in
    src/ops/               jobs: job.rs, manager.rs (queue and undo), walk.rs (copy, move,
                           trash, delete), conflict.rs (dialogs), archive.rs
    src/thumbnails.rs      thumbnail lookup and generation, the bwrap sandbox, seccomp
    src/terminal.rs        terminal discovery
    src/portal/            the chooser backend: backend.rs (ashpd), chooser_window.rs
    src/dbus/              FileManager1
    src/dialogs/           preferences, properties, open with, compress, folder chooser
    src/prefs.rs           settings that affect formatting and loading
    src/naming.rs          name validation, rename popover, new folder dialog
    src/file_utils.rs      names, dates, sizes, icons

Widgets are GObject subclasses with composite templates from the Blueprint files. Actions
use the usual prefixes: `app.`, `win.`, `view.` on the browser view (the chooser reuses
it), `sidebar.`.

I/O is async GIO on the main context. What GIO has no async form for — decoding a picture,
walking a tree with `std::fs`, waiting on a sandboxed tool, asking gvfs a question over the
bus — goes to a worker through `gio::spawn_blocking`, with only values that can cross
threads coming back; GObjects stay where they were made. Beside those workers the only
threads are the sandboxed children and the D-Bus thread of the portal backend.

Nothing that touches the disk belongs on a path the interface takes often. The answers
that would (which terminals are installed, what the bookmarks file says, whether gvfs
keeps per-folder metadata) are read once and kept, and forgotten again when something
says they have changed.

## Running

    just run [PATH]      # debug build with the schema compiled into target/schemas
    just run-portal      # the backend, needs a session bus and xdg-desktop-portal
    just check           # what CI would run: fmt, clippy -D warnings, blueprint, validators

`G_MESSAGES_DEBUG=spiral` enables the debug log domain.

To poke at the backend without xdg-desktop-portal, start it on a private bus and call it
with `gdbus`:

    export DBUS_SESSION_BUS_ADDRESS=$(dbus-daemon --session --fork --print-address)
    target/debug/xdg-desktop-portal-spiral &
    gdbus call --session --dest org.freedesktop.impl.portal.desktop.spiral \
        --object-path /org/freedesktop/portal/desktop \
        --method org.freedesktop.impl.portal.FileChooser.OpenFile \
        /org/freedesktop/portal/desktop/request/t/1 '' '' 'Test' \
        "{'choices': <[('enc', 'Encoding', [('utf8','UTF-8'),('latin1','Latin-1')], 'utf8')]>}"

The whole thing also runs headless under Xvfb with `GDK_BACKEND=x11` and an isolated
`HOME`, which is handy for trying things on a machine without a desktop.

## A few things to know before changing them

User-visible strings go through `gettext` with `%s`-style placeholders and
`str::replace`; there is no printf from Rust. Counts use `ngettext` even where English
would not need it, because the plural rules of other languages do. The catalogues live in
`po/`; `just pot` regenerates the template from the Rust sources, the Blueprint files, the
desktop entries, the metainfo and the schema, and `just po` merges it into every
`po/<lang>.po`. A new language is a new line in `po/LINGUAS` plus the `.po` file. `install`
compiles the catalogues and merges the desktop and metainfo translations with `msgfmt`.

Anything that is a tool rather than a library (archives, terminals, thumbnailers) is
discovered in `PATH` at use time and degrades to "not offered" when missing. Child
processes that touch untrusted data go through `sandbox_base` in `thumbnails.rs`.

The portal backend must own its bus name before GTK is initialised, see
[integration.md](integration.md).

Where behaviour was a judgement call, GNOME Files was the reference.
