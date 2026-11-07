//! Runtime providers: the thing that owns the VM / daemon that hosts the
//! container engine. Each provider lists "profiles" (Colima profiles, Podman
//! machines, Docker contexts, ...), can optionally start/stop them, and tells
//! the engine layer how to reach the Docker API for a profile.

mod colima;
mod docker_context;
mod podman;

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::engine::EngineTarget;
use crate::error::Result;
use crate::process::LineSink;

pub use colima::Colima;
pub use docker_context::DockerContext;
pub use podman::PodmanMachine;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Running,
    Stopped,
    /// The provider does not manage a VM, or the state could not be determined.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub name: String,
    pub state: VmState,
    pub arch: Option<String>,
    pub cpus: Option<u64>,
    /// Bytes.
    pub memory: Option<u64>,
    /// Bytes.
    pub disk: Option<u64>,
    pub runtime: Option<String>,
    /// Free-form extra info (e.g. a context's endpoint).
    pub detail: Option<String>,
    /// Whether this is the provider's default/current profile.
    pub is_default: bool,
}

impl Profile {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: VmState::Unknown,
            arch: None,
            cpus: None,
            memory: None,
            disk: None,
            runtime: None,
            detail: None,
            is_default: false,
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable id used in settings and IPC, e.g. `"colima"`.
    fn id(&self) -> &'static str;
    /// Human-readable name.
    fn name(&self) -> &'static str;
    /// Whether this provider can start/stop the engine host.
    fn manages_vm(&self) -> bool;
    /// Whether the provider's CLI is installed.
    fn is_installed(&self) -> bool;

    async fn profiles(&self) -> Result<Vec<Profile>>;
    async fn engine_target(&self, profile: &str) -> Result<EngineTarget>;

    async fn start(&self, profile: &str, sink: LineSink) -> Result<()>;
    async fn stop(&self, profile: &str, sink: LineSink) -> Result<()>;
    async fn restart(&self, profile: &str, sink: LineSink) -> Result<()> {
        self.stop(profile, sink.clone()).await?;
        self.start(profile, sink).await
    }

    async fn profile(&self, name: &str) -> Result<Option<Profile>> {
        Ok(self.profiles().await?.into_iter().find(|p| p.name == name))
    }
}

/// All providers this build knows about, in preference order for the
/// current OS. Uninstalled providers are kept so the UI can explain why
/// they are unavailable.
pub fn all() -> Vec<Arc<dyn Provider>> {
    let mut v: Vec<Arc<dyn Provider>> = Vec::new();
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        v.push(Arc::new(Colima));
    }
    v.push(Arc::new(PodmanMachine));
    v.push(Arc::new(DockerContext));
    v
}

pub(crate) fn not_supported(provider: &str, what: &str) -> crate::error::AppError {
    crate::error::AppError::msg(format!(
        "{provider} does not support {what} from here; manage it from its own app"
    ))
}
