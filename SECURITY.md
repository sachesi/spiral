# Security

Please report a vulnerability privately rather than in a public issue: through
[a private advisory](https://github.com/sachesi/spiral/security/advisories/new) on GitHub,
or by mail to sachesi <xsachesi@pm.me>. Say what you found, how to reproduce it and which
version you ran; a fix is worked out with you before anything is published.

Only the latest release gets fixes.

## What counts

Spiral reads files that anyone may have written, and it answers other programs over D-Bus.
The parts where a mistake matters most:

- The sandbox around everything that parses a file's contents: thumbnailers, the PDF
  previewer, the preview's decoders of video and sound, and of pictures where glycin is
  missing, the reader of photo and media details, and the archive tools. They run under
  bubblewrap with a seccomp filter (see [docs/thumbnails.md](docs/thumbnails.md)); a way
  out of it, a way to reach files it does not bind, a way for what a helper hands back or
  leaves behind to make Spiral read or write elsewhere, or a helper that runs without it
  is a vulnerability. - The file chooser portal backend and the
  `org.freedesktop.FileManager1` service, which act on requests from other applications
  (see [docs/integration.md](docs/integration.md)). - File operations that could be led to
  write, delete or change permissions somewhere other than where the user pointed, through
  links, races or names.

The media decoder alone is given the GPU's render nodes, and NVIDIA's device files, to
decode video on the GPU (not for a file whose name says it holds sound alone), with
read-only sysfs for the drivers to tell which GPU they have: what a Flatpak application
gets with `--device=dri`. A render node shows nothing on screen and reaches no other
program's work on the GPU, but the kernel's GPU driver is within its reach. The pictures
it decodes there reach Spiral as DMA-BUF descriptors, whose layout Spiral checks against
what each descriptor holds before GTK imports them.

A thumbnail that comes out wrong or a helper that crashes inside its sandbox is a bug;
please file it as an ordinary issue.

Pictures are decoded by glycin where it is installed and its sandbox starts, and by
`spiral-thumbnailer` in Spiral's sandbox otherwise; thumbnails in the cache and pictures
set as folder icons are decoded the same way. Glycin always gets its bubblewrap sandbox:
Spiral does not let it fall back to none. A flaw in glycin's sandbox or its loaders
belongs with glycin. Glycin converts a picture that carries an ICC colour profile to sRGB
in the process that asked for it, so such a profile is read by lcms2 inside Spiral.