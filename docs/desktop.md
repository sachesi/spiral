# Desktop icons

`spiral-desktop` shows the files of the desktop folder (`XDG_DESKTOP_DIR`, `~/Desktop`
unless it is set otherwise, made when it is missing) as icons on the wallpaper, with the
home folder, the trash and mounted drives beside them. It is the same view as the file
manager's grid, spread over the screen, so selecting, renaming, dragging, the context
menus and the keys all work as they do in a Spiral window.

## Running it

The session starts `spiral-desktop` and keeps it running; it opens no window of its own
and exits only when the session ends. Only one runs at a time: a second start leaves the
first in place. It needs a Wayland compositor with `wlr-layer-shell`, on which it takes
the bottom layer of the display the compositor gives it, under the windows and inside
the room panels and docks keep for themselves. When that display goes away it comes back
on the one the compositor picks next.

## Using it

Items open as they do in Spiral, with one click or two as `click-policy` says; a folder
opens in a Spiral window. Dragging icons moves them, and dropping files from another
window moves or copies them into the desktop folder, as dropping onto a folder does. A
drag over the empty desktop selects the icons it touches. Ctrl or Shift and a click add an
icon to the selection or take it out; the icons stand where they were put, in no order a
range could follow. Home, the trash and the drives are selected, dragged and opened with
the files, and Select All takes them too; they only ever move within the desktop. A right
click on something selected keeps the selection: with files in it the menu is theirs,
and its Open opens the places as well; a place alone has its own menu. The arrow keys move
between icons by where they stand on the screen, Shift with them adds to the selection,
Enter opens, F2 renames, Escape clears the selection, and typing a name jumps to it. The
keyboard goes to the desktop once it is clicked.

Space previews the selected file, as in a Spiral window. Over the desktop the preview is a
window of its own, one that can be made larger by hand; where it opens, and whether it
keeps its middle as it is resized, is up to the compositor.

The menu of the empty desktop has New Folder and the rest of Spiral's new items, Paste,
Select All, Arrange Icons, Sort By, Open in Terminal when a terminal is found, and Desktop
Settings…, which opens the Desktop page of Spiral's Preferences. Unless the icons are
lined up in the sort order already, Sort Now lines them up once, and they stay where that
puts them. Home, the trash and the drives have menus of their own: Open, Empty Trash, and
Unmount or Eject. They are not files of the folder, so nothing done to the files reaches
them.

## Arranging

`desktop-arrangement` picks one of three:

- `grid`: each icon stays in the cell of the grid it was dropped in. A new file takes
  the first free cell, down the first column, then the next.
- `auto`: the icons are lined up in the order of `desktop-sort-key` and
  `desktop-sort-reversed`, and dragging one changes nothing.
- `free`: each icon stays where it was dropped.

The first column is at the right edge, or at the left with `desktop-alignment` set to
`left`; changing it moves every icon over to the other side, as far from it as it was
from this one. `desktop-icon-size` and `desktop-icon-spacing` set the least size of the
cells, as do the captions under the names when `desktop-show-captions` is on: the same
captions as the file manager's grid, chosen with `captions`. Home, the trash and the drives
have them too, with what the trash holds and the room left on a drive for the size. The
cells that fit share out the room left over, so the last column and row are as far from
their edges as the first. A name takes two lines at most, shortened in the middle past
that.

Icons glide to a new place when the arrangement, the sort, the alignment or the size of
the cells moves them, and into their cell when dropped; with reduced motion they are put
there at once.

Where the icons were left is kept in `$XDG_STATE_HOME/spiral/desktop-positions`
(`~/.local/state/spiral/desktop-positions`), measured from the side they start at, for each
size of desktop on its own. A display
of a size not seen before starts from the layout of the nearest size kept, scaled to fit.
Removing the file puts every icon back in order.

## Menu entries of other programs

Programs add entries to the menu of the empty desktop with desktop entry files in
`spiral/desktop-menu/` under the data directories: `~/.local/share/spiral/desktop-menu/`,
then each directory of `XDG_DATA_DIRS`, such as `/usr/share/spiral/desktop-menu/`. A file
hides one of the same name further down that list, so a user can replace or hide an entry
of the system's. Entries are shown in the order of their file names, below the ones of
Spiral, with their `Name`, and choosing one runs its `Exec`.

    [Desktop Entry]
    Type=Application
    Name=Change Background…
    Exec=example-settings background
    TryExec=example-settings

An entry is left out when the program in its `TryExec` is not installed, when it has
`Hidden=true` or `NoDisplay=true`, or when `OnlyShowIn` and `NotShowIn` keep it from the
desktops named in `XDG_CURRENT_DESKTOP`.
