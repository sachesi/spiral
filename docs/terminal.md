# Open in Terminal

Right-click the background for the current folder, or a single folder and pick it under
"Open In" for that one. Local folders only.

Terminals are found in `PATH`, like the archive tools. Preferences lists the installed
ones under "Terminal" and says which one "Automatic" would use. The setting stores the
executable name; empty means automatic, which takes the first installed of:

`xdg-terminal-exec`, ghostty, kitty, alacritty, foot, wezterm, ptyxis, kgx (Console),
gnome-terminal, konsole, xfce4-terminal, tilix, terminator, xterm, urxvt.

The same terminal opens an application that asks for one. A desktop entry with
`Terminal=true` — Helix, Vim, Emacs in its terminal build, anything of that kind — cannot be
started by GIO, which will not go looking for a terminal emulator and refuses instead; set
one as the default for `.rs` or `.py` files and opening one used to say only that it could
not be opened. Spiral reads the entry's command line, puts the files where its field code
says they go, and hands the lot to the chosen terminal, started in the file's folder. Both
"Open" and "Open With…" go this way.

`xdg-terminal-exec` is first because it is the freedesktop way of naming a default
terminal. Each terminal gets its own working-directory option (`--working-directory=`,
`--directory=`, `--workdir=`, `wezterm start --cwd`), and the process is also started in
the folder, so the ones without an option still land in the right place. If the chosen
terminal disappears, automatic applies.
