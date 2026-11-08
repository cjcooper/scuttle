//! Registered Docker Compose stacks: started with the app (once the engine
//! is reachable) and stopped or taken down when the app quits.
//!
//! Commands run as `docker compose …` in the stack's directory against the
//! selected engine, with the stack's environment (optionally `UID`/`GID` of
//! the host user, which shells set but don't export, so compose files using
//! `user: "$UID:$GID"` work as they do from a prompt).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::commands::log_sink;
use crate::engine::{shell_quote, Docker};
use crate::error::{AppError, Result};
use crate::process::{self, LineSink};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnQuit {
    /// `docker compose stop` (like Ctrl-C on an attached `up`).
    #[default]
    Stop,
    /// `docker compose down` (also removes containers and networks).
    Down,
    /// Leave it running.
    Leave,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Stack {
    pub id: String,
    pub name: String,
    /// Working directory; compose files are relative to it.
    pub directory: String,
    /// `-f` files, in order. Empty = compose's default lookup.
    pub files: Vec<String>,
    /// `-p`; empty = compose's default (directory name).
    pub project_name: String,
    /// `--profile`
    pub profiles: Vec<String>,
    /// `--env-file`
    pub env_files: Vec<String>,
    /// Extra environment, `KEY=VALUE`.
    pub env: Vec<String>,
    /// Set `UID` and `GID` to the host user's.
    pub host_ids: bool,
    /// Extra arguments for `up` (e.g. `--build`); `-d` is always added.
    pub up_args: Vec<String>,
    /// Start when the app starts (once the engine is reachable).
    pub autostart: bool,
    pub on_quit: OnQuit,
}

impl Stack {
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(AppError::msg("give the stack a name"));
        }
        let dir = std::path::Path::new(self.directory.trim());
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(AppError::msg(format!(
                "`{}` is not a directory",
                self.directory
            )));
        }
        for f in &self.files {
            let p = dir.join(f.trim());
            if !p.is_file() {
                return Err(AppError::msg(format!(
                    "compose file `{}` not found in {}",
                    f, self.directory
                )));
            }
        }
        Ok(())
    }

    /// `compose` plus the global options (`-f`, `-p`, `--profile`, `--env-file`).
    fn base_args(&self) -> Vec<String> {
        let mut a = vec!["compose".to_string()];
        for f in self
            .files
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            a.extend(["-f".into(), f.into()]);
        }
        if !self.project_name.trim().is_empty() {
            a.extend(["-p".into(), self.project_name.trim().into()]);
        }
        for p in self
            .profiles
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            a.extend(["--profile".into(), p.into()]);
        }
        for e in self
            .env_files
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            a.extend(["--env-file".into(), e.into()]);
        }
        a
    }

    fn args(&self, sub: &[&str]) -> Vec<String> {
        let mut a = self.base_args();
        a.extend(sub.iter().map(|s| s.to_string()));
        a
    }

    fn up_args(&self) -> Vec<String> {
        let mut a = self.args(&["up", "-d"]);
        a.extend(
            self.up_args
                .iter()
                .filter(|s| !s.trim().is_empty())
                .cloned(),
        );
        a
    }

    /// The stack's environment for docker compose.
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if self.host_ids {
            if let Some((uid, gid)) = host_ids() {
                env.push(("UID".to_string(), uid));
                env.push(("GID".to_string(), gid));
            }
        }
        for line in &self.env {
            if let Some((k, v)) = line.split_once('=') {
                if !k.trim().is_empty() {
                    env.push((k.trim().to_string(), v.to_string()));
                }
            }
        }
        env
    }

    /// The `up` command as the user would type it (for display / history).
    pub fn command_line(&self) -> String {
        self.environment()
            .iter()
            .map(|(k, v)| format!("{k}={}", shell_quote(v)))
            .chain(std::iter::once("docker".to_string()))
            .chain(self.up_args().iter().map(|a| shell_quote(a)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(unix)]
fn host_ids() -> Option<(String, String)> {
    let id = |flag: &str| -> Option<String> {
        let out = std::process::Command::new("id").arg(flag).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    Some((id("-u")?, id("-g")?))
}

#[cfg(not(unix))]
fn host_ids() -> Option<(String, String)> {
    None
}

/// A docker command for `stack` against the selected engine.
async fn command(
    app: &AppHandle,
    stack: &Stack,
    args: &[String],
) -> Result<tokio::process::Command> {
    let docker: Docker = app.state::<AppState>().docker().await?;
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = docker.cli(&argv)?;
    cmd.current_dir(stack.directory.trim());
    cmd.envs(stack.environment());
    Ok(cmd)
}

/// Run a compose subcommand, streaming output to `sink`.
async fn run(app: &AppHandle, stack: &Stack, args: Vec<String>, sink: LineSink) -> Result<()> {
    let display = std::iter::once("docker".to_string())
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    sink(&format!("[{}] $ {display}", stack.name));
    let cmd = command(app, stack, &args).await?;
    process::stream(cmd, sink).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "PascalCase"))]
pub struct Service {
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub ports: String,
}

/// `docker compose ps -a --format json` (JSON lines on newer compose, an
/// array on older).
fn parse_ps(out: &str) -> Vec<Service> {
    let out = out.trim();
    if out.starts_with('[') {
        serde_json::from_str(out).unwrap_or_default()
    } else {
        out.lines()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect()
    }
}

pub async fn services(app: &AppHandle, stack: &Stack) -> Result<Vec<Service>> {
    let cmd = command(app, stack, &stack.args(&["ps", "-a", "--format", "json"])).await?;
    Ok(parse_ps(&process::output(cmd, process::QUICK).await?))
}

fn find(app: &AppHandle, id: &str) -> Result<Stack> {
    app.state::<AppState>()
        .settings()
        .compose_stacks
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| AppError::msg("compose stack not found"))
}

/// Start every autostart stack once the engine is reachable (the provider
/// may still be starting, e.g. after a login launch).
pub fn spawn_autostart(app: &AppHandle) {
    let stacks: Vec<Stack> = app
        .state::<AppState>()
        .settings()
        .compose_stacks
        .into_iter()
        .filter(|s| s.autostart)
        .collect();
    if stacks.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let sink = log_sink(&app, "compose");
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let up = matches!(app.state::<AppState>().status().await, Ok(s) if s.engine_version.is_some() && s.busy.is_none());
            if up {
                break;
            }
            if Instant::now() > deadline {
                sink("Compose autostart: the engine didn't become reachable within 10 minutes; skipped.");
                return;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        for stack in stacks {
            if let Err(e) = run(&app, &stack, stack.up_args(), sink.clone()).await {
                sink(&format!("[{}] autostart failed: {e}", stack.name));
            }
        }
        let _ = tauri::Emitter::emit(&app, "compose", ());
    });
}

/// On app exit: stop or take down registered stacks that are running,
/// according to each stack's `on_quit`. Bounded so quitting can't hang.
pub async fn shutdown(app: &AppHandle) {
    let stacks: Vec<Stack> = app
        .state::<AppState>()
        .settings()
        .compose_stacks
        .into_iter()
        .filter(|s| s.on_quit != OnQuit::Leave)
        .collect();
    if stacks.is_empty() {
        return;
    }
    let sink: LineSink = std::sync::Arc::new(|_: &str| {});
    let work = async {
        for stack in &stacks {
            let running = services(app, stack)
                .await
                .map(|s| {
                    s.iter()
                        .any(|s| s.state == "running" || s.state == "restarting")
                })
                .unwrap_or(false);
            if !running {
                continue;
            }
            let sub = match stack.on_quit {
                OnQuit::Down => "down",
                _ => "stop",
            };
            let _ = run(app, stack, stack.args(&[sub]), sink.clone()).await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(120), work).await;
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackView {
    #[serde(flatten)]
    stack: Stack,
    command: String,
}

#[tauri::command]
pub fn compose_list(app: AppHandle) -> Vec<StackView> {
    app.state::<AppState>()
        .settings()
        .compose_stacks
        .into_iter()
        .map(|stack| StackView {
            command: stack.command_line(),
            stack,
        })
        .collect()
}

#[tauri::command]
pub fn compose_preview(stack: Stack) -> String {
    stack.command_line()
}

/// Insert or update (by id) a stack.
#[tauri::command]
pub fn compose_save(app: AppHandle, mut stack: Stack) -> Result<Vec<StackView>> {
    stack.validate()?;
    if stack.id.is_empty() {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        stack.id = format!("stack-{ms}");
    }
    app.state::<AppState>().update_settings(|s| {
        match s.compose_stacks.iter_mut().find(|x| x.id == stack.id) {
            Some(existing) => *existing = stack,
            None => s.compose_stacks.push(stack),
        }
    })?;
    Ok(compose_list(app))
}

#[tauri::command]
pub fn compose_delete(app: AppHandle, id: String) -> Result<Vec<StackView>> {
    app.state::<AppState>()
        .update_settings(|s| s.compose_stacks.retain(|x| x.id != id))?;
    Ok(compose_list(app))
}

#[tauri::command]
pub async fn compose_services(app: AppHandle, id: String) -> Result<Vec<Service>> {
    let stack = find(&app, &id)?;
    services(&app, &stack).await
}

/// `up` (detached), `stop`, `down` or `restart`.
#[tauri::command]
pub async fn compose_action(
    app: AppHandle,
    id: String,
    action: String,
    task_id: String,
) -> Result<()> {
    let stack = find(&app, &id)?;
    let args = match action.as_str() {
        "up" => stack.up_args(),
        "stop" => stack.args(&["stop"]),
        "down" => stack.args(&["down"]),
        "restart" => stack.args(&["restart"]),
        "pull" => stack.args(&["pull"]),
        other => return Err(AppError::msg(format!("unknown compose action `{other}`"))),
    };
    let sink = log_sink(&app, &task_id);
    let result = run(&app, &stack, args, sink.clone()).await;
    if let Err(e) = &result {
        sink(&format!("[{}] {action} failed: {e}", stack.name));
    }
    if action == "up" {
        let mut entry =
            crate::history::Entry::new(crate::history::Kind::Compose, stack.command_line())
                .outcome(&result);
        entry.container = Some(stack.id.clone());
        crate::history::record(&app, entry);
    }
    let _ = tauri::Emitter::emit(&app, "compose", ());
    result
}

/// Follow the stack's logs in a terminal window.
#[tauri::command]
pub fn compose_logs(app: AppHandle, id: String) -> Result<()> {
    let stack = find(&app, &id)?;
    let target = crate::shell::ShellTarget::Command {
        title: format!("{} logs", stack.name),
        args: stack.args(&["logs", "-f", "--tail", "300"]),
        cwd: Some(stack.directory.clone()),
        env: stack.environment(),
    };
    crate::shell::open(&app, target, None)
}

#[tauri::command]
pub fn compose_host_ids() -> Option<(String, String)> {
    host_ids()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Stack {
        Stack {
            name: "eg".into(),
            directory: "/tmp".into(),
            files: vec![
                "docker-compose.yml".into(),
                "docker-compose/colima.yml".into(),
            ],
            up_args: vec!["--build".into()],
            env: vec!["DOCKER_BUILDKIT=1".into()],
            ..Default::default()
        }
    }

    #[test]
    fn args_and_command_line() {
        let s = stack();
        assert_eq!(
            s.up_args().join(" "),
            "compose -f docker-compose.yml -f docker-compose/colima.yml up -d --build"
        );
        assert_eq!(s.args(&["stop"]).last().unwrap(), "stop");
        assert_eq!(
            s.command_line(),
            "DOCKER_BUILDKIT=1 docker compose -f docker-compose.yml -f docker-compose/colima.yml up -d --build"
        );
        let mut ids = s.clone();
        ids.host_ids = true;
        assert!(ids.command_line().starts_with("UID="));
    }

    #[test]
    fn parses_ps_both_formats() {
        let lines = "{\"Service\":\"db\",\"Name\":\"x-db-1\",\"State\":\"running\",\"Status\":\"Up 2 minutes\",\"Health\":\"\"}\n{\"Service\":\"redis\",\"State\":\"exited\"}";
        let a = parse_ps(lines);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].state, "running");
        let arr = "[{\"Service\":\"db\",\"State\":\"running\"}]";
        assert_eq!(parse_ps(arr).len(), 1);
        assert!(parse_ps("").is_empty());
    }
}

/// The user's shell aliases (`name -> expansion`), read once from an
/// interactive shell, so a pasted `dcc -f … up` can be understood.
#[cfg(unix)]
fn aliases() -> std::collections::HashMap<String, String> {
    use std::sync::OnceLock;
    static ALIASES: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    ALIASES
        .get_or_init(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let out = std::process::Command::new(shell)
                .args(["-ic", "alias"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            let text = out
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            parse_aliases(&text)
        })
        .clone()
}

#[cfg(not(unix))]
fn aliases() -> std::collections::HashMap<String, String> {
    Default::default()
}

/// `alias` output: zsh prints `name=value` or `name='value'`, bash
/// `alias name='value'`.
fn parse_aliases(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("alias ").unwrap_or(l.trim());
            let (name, value) = l.split_once('=')?;
            let value = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .map(|v| v.replace(r"'\''", "'"))
                .unwrap_or_else(|| value.to_string());
            Some((name.trim().to_string(), value))
        })
        .collect()
}

/// Expand aliases in the command word of `text` (after any leading
/// `KEY=VALUE` assignments), repeatedly, as the shell would.
fn expand_with(text: &str, aliases: &std::collections::HashMap<String, String>) -> String {
    let mut line = text.trim().to_string();
    for _ in 0..8 {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some(i) = words
            .iter()
            .position(|w| !(w.contains('=') && !w.starts_with('-')))
        else {
            break;
        };
        let Some(exp) = aliases.get(words[i]) else {
            break;
        };
        let mut next: Vec<String> = words[..i].iter().map(|s| s.to_string()).collect();
        next.push(exp.clone());
        next.extend(words[i + 1..].iter().map(|s| s.to_string()));
        let joined = next.join(" ");
        if joined == line {
            break;
        }
        line = joined;
    }
    line
}

#[tauri::command]
pub fn compose_expand(text: String) -> String {
    expand_with(&text, &aliases())
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    #[test]
    fn expands_nested_aliases() {
        let a = parse_aliases(
            "dc='COMPOSE_DOCKER_CLI_BUILD=1 DOCKER_BUILDKIT=1 docker-compose'\ndcc='UID=$UID GID=$GID dc'\nll='ls -l'",
        );
        assert_eq!(
            expand_with("dcc -f docker-compose.yml -f docker-compose/colima.yml up", &a),
            "UID=$UID GID=$GID COMPOSE_DOCKER_CLI_BUILD=1 DOCKER_BUILDKIT=1 docker-compose -f docker-compose.yml -f docker-compose/colima.yml up"
        );
        assert_eq!(expand_with("docker compose up", &a), "docker compose up");
        let bash = parse_aliases("alias dc='docker compose'");
        assert_eq!(bash["dc"], "docker compose");
    }
}
