//! Running external CLIs (colima, docker, podman, ...).
//!
//! GUI apps on macOS are launched with a minimal PATH (no Homebrew), so every
//! command runs with an augmented search path and binaries are resolved
//! explicitly against it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

use crate::error::{AppError, Result};

/// Default timeout for short, non-streaming commands.
pub const QUICK: Duration = Duration::from_secs(30);

fn extra_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        dirs.extend(
            [
                "/opt/homebrew/bin",
                "/opt/homebrew/sbin",
                "/usr/local/bin",
                "/opt/local/bin",
                "/Applications/Docker.app/Contents/Resources/bin",
            ]
            .map(PathBuf::from),
        );
        dirs.push(home.join(".orbstack/bin"));
    }
    #[cfg(target_os = "linux")]
    {
        dirs.extend(
            [
                "/usr/local/bin",
                "/home/linuxbrew/.linuxbrew/bin",
                "/snap/bin",
            ]
            .map(PathBuf::from),
        );
    }
    #[cfg(target_os = "windows")]
    {
        dirs.extend(
            [
                r"C:\Program Files\Docker\Docker\resources\bin",
                r"C:\Program Files\RedHat\Podman",
            ]
            .map(PathBuf::from),
        );
    }
    #[cfg(unix)]
    {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".rd/bin")); // Rancher Desktop
        dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    }
    dirs
}

/// The PATH used for every child process: the inherited PATH plus well-known
/// install locations for container tooling.
pub fn search_path() -> &'static OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for d in extra_dirs() {
            if !paths.contains(&d) {
                paths.push(d);
            }
        }
        std::env::join_paths(paths).unwrap_or_default()
    })
}

/// Locate `program` on the augmented search path.
pub fn resolve(program: &str) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) {
        vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            program.to_string(),
        ]
    } else {
        vec![program.to_string()]
    };
    std::env::split_paths(search_path())
        .find_map(|dir| candidates.iter().map(|c| dir.join(c)).find(|p| p.is_file()))
}

/// Build a command for `program` with the augmented PATH applied.
pub fn command(program: &str) -> Result<Command> {
    let bin = resolve(program).ok_or_else(|| {
        AppError::msg(format!(
            "`{program}` was not found on PATH or in common install locations"
        ))
    })?;
    let mut cmd = Command::new(bin);
    cmd.env("PATH", search_path())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

/// Run to completion and return stdout. Non-zero exit becomes an error carrying stderr.
pub async fn output(mut cmd: Command, timeout: Duration) -> Result<String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| AppError::msg(format!("command timed out after {}s", timeout.as_secs())))??;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        Err(AppError::msg(format!(
            "exited with {}: {}",
            out.status,
            detail.trim()
        )))
    }
}

pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

async fn pump<R: AsyncRead + Unpin>(reader: R, sink: LineSink) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        sink(&line);
    }
}

/// Run a long command, forwarding every stdout/stderr line to `sink`.
pub async fn stream(mut cmd: Command, sink: LineSink) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (_, _, status) = tokio::join!(
        pump(stdout, sink.clone()),
        pump(stderr, sink.clone()),
        child.wait()
    );
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::msg(format!("exited with {status}")))
    }
}

/// Copy the user's login-shell environment into this process. Must run
/// before any other threads start.
///
/// Apps launched from Finder get a minimal environment, so `docker` would
/// miss things like `AWS_PROFILE` that credential helpers (ECR's
/// `docker-credential-ecr-login`) need, plus the user's PATH. Variables that
/// would retarget the engine are skipped; the app chooses the engine itself.
#[cfg(unix)]
pub fn inherit_login_env() {
    const MARK: &str = "__SCUTTLE_ENV__";
    const SKIP: &[&str] = &[
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
        "TERM_SESSION_ID",
    ];
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/sh"
        }
        .into()
    });
    let Ok(mut child) = std::process::Command::new(&shell)
        .args(["-ilc", &format!("printf %s {MARK}; env -0")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    // Drain stdout on a helper thread (so a large env can't fill the pipe),
    // and don't let a slow or interactive dotfile hang startup.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => break false,
        }
    };
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Joined before any env mutation, so no other thread is running.
    let out = reader.join().unwrap_or_default();
    if !finished {
        return;
    }
    let out = String::from_utf8_lossy(&out);
    let Some((_, env)) = out.split_once(MARK) else {
        return;
    };
    for entry in env.split('\0') {
        if let Some((k, v)) = entry.split_once('=') {
            if !k.is_empty() && !SKIP.contains(&k) && !k.contains(char::is_whitespace) {
                // Called from main before the app starts any threads.
                std::env::set_var(k, v);
            }
        }
    }
}

#[cfg(not(unix))]
pub fn inherit_login_env() {}
