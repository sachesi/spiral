# Open in Terminal

Right-click the background for the current folder, or a single folder for that one.
Local folders only.

Terminals are found in `PATH`, like the archive tools. Preferences lists the installed
ones under "Terminal" and says which one "Automatic" would use. The setting stores the
executable name; empty means automatic, which takes the first installed of:

`xdg-terminal-exec`, ghostty, kitty, alacritty, foot, wezterm, ptyxis, kgx (Console),
gnome-terminal, konsole, xfce4-terminal, tilix, terminator, xterm, urxvt.

`xdg-terminal-exec` is first because it is the freedesktop way of naming a default
terminal. Each terminal gets its own working-directory option (`--working-directory=`,
`--directory=`, `--workdir=`, `wezterm start --cwd`), and the process is also started in
the folder, so the ones without an option still land in the right place. If the chosen
terminal disappears, automatic applies.
