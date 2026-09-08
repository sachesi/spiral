# Settings

Everything is in the GSettings schema `io.github.sachesi.spiral`. Preferences (Ctrl+,)
exposes the ones you are likely to change; the rest are written by the interface and can
be poked with `gsettings`:

    gsettings set io.github.sachesi.spiral captions "['size', 'date_modified', 'none']"

Changes apply immediately to open windows.

| Key | Values | Default | In Preferences as |
|---|---|---|---|
| `size-units` | `decimal`, `binary` | `decimal` | Size Units |
| `click-policy` | `double`, `single` | `double` | Open Items With |
| `folders-first` | bool | true | Sort Folders Before Files |
| `use-tree-view` | bool | false | Expandable Folders in List View |
| `use-column-view` | bool | false | Column View |
| `show-root` | bool | true | Root (sidebar) |
| `show-favorites` | bool | true | Favorites (sidebar) |
| `recursive-search` | `local`, `always`, `never` | `local` | Search in Subfolders |
| `visible-columns` | list of `size`, `type`, `modified`, `accessed`, `created`, `owner`, `group`, `permissions`, `star` | size, type, modified, star | List view columns (Visible Columns…) |
| `show-create-link` | bool | false | Create Link in context menus |
| `show-delete-permanently` | bool | false | Delete Permanently in context menus |
| `date-format` | `relative`, `full` | `relative` | Date Format |
| `terminal` | executable name or empty | empty | Terminal (see [terminal.md](terminal.md)) |
| `remember-view` | bool | true | Remember View per Folder |
| `guess-view` | bool | true | Grid View for Media Folders |
| `thumbnails` | `local`, `always`, `never` | `local` | Show Thumbnails |
| `item-counts` | `local`, `always`, `never` | `local` | Count Items in Folders |
| `view-mode` | `grid`, `list`, `columns` | `grid` | |
| `chooser-view-mode` | `grid`, `list`, `columns` | `list` | |
| `sort-key` | `name`, `size`, `type`, `modified` | `name` | Order for folders without a remembered one |
| `sort-reversed` | bool | false | Direction, same scope |
| `show-hidden` | bool | false | |
| `compression-format` | `zip`, `tar.xz`, `tar.zst`, `7z`, `tar.gz` | `zip` | |
| `captions` | three of `size`, `date_modified`, `permissions`, `type`, `mime_type`, `owner`, `group`, `none` | all `none` | |
| `grid-zoom` | 48 to 256 | 96 | |
| `list-zoom` | 16 to 64 | 32 | |
| `window-size`, `window-maximized` | | 1000x680, false | |
| `sidebar-visible` | bool | true | |

Notes on a few of them:

`remember-view` on means the grid/list switch and the sort order only affect the folder
you are in; they are kept in the folder's `metadata::spiral-view` and
`metadata::spiral-sort` attributes. Off, the switch changes `view-mode`, and sorting
changes `sort-key` and `sort-reversed`, for everything. Both attributes need gvfs running its
metadata backend; without it there is nowhere to keep them, so the preference is greyed
out and the keys for everything are used instead. `chooser-view-mode` is the same thing for portal file dialogs,
which never remember per folder.

`use-column-view` off, which is how it starts, leaves the view button and the view menu with
the grid and the list alone, and Ctrl+3 does nothing; a folder or a default remembering the
columns from when it was on opens in the list instead.

`compression-format` is whatever Create Archive was last confirmed with; the dialog starts
there next time, provided the tools for it are still installed.

`local` for thumbnails and item counts means files on the local disk only; network
mounts are skipped because reading every file on a share can take a while.

`captions` are the lines under grid icons, top to bottom. `size` becomes an item count on
folders when `item-counts` allows it.

Outside GSettings, Spiral keeps bookmarks in `~/.config/gtk-3.0/bookmarks`, favorites in
`~/.local/share/spiral/starred`, and per-folder view, sort order and custom icon in GIO file metadata.
