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
        exec: "caret",
        label: "Caret",
        args: &["--working-directory={}"],
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

impl Terminal {
    /// The terminal started in `dir`, running `command` there, or a shell without one.
    fn argv(&self, dir: &Path, command: &[String]) -> Vec<String> {
        let dir = dir.to_string_lossy();
        let mut argv: Vec<String> = std::iter::once(self.exec.to_string())
            .chain(self.args.iter().map(|a| a.replace("{}", &dir)))
            .collect();
        if !command.is_empty() {
            argv.extend(self.run_args.iter().map(|a| a.to_string()));
            argv.extend(command.iter().cloned());
        }
        argv
    }
}

/// Which of the terminals are on the system, in preference order, looked up once: `PATH`
/// does not change under a running process, and this is asked again for every change of
/// the selection.
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
/// The first entry found is the one GIO launches: a user's own copy hides the system's.
fn wants_terminal(app: &gio::AppInfo) -> bool {
    let Some(id) = app.id() else { return false };
    let mut dirs = vec![glib::user_data_dir()];
    dirs.extend(glib::system_data_dirs());
    dirs.into_iter()
        .find_map(|dir| {
            let file = glib::KeyFile::new();
            file.load_from_file(
                dir.join("applications").join(id.as_str()),
                glib::KeyFileFlags::NONE,
            )
            .ok()?;
            Some(
                file.boolean(glib::KEY_FILE_DESKTOP_GROUP, "Terminal")
                    .unwrap_or(false),
            )
        })
        .unwrap_or(false)
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
/// start those itself -- it will not go looking for a terminal emulator -- so an editor that runs
/// in a terminal, set as the default for a file type, would only ever say it could not be
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
    let dir = files
        .first()
        .and_then(|f| f.parent())
        .and_then(|p| p.path())
        .unwrap_or_else(glib::home_dir);
    Some(spawn(&dir, &command_line(&argv, files)))
}

/// The application's command line with `files` put where its field codes say. One that
/// names none takes them at the end, as the specification says a launcher should.
fn command_line(argv: &[std::ffi::OsString], files: &[gio::File]) -> Vec<String> {
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
    command
}

fn spawn(dir: &Path, command: &[String]) -> Result<(), glib::Error> {
    let Some(t) = chosen() else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotFound,
            &gettext("No terminal emulator found"),
        ));
    };
    let argv = t.argv(dir, command);
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    // The launcher inherits the environment (display, session bus) and reaps the child.
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::NONE);
    launcher.set_cwd(dir);
    launcher.spawn(&argv).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(exec: &str) -> &'static Terminal {
        TERMINALS.iter().find(|t| t.exec == exec).unwrap()
    }

    fn argv(line: &str) -> Vec<std::ffi::OsString> {
        glib::shell_parse_argv(line).unwrap()
    }

    #[test]
    fn a_folder_opens_with_the_terminal_s_own_option_for_it() {
        let dir = Path::new("/home/me/My Files");
        assert_eq!(
            terminal("caret").argv(dir, &[]),
            ["caret", "--working-directory=/home/me/My Files"]
        );
        assert_eq!(
            terminal("wezterm").argv(dir, &[]),
            ["wezterm", "start", "--cwd", "/home/me/My Files"]
        );
        // Started in the folder, which it inherits.
        assert_eq!(terminal("xterm").argv(dir, &[]), ["xterm"]);
    }

    #[test]
    fn a_command_follows_what_introduces_it_and_keeps_its_arguments_whole() {
        let command = ["vim".to_string(), "a b.txt".to_string()];
        assert_eq!(
            terminal("caret").argv(Path::new("/tmp"), &command),
            ["caret", "--working-directory=/tmp", "--", "vim", "a b.txt"]
        );
        assert_eq!(
            terminal("konsole").argv(Path::new("/tmp"), &command),
            ["konsole", "--workdir=/tmp", "-e", "vim", "a b.txt"]
        );
        assert_eq!(
            terminal("xdg-terminal-exec").argv(Path::new("/tmp"), &command),
            ["xdg-terminal-exec", "vim", "a b.txt"]
        );
    }

    #[test]
    fn every_terminal_has_one_entry() {
        let mut execs: Vec<_> = TERMINALS.iter().map(|t| t.exec).collect();
        execs.sort_unstable();
        execs.dedup();
        assert_eq!(execs.len(), TERMINALS.len());
    }

    #[test]
    fn files_go_where_the_field_code_puts_them() {
        let files = [
            gio::File::for_path("/home/me/a.rs"),
            gio::File::for_path("/home/me/b c.rs"),
        ];
        assert_eq!(
            command_line(&argv("nvim --remote %F -c 'set nu'"), &files),
            [
                "nvim",
                "--remote",
                "/home/me/a.rs",
                "/home/me/b c.rs",
                "-c",
                "set nu"
            ]
        );
        // Icon, name and entry path are dropped.
        assert_eq!(
            command_line(&argv("hx %i %c %k %f"), &files[..1]),
            ["hx", "/home/me/a.rs"]
        );
    }

    #[test]
    fn a_command_line_without_a_field_code_takes_the_files_at_the_end() {
        let files = [gio::File::for_path("/home/me/a.rs")];
        assert_eq!(
            command_line(&argv("micro -autosave 1"), &files),
            ["micro", "-autosave", "1", "/home/me/a.rs"]
        );
    }

    #[test]
    fn a_file_with_no_path_is_given_by_its_uri() {
        let files = [gio::File::for_uri("sftp://host/srv/a.rs")];
        let command = command_line(&argv("vim %U"), &files);
        assert_eq!(command[1], files[0].uri().as_str());
    }
}
