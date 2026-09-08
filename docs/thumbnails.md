# Thumbnails

Spiral uses the freedesktop thumbnail cache, `~/.cache/thumbnails/large`, so thumbnails
made by other programs are reused and the ones Spiral makes are usable elsewhere. Only the
256 px "large" size is generated. The "Show Thumbnails" preference limits this to local
files, all files, or none.

## How one gets made

A file's thumbnail is looked up in this order: a small in-memory cache, the on-disk cache
as GIO sees it, then generation. For generation, the system `.thumbnailer` entries under
`~/.local/share/thumbnailers` and `XDG_DATA_DIRS` are consulted first, as GNOME does; an
entry matches if its MIME type equals or is a supertype of the file's, and its `TryExec`
has to be in `PATH`. Images with no system thumbnailer go to the bundled
`spiral-thumbnailer`, a gdk-pixbuf loader that handles whatever loaders are installed
(PNG, JPEG, GIF, BMP, TIFF, and WebP, AVIF, JPEG XL or SVG with their loader packages),
scales to fit, and applies the EXIF orientation. Anything else gets no thumbnail. The
helper is a separate program because it is a decoder: it is sandboxed like any other
thumbnailer, and nothing is ever decoded in the process drawing the window.

The in-memory cache is keyed by URI, modification time and size. The size is part of it
because a file another program is still writing is seen empty first, and the verdict taken
from that snapshot would otherwise stand until the next start whenever the empty file and
the finished one share a modification second.

At most four thumbnailers run at once, newest requests first, so the rows on screen win
over the ones you scrolled past.

Thumbnailers write plain PNGs, but GIO only accepts a cached thumbnail as valid if the PNG
carries `Thumb::URI` and `Thumb::MTime` text chunks. Spiral re-saves every generated
thumbnail with those, otherwise every start would regenerate everything. Output goes to a
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
without it no thumbnail is generated at all, and Spiral says so once at startup. There is
no unsandboxed path, because the input is a file the reader did not write.

## Debugging

    G_MESSAGES_DEBUG=spiral spiral ~/Pictures

logs failed thumbnailer runs with their stderr. `gio info -a 'thumbnail::*' FILE` tells you
whether GIO considers a cached thumbnail valid.
