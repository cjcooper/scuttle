//! Integrated shell windows: a native window per interactive session
//! (`docker exec -it` into a container, or `docker run -it` of an image),
//! rendered with xterm.js and backed by a real PTY running the docker CLI.
//!
//! Each window keeps its target so it can reconnect, and lets the user switch
//! the user/shell and reconnect without leaving the window.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::engine::{self, Docker, RunRequest};
use crate::error::{AppError, Result};
use crate::process;
use crate::state::AppState;

/// Window labels look like `shell-3`; capabilities grant `shell-*`.
pub const LABEL_PREFIX: &str = "shell-";

/// What a shell window connects to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[allow(clippy::large_enum_variant)] // few, short-lived values
pub enum ShellTarget {
    /// `docker exec -it` into a running container.
    Exec { container: String },
    /// `docker run -it` of an image. When `shell_entrypoint` is set the
    /// entrypoint/command come from the window's shell choice (the
    /// "shell into image" action); otherwise the request runs as given.
    Run {
        request: RunRequest,
        #[serde(default)]
        shell_entrypoint: bool,
    },
}

impl ShellTarget {
    fn title(&self) -> String {
        match self {
            ShellTarget::Exec { container } => container.clone(),
            ShellTarget::Run { request, .. } => request
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| request.image.clone()),
        }
    }
}

/// Per-connection options the window can change before reconnecting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOptions {
    /// User spec: `""`, `"root"`, `"host"` or a literal user.
    pub user: String,
    /// `"auto"` or a shell path.
    pub shell: String,
}

/// Output events, sent to the owning window under its label.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ShellOutput {
    /// Base64 of raw terminal bytes.
    Data { generation: u64, data: String },
    /// The docker process ended.
    Exited {
        generation: u64,
        code: Option<u32>,
        /// True when the user left the shell (window can close), false when
        /// it failed (show the message and offer Reconnect).
        clean: bool,
        message: String,
    },
}

/// Returned to the window when it (re)connects.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub title: String,
    pub target: ShellTarget,
    pub options: ShellOptions,
    /// The docker command being run, for display.
    pub command: String,
    pub generation: u64,
    /// The event name to listen on (the window label).
    pub event: String,
}

struct Connection {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Container name to force-remove on close (throwaway `--rm` runs).
    cleanup: Option<String>,
}

struct Session {
    target: ShellTarget,
    options: ShellOptions,
    title: String,
    generation: u64,
    conn: Option<Connection>,
}

#[derive(Default)]
pub struct ShellSessions {
    sessions: Mutex<HashMap<String, Session>>,
    next_id: AtomicU64,
}

impl ShellSessions {
    /// The window closed: end its session.
    pub fn stop(&self, app: &AppHandle, label: &str) {
        let session = self.sessions.lock().unwrap().remove(label);
        if let Some(name) = session.and_then(|s| s.conn).and_then(disconnect) {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { remove_container(&app, &name).await });
        }
    }
}

/// Kill the docker client. Returns the throwaway container to remove:
/// killing the CLI of a `docker run -it` leaves the container running.
fn disconnect(mut conn: Connection) -> Option<String> {
    let _ = conn.killer.kill();
    conn.cleanup.take()
}

async fn remove_container(app: &AppHandle, name: &str) {
    if let Ok(docker) = app.state::<AppState>().docker().await {
        let _ = docker.force_remove(name).await;
    }
}

/// Open a new shell window for `target`. `options` defaults to the saved
/// "Shell as" preferences.
pub fn open(app: &AppHandle, target: ShellTarget, options: Option<ShellOptions>) -> Result<()> {
    let sessions = app.state::<ShellSessions>();
    let options = options.unwrap_or_else(|| {
        let prefs = app.state::<AppState>().preferences();
        ShellOptions {
            user: prefs.user_spec(),
            shell: prefs.shell,
        }
    });
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let label = format!("{LABEL_PREFIX}{id}");
    let mut title = target.title();
    let same = sessions
        .sessions
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.target.title() == title)
        .count();
    if same > 0 {
        title.push_str(&format!(" ({})", same + 1));
    }
    sessions.sessions.lock().unwrap().insert(
        label.clone(),
        Session {
            target,
            options,
            title: title.clone(),
            generation: 0,
            conn: None,
        },
    );
    WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("shell.html?label={label}").into()),
    )
    .title(&title)
    .inner_size(960.0, 600.0)
    .min_inner_size(420.0, 240.0)
    .disable_drag_drop_handler()
    .build()
    .map_err(|e| {
        sessions.sessions.lock().unwrap().remove(&label);
        AppError::from(e)
    })?;
    Ok(())
}

/// A user/shell picked in a shell window becomes the default for new ones
/// (and the tray's Shell User menu).
fn remember_options(app: &AppHandle, o: &ShellOptions) {
    let state = app.state::<AppState>();
    let _ = state.update_settings(|s| {
        let p = &mut s.preferences;
        match o.user.as_str() {
            u @ ("" | "root" | "host") => p.shell_user = u.to_string(),
            custom => {
                p.shell_user = "custom".into();
                p.custom_user = custom.to_string();
            }
        }
        p.shell = o.shell.clone();
    });
    let _ = app.emit("preferences", state.preferences());
    crate::tray::rebuild(app);
}

/// The docker argv for a session.
fn docker_args(
    target: &ShellTarget,
    options: &ShellOptions,
) -> Result<(Vec<String>, Option<String>)> {
    match target {
        ShellTarget::Exec { container } => Ok((
            engine::exec_args(container, &options.user, &options.shell)?,
            None,
        )),
        ShellTarget::Run {
            request,
            shell_entrypoint,
        } => {
            let mut req = if *shell_entrypoint {
                let mut r =
                    engine::shell_run_request(&request.image, &options.user, &options.shell);
                r.platform = request.platform.clone();
                r.aws_profile = request.aws_profile.clone();
                r
            } else {
                RunRequest {
                    user: options.user.clone(),
                    ..request.clone()
                }
            };
            req.interactive = true;
            // Name throwaway containers so closing the window can remove them.
            let cleanup = if req.remove {
                let name = req
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| {
                        let ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or_default();
                        format!("cd-shell-{ms}")
                    });
                req.name = Some(name.clone());
                Some(name)
            } else {
                None
            };
            Ok((engine::run_args(&req)?, cleanup))
        }
    }
}

/// Exit codes from the docker CLI itself (daemon error, cannot invoke, not
/// found) rather than from the user's shell.
fn is_docker_failure(code: u32) -> bool {
    matches!(code, 125..=127)
}

/// Called by a shell window once it is listening and knows its size, and
/// again for Reconnect (optionally with new user/shell options).
pub async fn start(
    window: WebviewWindow,
    cols: u16,
    rows: u16,
    options: Option<ShellOptions>,
) -> Result<SessionInfo> {
    let app = window.app_handle().clone();
    let label = window.label().to_string();

    let sessions = app.state::<ShellSessions>();
    let requested = options.clone();
    let (target, options, title, generation, old) = {
        let mut map = sessions.sessions.lock().unwrap();
        let s = map
            .get_mut(&label)
            .ok_or_else(|| AppError::msg("this shell window is no longer connected"))?;
        if let Some(o) = options.clone() {
            s.options = o;
        }
        s.generation += 1;
        (
            s.target.clone(),
            s.options.clone(),
            s.title.clone(),
            s.generation,
            s.conn.take(),
        )
    };
    if let Some(o) = &requested {
        remember_options(&app, o);
    }
    // Finish removing the previous container before starting a new one, in
    // case it reuses the same name.
    if let Some(name) = old.and_then(disconnect) {
        remove_container(&app, &name).await;
    }

    // ECR runs: docker needs AWS_PROFILE (and maybe a login) to pull.
    let docker: Docker = match &target {
        ShellTarget::Run { request, .. } => {
            // Login progress goes to the terminal itself.
            let (w, event) = (window.clone(), label.clone());
            let sink: process::LineSink = std::sync::Arc::new(move |line: &str| {
                let data = base64::engine::general_purpose::STANDARD
                    .encode(format!("\x1b[2m{line}\x1b[0m\r\n"));
                let _ = w.emit_to(w.label(), &event, ShellOutput::Data { generation, data });
            });
            crate::commands::docker_for_image(
                &app,
                &request.image,
                request.aws_profile.as_deref(),
                false,
                &sink,
            )
            .await?
        }
        ShellTarget::Exec { .. } => app.state::<AppState>().docker().await?,
    };
    let (args, cleanup) = docker_args(&target, &options)?;
    let preview = std::iter::once("docker".to_string())
        .chain(args.iter().map(|a| engine::shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AppError::msg(format!("could not open a terminal: {e}")))?;
    let bin = process::resolve("docker").ok_or_else(|| AppError::msg("`docker` was not found"))?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.args(docker.global_args());
    cmd.args(&args);
    cmd.env("PATH", process::search_path());
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("DOCKER_HOST");
    cmd.env_remove("DOCKER_CONTEXT");
    for (k, v) in docker.env() {
        cmd.env(k, v);
    }
    if let Some(home) = std::env::var_os("HOME") {
        cmd.cwd(home);
    }
    let mut child = pty
        .slave
        .spawn_command(cmd)
        .map_err(|e| AppError::msg(format!("could not start docker: {e}")))?;
    // Drop our copy of the slave so the reader sees EOF when docker exits.
    drop(pty.slave);
    let killer = child.clone_killer();
    let mut reader = pty
        .master
        .try_clone_reader()
        .map_err(|e| AppError::msg(e.to_string()))?;
    let writer = pty
        .master
        .take_writer()
        .map_err(|e| AppError::msg(e.to_string()))?;

    {
        let mut map = sessions.sessions.lock().unwrap();
        match map.get_mut(&label) {
            Some(s) if s.generation == generation => {
                s.conn = Some(Connection {
                    writer,
                    master: pty.master,
                    killer,
                    cleanup,
                });
            }
            // Window closed or reconnected meanwhile.
            _ => {
                let _ = child.kill();
                return Err(AppError::msg("session superseded"));
            }
        }
    }

    let event = label.clone();
    let emitter = window.clone();
    let started = Instant::now();
    std::thread::spawn(move || {
        // A connection replaced by a reconnect (user/shell change) is killed;
        // its leftover output and exit must not reach the window, or the
        // window would take the kill for the user leaving and close.
        let current = || {
            emitter
                .state::<ShellSessions>()
                .sessions
                .lock()
                .unwrap()
                .get(emitter.label())
                .is_some_and(|s| s.generation == generation)
        };
        let emit = |out: ShellOutput| {
            if current() {
                let _ = emitter.emit_to(emitter.label(), &event, out);
            }
        };
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => emit(ShellOutput::Data {
                    generation,
                    data: b64.encode(&buf[..n]),
                }),
            }
        }
        let code = child.wait().ok().map(|s| s.exit_code());
        // Quick or docker-level failures keep the window open so the error
        // stays readable; anything else is the user leaving the shell.
        let clean = matches!(code, Some(c) if !is_docker_failure(c))
            && started.elapsed() > Duration::from_secs(2);
        let message = match code {
            Some(0) => "Shell exited.".to_string(),
            Some(c) => format!("Process exited with status {c}."),
            None => "Shell ended.".to_string(),
        };
        emit(ShellOutput::Exited {
            generation,
            code,
            clean,
            message,
        });
    });

    Ok(SessionInfo {
        title,
        target,
        options,
        command: preview,
        generation,
        event: label,
    })
}

pub fn input(app: &AppHandle, label: &str, data: &[u8]) {
    if let Some(conn) = app
        .state::<ShellSessions>()
        .sessions
        .lock()
        .unwrap()
        .get_mut(label)
        .and_then(|s| s.conn.as_mut())
    {
        let _ = conn.writer.write_all(data);
        let _ = conn.writer.flush();
    }
}

pub fn resize(app: &AppHandle, label: &str, cols: u16, rows: u16) {
    if let Some(conn) = app
        .state::<ShellSessions>()
        .sessions
        .lock()
        .unwrap()
        .get(label)
        .and_then(|s| s.conn.as_ref())
    {
        let _ = conn.master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn shell_start(
    window: WebviewWindow,
    cols: u16,
    rows: u16,
    options: Option<ShellOptions>,
) -> Result<SessionInfo> {
    start(window, cols, rows, options).await
}

#[tauri::command]
pub fn shell_input(window: WebviewWindow, data: String) {
    input(window.app_handle(), window.label(), data.as_bytes());
}

#[tauri::command]
pub fn shell_resize(window: WebviewWindow, cols: u16, rows: u16) {
    resize(window.app_handle(), window.label(), cols, rows);
}

/// Pop the current session's command out to the external terminal app.
#[tauri::command]
pub async fn shell_pop_out(window: WebviewWindow) -> Result<()> {
    let app = window.app_handle().clone();
    let (target, options, title) = {
        let sessions = app.state::<ShellSessions>();
        let map = sessions.sessions.lock().unwrap();
        let s = map
            .get(window.label())
            .ok_or_else(|| AppError::msg("this shell window is no longer connected"))?;
        (s.target.clone(), s.options.clone(), s.title.clone())
    };
    let state = app.state::<AppState>();
    let docker = match &target {
        ShellTarget::Run { request, .. } => state
            .docker()
            .await?
            .with_aws_profile(request.aws_profile.as_deref()),
        ShellTarget::Exec { .. } => state.docker().await?,
    };
    let (args, _) = docker_args(&target, &options)?;
    crate::terminal::launch(
        &docker.command_line(&args)?,
        &title,
        &state.preferences().terminal,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_shell_session_args() {
        let target = ShellTarget::Run {
            request: RunRequest {
                image: "php:APP-1".into(),
                ..Default::default()
            },
            shell_entrypoint: true,
        };
        let opts = ShellOptions {
            user: "root".into(),
            shell: "/bin/bash".into(),
        };
        let (args, cleanup) = docker_args(&target, &opts).unwrap();
        let name = cleanup.expect("throwaway run gets a name");
        assert_eq!(
            args.join(" "),
            format!(
                "container run -it --rm --name {name} --user root --entrypoint /bin/bash php:APP-1"
            )
        );
    }

    /// Live: drive `docker run -it` of a local image through a real PTY.
    /// `IMAGE=<local image> cargo test pty_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn pty_live() {
        let image = std::env::var("IMAGE").unwrap_or_else(|_| "alpine".into());
        let target = ShellTarget::Run {
            request: RunRequest {
                image,
                ..Default::default()
            },
            shell_entrypoint: true,
        };
        let opts = ShellOptions {
            user: "root".into(),
            shell: "auto".into(),
        };
        let (args, _) = docker_args(&target, &opts).unwrap();
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(process::resolve("docker").unwrap());
        cmd.args(&args);
        cmd.env("TERM", "xterm-256color");
        let mut child = pty.slave.spawn_command(cmd).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        let mut writer = pty.master.take_writer().unwrap();
        let out = std::thread::spawn(move || {
            let mut all = Vec::new();
            let _ = reader.read_to_end(&mut all);
            String::from_utf8_lossy(&all).into_owned()
        });
        std::thread::sleep(Duration::from_secs(3));
        writer
            .write_all(b"echo USER=$(id -un) SHELL=$0\rexit\r")
            .unwrap();
        let status = child.wait().unwrap();
        drop(pty.master);
        let out = out.join().unwrap();
        println!("{out}");
        assert!(status.success());
        assert!(out.contains("USER=root"), "{out}");
    }

    #[test]
    fn exec_session_args_follow_options() {
        let target = ShellTarget::Exec {
            container: "web".into(),
        };
        let opts = ShellOptions {
            user: "".into(),
            shell: "/bin/sh".into(),
        };
        let (args, cleanup) = docker_args(&target, &opts).unwrap();
        assert_eq!(args.join(" "), "container exec -it web /bin/sh");
        assert!(cleanup.is_none());
    }
}
