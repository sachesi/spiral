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
    /// Arguments that introduce a command to run instead of a shell.
    run_args: &'static [&'static str],
}

/// Preference order for the automatic choice. `xdg-terminal-exec` is first because it is the
/// desktop's own default terminal (freedesktop `xdg-terminals.list`).
const TERMINALS: &[Terminal] = &[
    Terminal {
        exec: "xdg-terminal-exec",
        label: "System Default",
        args: &[],
        run_args: &[],
    },
    Terminal {
        exec: "ghostty",
        label: "Ghostty",
        args: &["--working-directory={}"],
        run_args: &["-e"],
    },
    Terminal {
        exec: "kitty",
        label: "kitty",
        args: &["--directory={}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "alacritty",
        label: "Alacritty",
        args: &["--working-directory={}"],
        run_args: &["-e"],
    },
    Terminal {
        exec: "foot",
        label: "foot",
        args: &["--working-directory={}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "wezterm",
        label: "WezTerm",
        args: &["start", "--cwd", "{}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "ptyxis",
        label: "Ptyxis",
        args: &["--working-directory={}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "kgx",
        label: "Console",
        args: &["--working-directory={}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "gnome-terminal",
        label: "GNOME Terminal",
        args: &["--working-directory={}"],
        run_args: &["--"],
    },
    Terminal {
        exec: "konsole",
        label: "Konsole",
        args: &["--workdir={}"],
        run_args: &["-e"],
    },
    Terminal {
        exec: "xfce4-terminal",
        label: "Xfce Terminal",
        args: &["--working-directory={}"],
        run_args: &["-x"],
    },
    Terminal {
        exec: "tilix",
        label: "Tilix",
        args: &["--working-directory={}"],
        run_args: &["-e"],
    },
    Terminal {
        exec: "terminator",
        label: "Terminator",
        args: &["--working-directory={}"],
        run_args: &["-x"],
    },
    Terminal {
        exec: "xterm",
        label: "xterm",
        args: &[],
        run_args: &["-e"],
    },
    Terminal {
        exec: "urxvt",
        label: "urxvt",
        args: &[],
        run_args: &["-e"],
    },
];

/// Terminals present in `PATH`, in preference order.
/// Which of the terminals are on the system, looked up once: `PATH` does not change under
/// a running process, and this is asked again for every change of the selection.
pub fn installed() -> Vec<&'static Terminal> {
    thread_local! {
        static FOUND: std::cell::OnceCell<Vec<&'static Terminal>> =
            const { std::cell::OnceCell::new() };
    }
    FOUND.with(|found| {
        found
            .get_or_init(|| {
                TERMINALS
                    .iter()
                    .filter(|t| glib::find_program_in_path(t.exec).is_some())
                    .collect()
            })
            .clone()
    })
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

/// Whether the application's desktop entry has `Terminal=true`. The entry is read from disk
/// because the `gio` bindings do not carry `GDesktopAppInfo`, which is where GIO keeps it.
fn wants_terminal(app: &gio::AppInfo) -> bool {
    let Some(id) = app.id() else { return false };
    let mut dirs = vec![glib::user_data_dir()];
    dirs.extend(glib::system_data_dirs());
    dirs.into_iter().any(|dir| {
        let file = glib::KeyFile::new();
        file.load_from_file(
            dir.join("applications").join(id.as_str()),
            glib::KeyFileFlags::NONE,
        )
        .is_ok()
            && file
                .boolean(glib::KEY_FILE_DESKTOP_GROUP, "Terminal")
                .unwrap_or(false)
    })
}

/// Open `dir` in the chosen terminal.
pub fn open(dir: &Path) -> Result<(), glib::Error> {
    spawn(dir, &[])
}

/// Run `program` in the chosen terminal, in `dir`; the terminal closes when it exits.
pub fn run(dir: &Path, program: &Path) -> Result<(), glib::Error> {
    spawn(dir, &[program.to_string_lossy().into_owned()])
}

/// Start `app` on `files`, in a terminal if its desktop entry asks for one. GIO refuses to
/// start those itself -- it will not go looking for a terminal emulator -- so an editor like
/// Helix or Vim set as the default for a file type would only ever say it could not be
/// opened. Returns false for an application that wants no terminal, which is launched the
/// ordinary way.
pub fn launch_if_wanted(
    app: &gio::AppInfo,
    files: &[gio::File],
) -> Option<Result<(), glib::Error>> {
    if !wants_terminal(app) {
        return None;
    }
    let line = app.commandline()?;
    let Ok(argv) = glib::shell_parse_argv(line.as_os_str()) else {
        return Some(Err(glib::Error::new(
            gio::IOErrorEnum::Failed,
            &gettext("The command line of the application cannot be read"),
        )));
    };
    // The desktop entry names where the files go with a field code; one that names none
    // takes them at the end, as the specification says a launcher should.
    let mut command: Vec<String> = Vec::new();
    let mut placed = false;
    for arg in argv {
        match arg.to_string_lossy().as_ref() {
            "%f" | "%F" | "%u" | "%U" => {
                command.extend(files.iter().map(|f| match f.path() {
                    Some(path) => path.to_string_lossy().into_owned(),
                    None => f.uri().to_string(),
                }));
                placed = true;
            }
            // Icon, translated name and entry path: nothing a command line wants.
            other if other.starts_with('%') => {}
            other => command.push(other.to_string()),
        }
    }
    if !placed {
        command.extend(
            files
                .iter()
                .filter_map(|f| f.path())
                .map(|p| p.to_string_lossy().into_owned()),
        );
    }
    let dir = files
        .first()
        .and_then(|f| f.parent())
        .and_then(|p| p.path())
        .unwrap_or_else(glib::home_dir);
    Some(spawn(&dir, &command))
}

fn spawn(dir: &Path, command: &[String]) -> Result<(), glib::Error> {
    let Some(t) = chosen() else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotFound,
            &gettext("No terminal emulator found"),
        ));
    };
    let dir_s = dir.to_string_lossy();
    let mut argv: Vec<String> = std::iter::once(t.exec.to_string())
        .chain(t.args.iter().map(|a| a.replace("{}", &dir_s)))
        .collect();
    if !command.is_empty() {
        argv.extend(t.run_args.iter().map(|a| a.to_string()));
        argv.extend(command.iter().cloned());
    }
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    // The launcher inherits the environment (display, session bus) and reaps the child.
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
    launcher.set_cwd(dir);
    launcher.spawn(&argv).map(|_| ())
}
