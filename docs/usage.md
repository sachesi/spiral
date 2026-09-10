# Using Spiral

If you have used GNOME Files, most of this is familiar. What follows is the parts worth
knowing and the places where Spiral does something of its own.

## Windows, tabs, sidebar

Ctrl+T opens a tab at the current folder. A folder's own menu opens it another way: "Open
in Terminal" is there wherever a terminal emulator is installed, and "Open in New Tab",
which opens the selected folders in the background, and "Open in New Window", which gives
each of them a window of its own, are off to begin with and turned on under Menus in
Preferences. Middle-clicking a folder in the view or an entry in the sidebar opens it in a
new tab whatever the menu carries. Alt and a digit goes to that tab, wherever the keyboard
is in the window.
Right-clicking a tab offers moving it left, right or into a window of its own and closing
the other tabs; dragging a tab out of the tab bar also gives it a window. Ctrl+Shift+T
brings back the last closed tab. The last tab leaving closes the window. Window size is
remembered, tabs are not.

The sidebar lists Home, Desktop, Root, Favorites and Trash, then the XDG user folders and the
bookmarks from `~/.config/gtk-3.0/bookmarks` (the same file the GTK file chooser uses), then
every volume and mount the system knows about, then the network (see
[Network locations](#network-locations)), then the tags if they are turned on (see
[Tags](#tags)). Bookmarks can be renamed, removed and
reordered by dragging; drop a folder on the empty area below the list to bookmark it, and
files on Trash to trash them. The Trash entry's own menu empties it. Root is
the top of the filesystem; it and Favorites can each be turned off in Preferences, under
Sidebar. Unmounted volumes mount when activated. Mounted ones get an eject button, and if a
removable drive has files in its trash you are asked whether to empty it first. A mounted
device met in a folder of its own — under `/run/media`, say — offers "Unmount", or "Eject"
where the drive takes its medium back, in the context menu, and asks about its trash the
same way.

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

Alt+Up and the arrow beside the history buttons go to the parent folder, and so does
Backspace, except in a folder opened from search results, where it goes back to the search;
Alt+Left and Alt+Right go through the history. Escape stops a folder or a search that is
still coming in and keeps what has arrived; with nothing loading it does nothing. A folder
stopped that way is a list of what had been read, and stops following what happens in the
folder until F5 reads it again.

## Network locations

Shares on other machines are reached through gvfs, so what Spiral can open is whatever
gvfs has a backend for: SMB, SFTP and SSH, FTP and FTPS, NFS, WebDAV and the rest. Without
gvfs installed there are no network locations at all, and Spiral says so instead of
offering them. "Network Locations" in Preferences, under General, turns the whole of it off
for those who have no servers to reach: the menu entry goes, the sidebar loses its Network
section, and an address goes nowhere by itself.

"Connect to Server…" in the main menu asks for an address: a scheme, `://` and the host,
as in `smb://server/share`, `sftp://user@host` or `davs://host/dav`. The dialog names the
schemes this system can actually use, so an address it will not take is one there is no
backend for. A share over HTTPS is WebDAV, which is `davs://`. Passwords, anonymous logins
and whether to remember either are asked for by the system's own dialog. The servers
connected to are offered again the next time the dialog is opened, most recent first, and
can be taken off that list one by one.

A share that answers opens in the tab in front and joins the sidebar under Network, where
its button disconnects it again; the context menu of the row calls it "Disconnect" rather
than "Eject". "Network" itself, above them, is where the machines around are listed, for
finding a server whose address you do not know; it stays empty unless something on the
network announces itself and gvfs has the backend that hears it, `wsdd` for Windows shares
or `dns-sd` for the rest. A machine opened from there keeps the name it announced itself
under, since what gvfs reaches it by is often a bare address. Shares Windows keeps out of
sight, the ones whose name ends in `$`, are hidden files here and show with them.
Everything else works as it does on a disk: bookmarks, drag and drop, copying, renaming,
search. Reading a folder over a share is slower than reading one on a disk, which is why
thumbnails, item counts and searching subfolders are limited to local files unless you say
otherwise (see [Settings](settings.md)).

Going to a share that is not connected connects it: a bookmark, an address typed into the
path bar, or a folder on it opened from somewhere else all ask for the password and then
open. Turning that question down leaves the folder unopened; going there again asks again.

## Search

Typing with the view focused, or Ctrl+F, starts a search. It looks through the current
folder and, for local folders by default ("Search in Subfolders" in Preferences), every
folder below it; the list view then shows where each result lives. The filter button next
to the entry narrows results by type (folders, documents, images, audio, videos, PDF,
text, spreadsheets), by modification date, and can match file contents instead of or as
well as names. Content matching reads text files up to 10 MB with nothing indexed ahead of
time, so it is only as fast as the disk. Each tab keeps its own filters; "Open Item
Location" on a result jumps to its folder, and "Open Item Location in New Tab" opens the
folder in a tab behind the results, the item selected in both cases. A folder left while
it showed a search gets the search back, words and filters, when Back or Forward returns
to it, so Back from a result's folder, or Backspace there, is back at the results. The
last search left in a tab comes back with what it had found, in the same order and without
starting over, less anything that has gone since, and one left before it finished carries
on from there; F5 searches again. What was selected when a folder or a search was left is
selected again on the way back.

## Views

Grid or list, picked from the dropdown in the header or with Ctrl+1 and Ctrl+2; the button
beside it steps through them. "Column View" in Preferences adds the columns as a third,
on Ctrl+3. The dropdown also has zoom, sort
order, hidden files and the sidebar toggle. Zoom steps are 48, 64, 96, 168 and 256 px in
the grid and 16 to 64 in the list and the columns; Ctrl+wheel works too, and Ctrl+0 puts
the view back to the size the preference starts at.

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

A folder opened takes the keyboard with it, so the keys that act on files work as soon as
it is on screen, without a click first.

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
default. An application whose desktop entry asks for a terminal is given the one from
Preferences (see [terminal.md](terminal.md)), so a terminal editor works as a default. Rubber-band selection works in both views. The pill in the corner shows what is
selected and how big it is. A click on empty space clears the selection and gives the
files the keyboard, so Ctrl+A and the rest of the file keys work right after it. Ctrl+S
asks for a pattern — `*.png`, `file??.txt` — and selects the names in the folder that match
it; Ctrl+Shift+I selects everything the selection leaves out.

Dropping files asks what to do with them: copy, move or link. It always asks, because
nothing in a drop tells a held modifier apart from a plain drag: under Wayland the
compositor settles the question before the files ever reach Spiral. "Ask What to Do With
Dropped Files" in Preferences turns the question off, and then dragging inside Spiral
moves, Ctrl makes it a copy, Ctrl+Shift makes a link, and drags from other applications
copy.
A drag starts anywhere on a row or a grid tile, and a folder accepts a drop anywhere on
its row. Drop targets are folders in the view, breadcrumbs and sidebar entries except
Favorites; Trash takes a drop as well, and trashes what lands on it. A drag held for a
moment over a folder, a breadcrumb or a sidebar entry opens it, so files can be carried
into a folder that is nowhere on screen when the drag starts. Cut, copy and paste use the same clipboard format as GNOME Files, so
the two interoperate; files waiting on the clipboard as a cut are shown faded until they
are pasted. An image on the clipboard with no files behind it — a screenshot, say —
pastes into the folder as "Pasted Image.png". What a paste leaves in the folder is selected
once it lands, ready for whatever is done to it next. "Paste Into Folder" pastes into a
selected folder without entering it.

"New Folder…" and "New Document" both ask for a name before they make anything, and what
they make is selected once it is there, ready to be opened or renamed. What "New Document"
offers is what is in the XDG templates folder, `~/Templates` as a rule: a document per
file, a submenu per folder, and "Empty Document" at the end, which is all it offers where
there are no templates. The name starts as the template's own, with everything but the
extension selected, so typing replaces the name and Return alone takes it as it is; a
document started from a template is a copy of it.

"Copy to…" and "Move to…", which the Menus page of Preferences turns on, ask for a folder
instead of using the clipboard, in a picker attached to the window: the sidebar, the folders of the location and nothing else, with
New Folder in the header for a destination that does not exist yet. Ctrl+L or a click on
the current folder types a location, Ctrl+R reads it again, and the button takes the
folder selected, or the one on screen when nothing is. In a window too narrow for both
panes the sidebar folds away, and the button at the left of the header brings it back
over the folders. "Extract To…" asks the same way.
"Paste as Link" and, when turned on in Preferences, "Create Link" make symbolic links; a
link beside its target is called "Link to name".

F2 renames. With one file selected it is a popover over the file, with the name selected
up to its extension; with several, it is a dialog that renames them all by one rule —
a shared name with numbers after it, or some text of the old names replaced by other text.
The names it would give are listed as the rule is typed, and it refuses to rename while
two of them would collide or take a name the folder already has. One undo puts them all
back.

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
the theme, only the words are the scheme's. Lines are numbered down the side. Lines are not wrapped, since source is read the
way it was written, and long ones scroll sideways. The Left and Right arrows walk
through the folder without leaving the preview, Page Up and Page Down turn the pages of a
PDF, and Return hands the file to its application. The preview has no close button: it is
a preview, and the keys that opened it close it again.

The preview takes the proportions of what it holds, and takes them before it opens, so it
does not resize itself in front of you. It is as large as the window it opens over allows:
a picture keeps its own, read from the file header and turned the way its EXIF tag says; a
PDF opens at the size of its page, upright when the file keeps that size to itself, and a
video at the proportions its thumbnail or the header of its container gives, and only a file with neither opens in the shape most video has and moves once the stream reports; text gets a page to read on
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
and quietly drops entries that no longer exist. An item trashed or deleted here loses its
star at once, along with everything starred inside a folder, and does not get it back
when restored. The list is a plain file, `~/.local/share/spiral/starred`, one URI per
line.

## Tags

Off by default; "Colour Tags" in Preferences turns them on. Seven tags named after their
colours come with it. There are never more than seven: remove one, and "New Tag…" in the
menu of a tag in the sidebar makes another, with a name and one of the seven colours or a
colour of your own from the colour dialog. A tagged file is washed with the colour of its
first tag: across its row in the list and column views, behind its name in the grid.

The context menu of a selection shows the tags as a row of dots: a click puts the tag on
every selected file, or takes it off when they all have it; a dot with a tick is on all of
them, a faded tick on some. Dropping files on a tag in the sidebar gives them that tag.

Each tag in the sidebar opens the list of what carries it; the "Tags" crumb above it lists
every tagged file. A tag's own menu there renames it, changes its colour, removes it, or
makes a new one. Renaming and removing rewrite every file known to carry the tag, so
removing asks first. Removing every tag brings the seven colours back. Dragging a tag up or
down the sidebar reorders the tags, there and in the context menu.

A file's tags are the `user.xdg.tags` extended attribute on the file itself, the one Dolphin
and Baloo use, so they survive a rename, a move or a copy, and other programs see them and
Spiral sees theirs. That also means they live only where the filesystem keeps extended
attributes: files on a network share or a FAT-formatted stick cannot be tagged. Since no
program can ask every file on the disk, Spiral keeps an index of which files carry which
tag, `~/.local/share/spiral/tags`, one `tag<TAB>uri` line per pair; a file tagged elsewhere
or moved by another program joins it when a listing shows it, provided the tag is one on
offer, and an entry that has stopped being true is dropped when the tag is listed. A file
trashed or deleted here leaves the index, and its tag's list, at once. Restored from the
trash it comes back, and so do the tagged files inside a folder trashed since Spiral
started; those in one trashed earlier come back when a listing shows them. The tags
themselves, name and colour each, are the `tags` setting.

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
