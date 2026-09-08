# Using Spiral

If you have used GNOME Files, most of this is familiar. What follows is the parts worth
knowing and the places where Spiral does something of its own.

## Windows, tabs, sidebar

Ctrl+T opens a tab at the current folder. "Open in New Tab" on selected folders opens
them in the background; middle-clicking a folder in the view or an entry in the sidebar
opens it in a new tab too.
Right-clicking a tab offers moving it left, right or into a window of its own and closing
the other tabs; dragging a tab out of the tab bar also gives it a window. Ctrl+Shift+T
brings back the last closed tab. The last tab leaving closes the window. Window size is
remembered, tabs are not.

The sidebar lists Home, Desktop, Root, Favorites and Trash, then the XDG user folders and the
bookmarks from `~/.config/gtk-3.0/bookmarks` (the same file the GTK file chooser uses), then
every volume and mount the system knows about. Bookmarks can be renamed, removed and
reordered by dragging; drop a folder on the empty area below the list to bookmark it. Root is
the top of the filesystem; it and Favorites can each be turned off in Preferences, under
Sidebar. Unmounted volumes mount when activated. Mounted ones get an eject button, and if a
removable drive has files in its trash you are asked whether to empty it first.

Below roughly 680 px the sidebar folds away behind a button and the navigation controls
move to a bar at the bottom.

## Two panes

F3, or "Split View" in the view menu, splits every tab down the middle: two folders side
by side, dragging between them, copying from one to the other. It stays as you leave it,
for new tabs and windows too. F6 moves between the panes, and so does clicking in one.
The pane in charge is outlined, and the path bar, the header buttons and the keyboard all
act on that one. Drag the handle to give a pane more room. A window too narrow for two
panes shows the left one alone and takes the second one back, at the folder it was
showing, when it grows.

## Getting around

The path bar shows breadcrumbs starting at Home, at the mount point, or at the root of
the filesystem. Crumbs are drop targets. Click the last one, or press Ctrl+L, to type a
location instead: an absolute path, `~/something`, or a URI such as `trash:///`,
`starred:///` or `sftp://host/path`. Folder names complete inline as you type; Tab
accepts, Esc goes back to the crumbs. Hidden folders only complete if you typed the dot.
Right-clicking any crumb offers to open that folder in a new tab or window, bookmark it,
copy its location or show its properties; the ⋮ menu has the same for the current folder.

Alt+Up, Backspace and the arrow beside the history buttons go to the parent folder;
Alt+Left and Alt+Right go through the history.

## Search

Typing with the view focused, or Ctrl+F, starts a search. It looks through the current
folder and, for local folders by default ("Search in Subfolders" in Preferences), every
folder below it; the list view then shows where each result lives. The filter button next
to the entry narrows results by type (folders, documents, images, audio, videos, PDF,
text, spreadsheets), by modification date, and can match file contents instead of or as
well as names. Content matching reads text files up to 10 MB with nothing indexed ahead
of time, so it is only as fast as the disk. Each tab keeps its own filters; "Open Item
Location" on a result jumps to its folder.

## Views

Grid or list, picked from the dropdown in the header or with Ctrl+1 and Ctrl+2; the button
beside it steps through them. "Column View" in Preferences adds the columns as a third,
on Ctrl+3. The dropdown also has zoom, sort
order, hidden files and the sidebar toggle. Zoom steps are 48, 64, 96, 168 and 256 px in
the grid and 16 to 64 in the list and the columns; Ctrl+wheel works too.

Which view a folder opens in: the one it was last switched to, if "Remember View per Folder"
is on (stored in the folder's `metadata::spiral-view` attribute, invisible to other
programs); otherwise the grid if the folder is mostly images and videos and "Grid View for
Media Folders" is on; otherwise the global default. "Mostly" means at least four files, half
or more of them media, looking at the first 2000 entries. The sort order follows the same
preference: with it on, sorting from the menu or a column header applies to that folder and
is kept in `metadata::spiral-sort`; with it off, it changes the default for every folder.
"Sort Order" in Preferences sets that default directly. Both attributes are gvfs's doing:
where its metadata backend is not running there is nowhere to keep them, so the preference is
greyed out and the switch changes the global default instead.

Files and folders you cannot read or change carry a small lock, and the actions they do
not allow (cut, rename, trash, delete) are greyed out. Folders you are looking at update
themselves when something else creates, deletes or rewrites a file in them.

With "Expandable Folders in List View" on, every folder in the list gets an arrow that
unfolds its contents in place, as deep as you like; the grid never shows children.

The grid can show up to three lines under each name: size or item count, date, permissions,
type, MIME type, owner, group; "Captions…" in the view menu picks them. In the list, "Visible
Columns…" chooses among size, type, modified, accessed, created, owner, group, permissions
and a star column that toggles favourites; the name column always stays, and only name, size,
type and modified sort. The name column takes whatever the others leave, so as a window
narrows the columns are dropped from the right, one after another, rather than the name being
squeezed into an ellipsis; they come back as it widens. A name too long for the column it has
is cut in the middle and shown whole on hover. Type shows the extension — "txt", "tar.gz",
"Folder" — and falls back to the full description, on a tooltip, for names without one. Every
column but the name and the star can be resized by dragging its header edge.

### Columns

The column view is off until "Column View" in Preferences turns it on; with it off the
views are the grid and the list, and a folder that remembers the columns opens in the list.

It draws the path as a strip of lists, one folder per column, the way the
Finder does. The last column is the folder you are in; the columns before it are the
folders that lead there, with the one you came through picked out, and a column appears
past the last one whenever a single folder is selected, showing what it holds. The strip
scrolls sideways and keeps the last column in sight; Shift and the wheel move it by hand,
and a drag held near either end pushes it along, so a column that is off the screen can
still be dropped on.

One click in any column before the last goes there: a folder becomes the folder you are
in, a file makes its folder the one you are in and picks the file out. Left and Right step
out to the parent and into the selected folder. Everything else — the context menus,
renaming, dragging files out, the keys — works in the last column, which is the folder the
header and the file actions act on; dropping files on any other column puts them in the
folder that column lists.

Unlike the grid and the list, the columns stay put as you walk from folder to folder,
since walking is what they are for; a folder that remembers a view of its own is the
exception and takes the strip apart. Leave them with the view button or Ctrl+1 and Ctrl+2.
A search reaches past the folder you are in, so it falls back to the list while it runs.

## Selecting, opening, moving things

Double click opens by default; Preferences has a single-click option. Files open in
their default application; "Open With..." lists the alternatives and can change the
default. Rubber-band selection works in both views. The pill in the corner shows what is
selected and how big it is. A click on empty space clears the selection and gives the
files the keyboard, so Ctrl+A and the rest of the file keys work right after it.

Dropping files asks what to do with them: copy, move or link. It always asks, because
nothing in a drop tells a held modifier apart from a plain drag: under Wayland the
compositor settles the question before the files ever reach Spiral. "Ask What to Do With
Dropped Files" in Preferences turns the question off, and then dragging inside Spiral
moves, Ctrl makes it a copy, Ctrl+Shift makes a link, and drags from other applications
copy.
A drag starts anywhere on a row or a grid tile, and a folder accepts a drop anywhere on
its row. Drop targets are folders in the view, breadcrumbs and sidebar entries except
Trash and Favorites. Cut, copy and paste use the same clipboard format as GNOME Files, so
the two interoperate; files waiting on the clipboard as a cut are shown faded until they
are pasted. An image on the clipboard with no files behind it — a screenshot, say —
pastes into the folder as "Pasted Image.png". What a paste leaves in the folder is selected
once it lands, ready for whatever is done to it next. "Paste Into Folder" pastes into a
selected folder without entering it.

"Copy to…" and "Move to…" ask for a folder instead of using the clipboard. "Paste as
Link" and, when turned on in Preferences, "Create Link" make symbolic links; a link
beside its target is called "Link to name".

Delete moves to trash, Shift+Delete deletes for good after asking; the menu entry for it
is off by default and lives in Preferences under Optional Context Menu Actions. Locations
that cannot trash (some shares) offer permanent deletion instead. Inside Trash the menu
offers "Restore From Trash" and "Delete From Trash". Everything else about operations,
conflicts and undo is in [file-operations.md](file-operations.md).

Executable files get "Run as a Program": scripts run in the terminal so their output can
be read, binaries start on their own. In Favorites, "Open Item Location" jumps to the
folder holding the item and selects it there.

## Preview

Space shows the selected file without opening an application, and closes the preview
again; Escape closes it too. Images, video, sound and text files are drawn by Spiral
itself, and PDFs a page at a time. Source code is coloured by GtkSourceView, which knows
the language from the name and the type of the file; the page itself keeps the colours of
the theme, only the words are the scheme's. Lines are not wrapped, since source is read the
way it was written, and long ones scroll sideways. The Left and Right arrows walk
through the folder without leaving the preview, Page Up and Page Down turn the pages of a
PDF, and Return hands the file to its application. The preview has no close button: it is
a preview, and the keys that opened it close it again.

The preview takes the proportions of what it holds, and takes them before it opens, so it
does not resize itself in front of you. It is as large as the window it opens over allows:
a picture keeps its own, read from the file header and turned the way its EXIF tag says; a
PDF opens at the size of its page, upright when the file keeps that size to itself, and a
video at the proportions of its stream once it reports them; text gets a page to read on
and the sound player is no larger than its controls. Ctrl with + and -, Ctrl and the
wheel, which zooms around the pointer, or the buttons in the bar over the picture zoom a
picture or a page; Ctrl+0 fits it back into the window, zooming out stops there rather
than counting below it, and a zoomed picture is moved by dragging it. Nothing in the
preview shows scrollbars.

A sound file is shown with the cover it carries, cut square and drawn at the size of the
icon it stands in for, and with that icon until there is one. Anything else falls back to
the file's thumbnail, or to its icon, with the name and the type left to the header. Video
and sound play through one GStreamer pipeline that Spiral keeps for the whole session and
points at one file after another; a format with no plugin installed for it shows its icon
instead. PDFs
need `pdftoppm` from poppler-utils, which runs in the same bubblewrap sandbox as the
thumbnailers and, like them, is not run at all where bubblewrap is missing. Text files are shown up to 256 kB, images up to 128 MB and 80 megapixels. A
file is only loaded once it has been selected for a moment, so holding an arrow down runs
through a folder without starting a decoder per file.

## Favorites

"Add to Favorites" stars files and folders. They show up under Favorites in the sidebar,
which is the `starred:///` location; it lists every starred item including hidden ones
and quietly drops entries that no longer exist. The list is a plain file,
`~/.local/share/spiral/starred`, one URI per line.

## Properties

Alt+Return. Type, size (folders are summed in the background), location, times, default
application. A single folder can be given a custom icon; it is stored as
`metadata::custom-icon`, which GNOME Files reads as well. For a single local file there
is a Permissions page with owner, group and others as dropdowns and an Executable switch;
it is read-only unless you own the file.

## Command line

    spiral                    # a new window at the home folder
    spiral ~/Music /tmp       # a new window with a tab per path
    spiral -w ~/Music /tmp    # a window per path
    spiral -s ~/notes.txt     # the parent folder, with the file selected
    spiral trash:///          # URIs work too
    spiral -q                 # close every window and quit
    spiral --version

Every call opens new windows in the running instance, as with GNOME Files. `-q` stops
running file operations first, cleaning up partial files the same way the stop button
does.
