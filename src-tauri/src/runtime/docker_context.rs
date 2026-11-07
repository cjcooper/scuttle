use async_trait::async_trait;
use serde::Deserialize;

use super::{not_supported, Profile, Provider};
use crate::engine::EngineTarget;
use crate::error::Result;
use crate::process::{self, LineSink};

/// Any engine reachable through a `docker context`: Docker Desktop, OrbStack,
/// Rancher Desktop (moby), remote hosts, ... Lifecycle is owned by the other
/// app, so this provider only targets the engine.
pub struct DockerContext;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContextEntry {
    name: String,
    #[serde(default)]
    current: bool,
    #[serde(default)]
    docker_endpoint: String,
    #[serde(default)]
    description: String,
}

#[async_trait]
impl Provider for DockerContext {
    fn id(&self) -> &'static str {
        "docker-context"
    }
    fn name(&self) -> &'static str {
        "Docker Context"
    }
    fn manages_vm(&self) -> bool {
        false
    }
    fn is_installed(&self) -> bool {
        process::resolve("docker").is_some()
    }

    async fn profiles(&self) -> Result<Vec<Profile>> {
        let mut c = process::command("docker")?;
        c.env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .args(["context", "ls", "--format", "{{json .}}"]);
        let out = process::output(c, process::QUICK).await?;
        out.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let e: ContextEntry = serde_json::from_str(l)?;
                let mut p = Profile::named(e.name);
                p.is_default = e.current;
                p.detail = Some(if e.description.is_empty() {
                    e.docker_endpoint
                } else {
                    format!("{} — {}", e.description, e.docker_endpoint)
                });
                Ok(p)
            })
            .collect()
    }

    async fn engine_target(&self, profile: &str) -> Result<EngineTarget> {
        Ok(EngineTarget {
            host: None,
            context: Some(profile.to_string()),
        })
    }

    async fn start(&self, _profile: &str, _sink: LineSink) -> Result<()> {
        Err(not_supported(self.name(), "starting the engine"))
    }

    async fn stop(&self, _profile: &str, _sink: LineSink) -> Result<()> {
        Err(not_supported(self.name(), "stopping the engine"))
    }
}
