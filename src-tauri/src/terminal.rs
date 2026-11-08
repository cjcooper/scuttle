//! Open an interactive command (docker exec -it / run -it) in the user's
//! terminal emulator. Interactive TTYs need a real terminal, so instead of
//! embedding one we hand off a small script to Terminal.app / iTerm2 / the
//! platform's terminal.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::{AppError, Result};
use crate::process;

#[derive(Debug, Clone, Serialize)]
pub struct TerminalApp {
    pub id: &'static str,
    pub name: &'static str,
}

/// Terminals available on this machine (besides "auto").
pub fn available() -> Vec<TerminalApp> {
    let mut v = Vec::new();
    #[cfg(target_os = "macos")]
    {
        v.push(TerminalApp {
            id: "terminal",
            name: "Terminal",
        });
        if std::path::Path::new("/Applications/iTerm.app").exists() {
            v.push(TerminalApp {
                id: "iterm",
                name: "iTerm2",
            });
        }
    }
    #[cfg(target_os = "linux")]
    for (id, name) in LINUX_TERMINALS.iter().map(|(b, _)| (*b, *b)) {
        if process::resolve(id).is_some() {
            v.push(TerminalApp { id, name });
        }
    }
    #[cfg(windows)]
    {
        if process::resolve("wt").is_some() {
            v.push(TerminalApp {
                id: "wt",
                name: "Windows Terminal",
            });
        }
        v.push(TerminalApp {
            id: "cmd",
            name: "Command Prompt",
        });
    }
    v
}

#[cfg(target_os = "linux")]
const LINUX_TERMINALS: &[(&str, &[&str])] = &[
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("xterm", &["-e"]),
];

fn script_path(ext: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("scuttle-{nanos}.{ext}"))
}

/// Launch `command_line` (already shell-quoted) in a new terminal window.
pub fn launch(command_line: &str, title: &str, preference: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ext = if cfg!(target_os = "macos") {
            "command"
        } else {
            "sh"
        };
        let path = script_path(ext);
        let script = unix_script(command_line, title);
        let mut f = std::fs::File::create(&path)?;
        f.write_all(script.as_bytes())?;
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        open_unix(&path, preference)
    }
    #[cfg(windows)]
    {
        let path = script_path("cmd");
        let script = format!("@echo off\r\ntitle {title}\r\necho {command_line}\r\n{command_line}\r\npause\r\ndel \"%~f0\"\r\n");
        std::fs::write(&path, script)?;
        let p = path.to_string_lossy().to_string();
        let mut c = std::process::Command::new(if preference == "wt" { "wt" } else { "cmd" });
        if preference == "wt" {
            c.args(["cmd", "/c", &p]);
        } else {
            c.args(["/c", "start", "", "cmd", "/c", &p]);
        }
        c.spawn()?;
        Ok(())
    }
}

/// The launcher script: sets the title and PATH, then runs the command via
/// the user's interactive login shell so AWS_PROFILE, credential helpers etc.
/// from their dotfiles are available (needed for e.g. ECR's
/// docker-credential-ecr-login). Keeps the window open on failure.
#[cfg(unix)]
fn unix_script(command_line: &str, title: &str) -> String {
    let q = crate::engine::shell_quote;
    format!(
        "#!/bin/sh\n\
         rm -f \"$0\"\n\
         printf '\\033]0;%s\\007' {title}\n\
         export PATH={path}\n\
         clear\n\
         echo {echo}\n\
         \"${{SHELL:-/bin/sh}}\" -lic {cmd}\n\
         status=$?\n\
         if [ $status -ne 0 ]; then echo; echo \"[exited with status $status — press Return to close]\"; read _; fi\n",
        title = q(title),
        path = q(&process::search_path().to_string_lossy()),
        echo = q(&format!("$ {command_line}")),
        cmd = q(command_line),
    )
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn script_runs_command_line() {
        let dir = std::env::temp_dir().join(format!("scuttle-term-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("t.sh");
        std::fs::write(
            &script,
            super::unix_script("printf '%s|' 'a b' \"$HOME\" >&2", "t"),
        )
        .unwrap();
        let out = std::process::Command::new("/bin/sh")
            .arg(&script)
            .env("SHELL", "/bin/sh")
            .output()
            .unwrap();
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("a b|"), "stderr: {err}");
        assert!(!script.exists(), "script should delete itself");
        let _ = std::fs::remove_dir(&dir);
    }
}

#[cfg(target_os = "macos")]
fn open_unix(script: &std::path::Path, preference: &str) -> Result<()> {
    let app = match preference {
        "terminal" => "Terminal",
        "iterm" => "iTerm",
        // auto: prefer iTerm2 when installed
        _ if std::path::Path::new("/Applications/iTerm.app").exists() => "iTerm",
        _ => "Terminal",
    };
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-a", app])
        .arg(script)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::msg(format!("could not open {app}")))
    }
}

#[cfg(target_os = "linux")]
fn open_unix(script: &std::path::Path, preference: &str) -> Result<()> {
    let candidates = LINUX_TERMINALS
        .iter()
        .filter(|(b, _)| preference == "auto" || preference.is_empty() || *b == preference);
    for (bin, args) in candidates {
        if let Some(path) = process::resolve(bin) {
            std::process::Command::new(path)
                .args(*args)
                .arg("/bin/sh")
                .arg(script)
                .spawn()?;
            return Ok(());
        }
    }
    Err(AppError::msg("no supported terminal emulator found"))
}
