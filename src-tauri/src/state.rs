use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::engine::{Docker, RunRequest};
use crate::error::{AppError, Result};
use crate::runtime::{self, Provider, VmState};

/// Which provider/profile the app is controlling. Persisted between launches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub provider: String,
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub provider_id: String,
    pub provider_name: String,
    pub profile: String,
    pub manages_vm: bool,
    pub vm_state: VmState,
    /// Engine server version when the Docker API is reachable.
    pub engine_version: Option<String>,
    pub engine_error: Option<String>,
    /// Label of a lifecycle operation in progress ("Starting", "Stopping", ...).
    pub busy: Option<String>,
}

/// How "Shell" actions open a shell. Persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    /// `"auto"` or a terminal id from `terminal::available()`.
    pub terminal: String,
    /// `""` = container/image default user, `"root"`, `"host"` (your uid:gid)
    /// or `"custom"` (uses `custom_user`).
    pub shell_user: String,
    pub custom_user: String,
    /// `"auto"` (bash if present, else sh) or a path such as `/bin/bash`.
    pub shell: String,
    /// When opened at login, start the selected provider/profile.
    pub start_provider_at_login: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            terminal: "auto".into(),
            shell_user: String::new(),
            custom_user: String::new(),
            shell: "auto".into(),
            start_provider_at_login: true,
        }
    }
}

impl Preferences {
    /// The user spec understood by `engine::resolve_user`.
    pub fn user_spec(&self) -> String {
        match self.shell_user.as_str() {
            "custom" => self.custom_user.trim().to_string(),
            other => other.to_string(),
        }
    }
}

/// A saved `docker run` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPreset {
    pub name: String,
    pub request: RunRequest,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub selection: Option<Selection>,
    pub preferences: Preferences,
    pub presets: Vec<RunPreset>,
    /// Last AWS profile used per ECR registry host.
    pub ecr_profiles: std::collections::BTreeMap<String, String>,
    /// Registered Docker Compose stacks.
    pub compose_stacks: Vec<crate::compose::Stack>,
}

pub struct AppState {
    pub providers: Vec<Arc<dyn Provider>>,
    settings: Mutex<Settings>,
    busy: Mutex<Option<String>>,
    settings_path: Option<PathBuf>,
}

impl AppState {
    pub fn new(settings_path: Option<PathBuf>) -> Self {
        let providers = runtime::all();
        let raw = settings_path.as_ref().and_then(|p| std::fs::read(p).ok());
        let mut settings: Settings = raw
            .as_deref()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default();
        if settings.selection.is_none() {
            // v0.1 stored a bare Selection.
            settings.selection = raw.as_deref().and_then(|b| serde_json::from_slice(b).ok());
        }
        settings.selection = settings
            .selection
            .filter(|s| providers.iter().any(|p| p.id() == s.provider))
            .or_else(|| Some(default_selection(&providers)));
        Self {
            providers,
            settings: Mutex::new(settings),
            busy: Mutex::new(None),
            settings_path,
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn selection(&self) -> Selection {
        self.settings()
            .selection
            .expect("selection initialised in new()")
    }

    pub fn preferences(&self) -> Preferences {
        self.settings().preferences
    }

    /// Mutate and persist settings.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut guard = self.settings.lock().unwrap();
        let mut next = guard.clone();
        f(&mut next);
        if let Some(path) = &self.settings_path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, serde_json::to_vec_pretty(&next)?)?;
        }
        *guard = next;
        Ok(())
    }

    pub fn set_selection(&self, sel: Selection) -> Result<()> {
        self.provider(&sel.provider)?;
        self.update_settings(|s| s.selection = Some(sel))
    }

    pub fn provider(&self, id: &str) -> Result<Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| p.id() == id)
            .cloned()
            .ok_or_else(|| AppError::msg(format!("unknown provider `{id}`")))
    }

    pub fn current(&self) -> Result<(Arc<dyn Provider>, String)> {
        let sel = self.selection();
        Ok((self.provider(&sel.provider)?, sel.profile))
    }

    pub async fn docker(&self) -> Result<Docker> {
        let (provider, profile) = self.current()?;
        Ok(Docker::new(provider.engine_target(&profile).await?))
    }

    pub fn busy(&self) -> Option<String> {
        self.busy.lock().unwrap().clone()
    }

    /// Marks a lifecycle operation as running. Returns an error if one is
    /// already in flight.
    pub fn begin(&self, label: &str) -> Result<BusyGuard<'_>> {
        let mut b = self.busy.lock().unwrap();
        if let Some(cur) = b.as_ref() {
            return Err(AppError::msg(format!("{cur} is already in progress")));
        }
        *b = Some(label.to_string());
        Ok(BusyGuard(self))
    }

    pub async fn status(&self) -> Result<Status> {
        let (provider, profile) = self.current()?;
        let mut status = Status {
            provider_id: provider.id().into(),
            provider_name: provider.name().into(),
            profile: profile.clone(),
            manages_vm: provider.manages_vm(),
            vm_state: VmState::Unknown,
            engine_version: None,
            engine_error: None,
            busy: self.busy(),
        };
        if !provider.is_installed() {
            status.engine_error = Some(format!("{} is not installed", provider.name()));
            return Ok(status);
        }
        if provider.manages_vm() {
            status.vm_state = match provider.profile(&profile).await {
                Ok(Some(p)) => p.state,
                Ok(None) => {
                    status.engine_error = Some(format!("profile `{profile}` not found"));
                    return Ok(status);
                }
                Err(e) => {
                    status.engine_error = Some(e.to_string());
                    return Ok(status);
                }
            };
            if status.vm_state == VmState::Stopped {
                return Ok(status);
            }
        }
        let version = match provider.engine_target(&profile).await {
            Ok(t) => Docker::new(t).server_version().await,
            Err(e) => Err(e),
        };
        match version {
            Ok(v) => {
                status.engine_version = Some(v);
                if !provider.manages_vm() {
                    status.vm_state = VmState::Running;
                }
            }
            Err(e) => status.engine_error = Some(e.to_string()),
        }
        Ok(status)
    }
}

pub struct BusyGuard<'a>(&'a AppState);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *self.0.busy.lock().unwrap() = None;
    }
}

fn default_selection(providers: &[Arc<dyn Provider>]) -> Selection {
    let provider = providers
        .iter()
        .find(|p| p.is_installed())
        .or_else(|| providers.last())
        .expect("at least one provider");
    let profile = match provider.id() {
        "colima" => "default",
        "podman" => "podman-machine-default",
        _ => "default",
    };
    Selection {
        provider: provider.id().into(),
        profile: profile.into(),
    }
}
