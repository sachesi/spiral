# File operations

Copy, move, trash, delete, rename, new folder and document, extract and compress all run
as jobs on the main loop with async GIO calls. The button at the bottom of the sidebar
shows one pie per running job and opens a list with a progress bar, a detail line and a
stop button for each. Detail lines look like "12.3 MB of 210.0 MB, 4 seconds left
(52.1 MB/s)" once a job has run for a second; finished jobs linger for three seconds.

Copies, moves and deletes count their sources first so the total is known before the
rate clock starts. Progress inside a single file comes from GIO's copy callback.

## Copy and move

Symlinks are copied as symlinks and metadata (permissions, times, xattrs as far as GIO
carries them) is preserved. A move tries a rename first; across filesystems GIO refuses to
move a directory in one go, and the job copies the tree and deletes sources one by one as
they arrive, so an interrupted move never leaves half a tree deleted.

Copying a folder into itself is refused. Moving something into the folder it is already
in does nothing. Copying into the same folder produces `name (copy).ext`, then
`name (copy 2).ext`.

If the destination exists you get a dialog with Replace (Merge for folders), Skip, Rename
and Cancel, with both sides shown with size and date. "Apply this action to all files
and folders" remembers Replace or Skip for the rest of the job. Read and write errors ask
Cancel, Retry or Skip.

## Trash

Trashing goes through GIO, so files land in the freedesktop trash of their own
filesystem and appear in `trash:///`. Where trashing is not possible the job offers to
delete permanently instead. Permanent deletion always asks, is recursive, and cannot be
undone. "Empty Trash" is in the folder menu inside Trash.

## Names

Rename (F2) selects the part before the extension. Names may not be empty, contain `/`,
be `.` or `..`, or already exist; a leading dot gets a warning. New documents are called
"Untitled Document".

## Undo

Ctrl+Z, or the Undo button on the toast. One level. Undoing a copy, extraction, archive
or new folder deletes what was created; a move moves back; a trash restores from
`trash:///` to the original place, recreating a missing parent folder and picking the
most recently trashed item if several match; a rename renames back. Permanent deletion
is the one thing that cannot be undone. Redo exists for moves, renames and restores.

Undo acts on what actually happened, not on what was asked: items skipped in the conflict
dialog or left behind by a cancel are not touched.

## Cancel and close

Stop aborts at the next await; a file being written at that moment is removed. Archive
tools are killed and their working directory deleted.

Running jobs keep the application alive, so closing every window while a copy is going
does not stop it. Dialogs a job needs open a fresh window if none is left, and starting
`spiral` again brings the operations list up.
