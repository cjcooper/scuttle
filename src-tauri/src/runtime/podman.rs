use async_trait::async_trait;
use serde_json::Value;

use super::{Profile, Provider, VmState};
use crate::engine::EngineTarget;
use crate::error::{AppError, Result};
use crate::process::{self, LineSink};

/// `podman machine` (macOS / Windows / Linux). The Docker CLI talks to the
/// machine's Docker-compatible API socket.
pub struct PodmanMachine;

fn cmd(args: &[&str]) -> Result<tokio::process::Command> {
    let mut c = process::command("podman")?;
    c.args(args);
    Ok(c)
}

/// Podman has emitted sizes both as numbers and as numeric strings.
fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

#[async_trait]
impl Provider for PodmanMachine {
    fn id(&self) -> &'static str {
        "podman"
    }
    fn name(&self) -> &'static str {
        "Podman Machine"
    }
    fn manages_vm(&self) -> bool {
        true
    }
    fn is_installed(&self) -> bool {
        process::resolve("podman").is_some()
    }

    async fn profiles(&self) -> Result<Vec<Profile>> {
        let out = process::output(
            cmd(&["machine", "list", "--format", "json"])?,
            process::QUICK,
        )
        .await?;
        let list: Vec<Value> = serde_json::from_str(out.trim()).unwrap_or_default();
        Ok(list
            .iter()
            .map(|m| {
                let name = m["Name"].as_str().unwrap_or_default().trim_end_matches('*');
                let mut p = Profile::named(name);
                p.state = if m["Running"].as_bool().unwrap_or(false) {
                    VmState::Running
                } else {
                    VmState::Stopped
                };
                p.cpus = as_u64(&m["CPUs"]);
                p.memory = as_u64(&m["Memory"]);
                p.disk = as_u64(&m["DiskSize"]);
                p.detail = m["VMType"].as_str().map(str::to_string);
                p.is_default = m["Default"].as_bool().unwrap_or(false);
                p.runtime = Some("podman".into());
                p
            })
            .collect())
    }

    async fn engine_target(&self, profile: &str) -> Result<EngineTarget> {
        let out = process::output(cmd(&["machine", "inspect", profile])?, process::QUICK).await?;
        let v: Value = serde_json::from_str(out.trim())?;
        let path = v[0]["ConnectionInfo"]["PodmanSocket"]["Path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::msg(format!("no API socket reported for machine `{profile}`"))
            })?;
        let host = if cfg!(windows) {
            format!("npipe://{}", path.replace('\\', "/"))
        } else {
            format!("unix://{path}")
        };
        Ok(EngineTarget {
            host: Some(host),
            context: None,
        })
    }

    async fn start(&self, profile: &str, sink: LineSink) -> Result<()> {
        process::stream(cmd(&["machine", "start", profile])?, sink).await
    }

    async fn stop(&self, profile: &str, sink: LineSink) -> Result<()> {
        process::stream(cmd(&["machine", "stop", profile])?, sink).await
    }
}
