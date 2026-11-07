use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

use super::{Profile, Provider, VmState};
use crate::engine::EngineTarget;
use crate::error::{AppError, Result};
use crate::process::{self, LineSink};

/// [Colima](https://github.com/abiosoft/colima) on macOS / Linux.
pub struct Colima;

#[derive(Deserialize)]
struct ListEntry {
    name: String,
    status: String,
    arch: Option<String>,
    cpus: Option<u64>,
    memory: Option<u64>,
    disk: Option<u64>,
    runtime: Option<String>,
}

#[derive(Deserialize)]
struct StatusJson {
    runtime: Option<String>,
    docker_socket: Option<String>,
}

fn cmd(args: &[&str]) -> Result<tokio::process::Command> {
    let mut c = process::command("colima")?;
    c.args(args);
    Ok(c)
}

#[async_trait]
impl Provider for Colima {
    fn id(&self) -> &'static str {
        "colima"
    }
    fn name(&self) -> &'static str {
        "Colima"
    }
    fn manages_vm(&self) -> bool {
        true
    }
    fn is_installed(&self) -> bool {
        process::resolve("colima").is_some()
    }

    async fn profiles(&self) -> Result<Vec<Profile>> {
        let out = process::output(cmd(&["list", "--json"])?, process::QUICK).await?;
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let e: ListEntry = serde_json::from_str(l)?;
                Ok(Profile {
                    is_default: e.name == "default",
                    state: match e.status.as_str() {
                        "Running" => VmState::Running,
                        "Stopped" => VmState::Stopped,
                        _ => VmState::Unknown,
                    },
                    detail: (e.status != "Running" && e.status != "Stopped")
                        .then(|| e.status.clone()),
                    name: e.name,
                    arch: e.arch,
                    cpus: e.cpus,
                    memory: e.memory,
                    disk: e.disk,
                    runtime: e.runtime,
                })
            })
            .collect()
    }

    async fn engine_target(&self, profile: &str) -> Result<EngineTarget> {
        let out = process::output(
            cmd(&["status", "--profile", profile, "--json"])?,
            Duration::from_secs(10),
        )
        .await
        .map_err(|_| AppError::msg(format!("Colima profile `{profile}` is not running")))?;
        let s: StatusJson = serde_json::from_str(out.trim())?;
        match (s.runtime.as_deref(), s.docker_socket) {
            (Some("docker"), Some(sock)) if !sock.is_empty() => Ok(EngineTarget {
                host: Some(sock),
                context: None,
            }),
            (rt, _) => Err(AppError::msg(format!(
                "Colima profile `{profile}` uses the `{}` runtime; only the docker runtime is supported",
                rt.unwrap_or("unknown")
            ))),
        }
    }

    async fn start(&self, profile: &str, sink: LineSink) -> Result<()> {
        process::stream(cmd(&["start", "--profile", profile])?, sink).await
    }

    async fn stop(&self, profile: &str, sink: LineSink) -> Result<()> {
        process::stream(cmd(&["stop", "--profile", profile])?, sink).await
    }

    async fn restart(&self, profile: &str, sink: LineSink) -> Result<()> {
        process::stream(cmd(&["restart", "--profile", profile])?, sink).await
    }
}
