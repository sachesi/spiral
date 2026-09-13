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
  previewer, the reader of photo and media details, and the archive tools. They run under
  bubblewrap with a seccomp filter (see [docs/thumbnails.md](docs/thumbnails.md)); a way out
  of it, a way to reach files it does not bind, or a helper that runs without it is a
  vulnerability.
- The file chooser portal backend and the `org.freedesktop.FileManager1` service, which act
  on requests from other applications (see [docs/integration.md](docs/integration.md)).
- File operations that could be led to write, delete or change permissions somewhere other
  than where the user pointed, through links, races or names.

A thumbnail that comes out wrong or a helper that crashes inside its sandbox is a bug;
please file it as an ordinary issue.
