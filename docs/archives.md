# Archives

Spiral links no archive library. "Extract Here", "Extract To..." and "Compress..." drive
whatever command-line tools are in `PATH` when you use them, and a missing tool only
takes its formats away; the error names the package to install.

## Extracting

| Format | Tools tried, in order |
|---|---|
| zip | 7-Zip, bsdtar, unzip |
| 7z | 7-Zip, bsdtar |
| rar | 7-Zip, unrar, unar, bsdtar |
| tar and compressed tar (gz, bz2, xz, zst, lzma, lz, lz4) | tar, bsdtar (7-Zip for plain tar) |
| bare gz, xz, bz2, zst, lzma | 7-Zip |
| iso, deb, rpm, cab, cpio, lha, arj, xar | 7-Zip, bsdtar |

7-Zip is `7zz`, `7z` or `7za`, whichever is found first. GNU tar needs the matching
`gzip`, `xz` or `zstd` binary.

The tool extracts into a hidden working directory inside the destination. If the archive
had a single top-level item, that item is moved out under its own name; otherwise the
directory is renamed after the archive minus its extension (`photos.tar.gz` becomes
`photos`). Nothing existing is overwritten, a ` (2)` suffix is used instead. Extraction
only targets local folders.

Progress is by the bytes of the archive, with a rate and time left. GNU tar is handed the
archive on its standard input, and how far it has read is where that file stands. 7-Zip
reports a percentage, which is turned into bytes. The tools that say nothing on the way
(bsdtar, unzip, unrar, unar) are measured by what the kernel has them down as having read,
which is not asked of 7-Zip: it reads a block ahead of what it has written, and the bar
would be at the end before the files are there. Where the kernel does not say, as with a
setuid bwrap, such an archive counts once it is done.

An encrypted archive brings up a password prompt: every tool is started with an empty
password so it fails instead of waiting for a terminal, and a failure that mentions a
password or encryption asks for one and runs the tool again. A wrong password asks again;
Cancel stops the job. 7-Zip is given the password on its standard input. The other tools
only take one on their command line, where other processes of the machine can read it
while the tool runs, so 7-Zip, which comes first for zip, 7z and rar anyway, is the one to
have installed for encrypted archives. The password is never written down or logged.

## Creating

The dialog lists the formats whose tools are present: zip (`zip` or 7-Zip), tar.xz,
tar.zst and tar.gz (`tar` plus the compressor), 7z (7-Zip). It opens on the format used
last time. zip and 7z archives can take a password; with one, 7-Zip is preferred because
it encrypts zips with AES-256, while `zip` only offers the old PKWARE scheme, and it reads
the password from its standard input rather than its command line. The tool runs in the
folder of the selected items with their base names, after a `--`, so paths inside the
archive are relative and a name that starts with a dash is still a name. The archive is written to a hidden directory first and moved into
place, again without overwriting.

Progress is by bytes, with a rate and time left: the sources are measured beforehand.
7-Zip reports a percentage of them. tar reports every hundred records it writes
(`--checkpoint`), measured against the headers and padded blocks the sources make. zip
says nothing until an entry is packed, and then only its name, so it is measured by what
the kernel has it down as having read, with the size of each finished entry as a floor
under that. The dots zip can be asked for count what it writes, which says nothing of how
much is left to read.

## Sandbox

Archive tools run in the same bubblewrap sandbox as the thumbnailers (see
[thumbnails.md](thumbnails.md)): `/usr` read-only, no network, no home, a seccomp filter.
The archive or the sources are bound read-only, the working directory is the only writable
place. An archive with hostile paths can at most fill that directory. `bwrap` is required:
without it the operation fails with a message saying so, rather than running the tool
unconfined over a file that came from somewhere else.

Both operations can be stopped, and both are undoable: undo deletes what was extracted or
the archive that was created.
