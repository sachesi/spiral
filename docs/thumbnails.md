# Thumbnails

Spiral uses the freedesktop thumbnail cache, `~/.cache/thumbnails/large`, so thumbnails
made by other programs are reused and the ones Spiral makes are usable elsewhere. Only the
256 px "large" size is generated. The "Show Thumbnails" preference limits this to local
files, all files, or none.

## How one gets made

A thumbnail is only ever asked for once the cell holding it is on screen and the folder has
finished listing. A folder too big for one batch of the listing is shown only when the
listing is complete, sorted once; one that takes longer than a second is shown as it
arrives, in the order it arrives, and put in order at the end. Waiting for the listing
matters for that second case: a thumbnail asked for before the order lands is for a file
about to move somewhere else. A search is not waited for, since its results arrive for as
long as it runs.

Waiting for the cell matters because the list widgets bind many more cells than they show:
a list keeps two hundred rows and a grid thirty rows of cells, whatever the window shows.
Only the view on screen holds the model, so the other two bind nothing, and the grid is
told how many columns fit so its rows are no wider than the window. The request is dropped
when the row is scrolled away or rebound, and it is carried at low priority, since the
folder appearing matters more than the pictures in it.

Pictures larger than `thumbnail-limit` (50 MB by default) are left with their icon: reading
one costs time and memory out of proportion to a thumbnail. Video and sound are not weighed
that way, because their thumbnailers read a frame rather than the whole file, and a
thumbnail that already exists is shown whatever the size of the file.

A file's thumbnail is looked up in this order: a small in-memory cache, the on-disk cache,
then generation. The on-disk lookup hashes the file's URI, looks for that name under
`large`, `normal` and its own `fail/spiral` directory, and accepts what it finds if the
PNG's `Thumb::MTime` text chunk names the time the file last changed. GIO answers the same
question through its `thumbnail::` attributes, but only by asking it of every file in a
folder as the folder is listed, which is two fifths of the time a folder of fifty thousand
files takes to appear; asking it here means asking it for the rows that are actually
shown. A file that defeats a thumbnailer here has a note left under `fail/spiral`, so the
next run does not try it again. The notes other programs leave, gnome-desktop's under
`fail/gnome-thumbnail-factory`, are not read: they draw with other tools, and a file one
of them gave up on before a codec was installed is not one this one cannot draw; writing the file again makes the note stale, as it carries the time the file
was last changed. No note is left for a thumbnailer that ran out of time or could not be
started: a machine busy with other decoders or a helper not installed yet say nothing about
the file, and it is asked about again on the next visit. For generation, the system `.thumbnailer` entries under
`~/.local/share/thumbnailers` and `XDG_DATA_DIRS` are consulted first, as GNOME does; an
entry matches if its MIME type equals or is a supertype of the file's, and its `TryExec`
has to be in `PATH`. Where several entries claim a type, the user's come before the
system's and within a directory they are taken by file name, so the choice is the same on
every machine; they are tried in that order, and one that fails or hangs on a file hands
it to the next. Images with no system thumbnailer go to the bundled
`spiral-thumbnailer`, a gdk-pixbuf loader that handles whatever loaders are installed
(PNG, JPEG, GIF, BMP, TIFF, and WebP, AVIF, JPEG XL or SVG with their loader packages),
scales to fit, and applies the EXIF orientation. Anything else gets no thumbnail. The
helper is a separate program because it is a decoder: it is sandboxed like any other
thumbnailer, and nothing is ever decoded in the process drawing the window.

The in-memory cache is keyed by URI, modification time and size. The size is part of it
because a file another program is still writing is seen empty first, and the verdict taken
from that snapshot would otherwise stand until the next start whenever the empty file and
the finished one share a modification second.

A thumbnailer runs behind the window in both queues: nice 10 for the processor and the idle
class for the disk. Eight decoders at once would otherwise take the machine over, and the
window they are drawing into is the thing that stops answering.

Thumbnailers run a few at a time, one fewer than the machine has cores and at most eight.
Where several are waiting, the row nearest the top of the folder goes first, so a screenful
fills the way it is read instead of in whatever order the rows happened to be bound; only
rows that were on screen a moment ago are ever waiting, so that is the top of what is being
looked at, and rows left behind by scrolling drop out without doing any work. One that
takes longer than twenty seconds is killed, so a file that hangs a decoder costs one
thumbnail rather than every thumbnail after it. The PDF previewer is bounded the same
way.

Answers are kept in memory until 2048 of them or 64 MB of decoded picture have gathered,
and then the oldest go. Walking through a folder of thousands of pictures therefore costs
a bounded amount of memory, and scrolling back over the last screens still finds them.

Thumbnailers write plain PNGs, but a cached thumbnail only counts as valid if the PNG
carries `Thumb::URI` and `Thumb::MTime` text chunks. A thumbnailer that wrote them itself is
left as it is; one that did not has them written into the file as text chunks, which is a
read and a write rather than a decode and an encode. Otherwise every start would regenerate
everything. Output goes to a
temporary name and is renamed into place.

`spiral-odf.thumbnailer`, installed with the rest, is an ordinary thumbnailer entry that
pulls the preview OpenDocument files embed, using `unzip`.

## Sandbox

Every thumbnailer runs inside bubblewrap with `/usr` read-only, fontconfig directories
visible, the input file bound read-only, a private output directory bound read-write, a
fresh `/tmp`, no network, a cleared environment, and a seccomp filter.
The filter is the one Flatpak and gnome-desktop use: namespace, mount, ptrace, module,
kexec, io_uring and similar system calls fail with EPERM, `clone` with `CLONE_NEWUSER` is
refused, and `clone3` returns ENOSYS so libc falls back to `clone`. The exact list is in
`src/thumbnails.rs`.

A thumbnailer that crashes or misbehaves is confined to that sandbox. `bwrap` is required:
without it no thumbnail is generated at all. Spiral tries the sandbox once at startup and
says what is wrong with it if anything is, so a system with `bwrap` missing or with user
namespaces turned off gives a reason rather than empty icons. There is
no unsandboxed path, because the input is a file the reader did not write.

## Debugging

    G_MESSAGES_DEBUG=spiral spiral ~/Pictures

logs every thumbnail made, with the time it took and the thumbnailer that made it, every
failed run with its stderr, and every file skipped because a note says it failed before.
`gio info -a 'thumbnail::*' FILE` tells you whether GIO considers a cached thumbnail valid;
its `thumbnail::failed` looks at gnome-desktop's notes, which Spiral does not read.
