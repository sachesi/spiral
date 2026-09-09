# Portal and D-Bus integration

## File chooser portal

`xdg-desktop-portal-spiral` implements `org.freedesktop.impl.portal.FileChooser`. Once
`portals.conf` prefers it (`just setup-portal`, see [installing.md](installing.md)),
applications that go through the portal for file dialogs, which is GTK 4 apps under a
portal, Flatpaks, Firefox, Chromium and Electron apps, Qt with the xdg platform theme,
get Spiral's dialog.

The dialog is a trimmed file manager window: sidebar, path bar, the same views, and a
bottom bar with the filter dropdown, the name entry in save mode, a button for the
application's extra options, and the accept button. Trash, delete, cut and paste, rename
and drag and drop are off. New Folder is there in save mode and when a folder is being
asked for. Opening a file accepts; pressing Open with a folder selected enters it; Esc
cancels.

The dialog remembers how it was left, in keys of its own: `chooser-view-mode` (list by
default) for the view and `chooser-sort-key` with `chooser-sort-reversed` for the order,
which the header's sort button sets. Nothing it is given changes the file manager's own
view or order, and folders are not remembered one by one as they are there.

The search button, or Ctrl+F, or simply typing, searches the folder on screen and only
that one: a dialog is being asked for a file in a folder, not for a walk of the disk, so
the "Search in Subfolders" preference does not apply and results carry no location column.
Leaving the folder ends the search.

Honoured request options: title, accept label, modal, multiple, directory, filters and
current filter (glob and MIME), choices (combos become dropdowns, booleans check boxes),
current name, current file and current folder for saving, and the file list for
SaveFiles. Saving over an existing file asks first. The reply carries the URIs, the
selected filter and the choice values.

The dialog is made transient for the caller when the request has a Wayland handle, via
xdg-foreign. Without that the compositor sees an independent window.

### Why the backend starts the way it does

GTK 4.22 on Wayland gets theme, icon theme and fonts only from the
`org.freedesktop.portal.Settings` interface of xdg-desktop-portal, with built-in defaults
as the fallback. xdg-desktop-portal activates a backend and then blocks until the backend
owns its bus name. If the backend initialised GTK first, GTK would ask the portal for
settings while the portal is waiting for the backend, and both would hang. So the backend
takes `org.freedesktop.impl.portal.desktop.spiral` on a separate thread and only starts
GTK once the name is owned. Keep that order if you touch
`src/bin/xdg-desktop-portal-spiral.rs`.

### Checking

    busctl --user list | grep portal.desktop.spiral

should show the name after any application has opened a file dialog, and
`journalctl --user -u xdg-desktop-portal -b` shows which backend was picked for
FileChooser. You can call the backend directly, bypassing xdg-desktop-portal:

    gdbus call --session --dest org.freedesktop.impl.portal.desktop.spiral \
        --object-path /org/freedesktop/portal/desktop \
        --method org.freedesktop.impl.portal.FileChooser.OpenFile \
        /org/freedesktop/portal/desktop/request/t/1 '' '' 'Pick a file' '{}'

Adwaita colours instead of your theme mean the Settings portal is not answering; check
that something serves `org.freedesktop.impl.portal.Settings` in your portals.conf. No
dialog at all usually means `WAYLAND_DISPLAY` is missing from the D-Bus activation
environment, or the portal file is not where xdg-desktop-portal looks. A stale backend
after an update: `pkill -f xdg-desktop-portal-spiral`.

## org.freedesktop.FileManager1

Spiral owns `org.freedesktop.FileManager1` while running and is D-Bus activatable for it
(`spiral --gapplication-service`), so browsers and chat clients can "Show in folder".
`ShowFolders` opens a window with a tab per folder, `ShowItems` a window per parent folder
with the files selected, `ShowItemProperties` the Properties dialog. The startup id is
ignored.

The interface is served from a thread with a main context of its own, and the application
registers on the bus before it starts GTK. Both matter: the caller is usually
xdg-desktop-portal answering a browser's `OpenURI.OpenDirectory`, and it waits for our
reply, while GTK 4.22 asks that same portal for its settings as it starts. Serving the
interface from the main thread, or initialising GTK before registering, deadlocks the two
until D-Bus gives up on both, some twenty-five seconds later; the browser then gives up
too and opens the folder itself, without the file selected. So `spiral` calls
`init_early()` and leaves the stylesheet to `startup()`, and the method handler replies
first and opens the window from the main loop afterwards.

    gdbus call --session --dest org.freedesktop.FileManager1 \
        --object-path /org/freedesktop/FileManager1 \
        --method org.freedesktop.FileManager1.ShowItems "['file:///etc/hosts']" ""

The application name on the bus is `io.github.sachesi.spiral`; a second `spiral` forwards
its command line to the first, which opens the windows (or quits, for `-q`).
