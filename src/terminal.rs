//! Open a folder in a terminal emulator, found the same way as the archive tools: nothing is
//! linked in, whatever is in `PATH` shows up in the preference.

use std::path::Path;

use gettextrs::gettext;

use crate::gio::prelude::*;
use crate::{gio, glib};

pub struct Terminal {
    pub exec: &'static str,
    pub label: &'static str,
    /// Arguments that pick the working directory; `{}` stands for the folder. Terminals
    /// without one inherit the spawn directory, which is set in every case.
    args: &'static [&'static str],
}

/// Preference order for the automatic choice. `xdg-terminal-exec` is first because it is the
/// desktop's own default terminal (freedesktop `xdg-terminals.list`).
const TERMINALS: &[Terminal] = &[
    Terminal {
        exec: "xdg-terminal-exec",
        label: "System Default",
        args: &[],
    },
    Terminal {
        exec: "ghostty",
        label: "Ghostty",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "kitty",
        label: "kitty",
        args: &["--directory={}"],
    },
    Terminal {
        exec: "alacritty",
        label: "Alacritty",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "foot",
        label: "foot",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "wezterm",
        label: "WezTerm",
        args: &["start", "--cwd", "{}"],
    },
    Terminal {
        exec: "ptyxis",
        label: "Ptyxis",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "kgx",
        label: "Console",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "gnome-terminal",
        label: "GNOME Terminal",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "konsole",
        label: "Konsole",
        args: &["--workdir={}"],
    },
    Terminal {
        exec: "xfce4-terminal",
        label: "Xfce Terminal",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "tilix",
        label: "Tilix",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "terminator",
        label: "Terminator",
        args: &["--working-directory={}"],
    },
    Terminal {
        exec: "xterm",
        label: "xterm",
        args: &[],
    },
    Terminal {
        exec: "urxvt",
        label: "urxvt",
        args: &[],
    },
];

/// Terminals present in `PATH`, in preference order.
pub fn installed() -> Vec<&'static Terminal> {
    TERMINALS
        .iter()
        .filter(|t| glib::find_program_in_path(t.exec).is_some())
        .collect()
}

/// The preferred terminal if still installed, else the first one found.
pub fn chosen() -> Option<&'static Terminal> {
    let want = crate::prefs::settings().string("terminal");
    let found = installed();
    found
        .iter()
        .find(|t| t.exec == want.as_str())
        .or(found.first())
        .copied()
}

/// Open `dir` in the chosen terminal.
pub fn open(dir: &Path) -> Result<(), glib::Error> {
    let Some(t) = chosen() else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotFound,
            &gettext("No terminal emulator found"),
        ));
    };
    let dir_s = dir.to_string_lossy();
    let argv: Vec<String> = std::iter::once(t.exec.to_string())
        .chain(t.args.iter().map(|a| a.replace("{}", &dir_s)))
        .collect();
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    // The launcher inherits the environment (display, session bus) and reaps the child.
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
    launcher.set_cwd(dir);
    launcher.spawn(&argv).map(|_| ())
}
