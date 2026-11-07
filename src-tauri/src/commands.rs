//! IPC commands for the dashboard, plus the shared actions the tray menu uses.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::aws::{self, EcrRegistry};
use crate::engine::{
    self, BuildRequest, ContainerAction, ContainerInfo, DiskUsage, ImageInfo, RunRequest,
};
use crate::error::Result;
use crate::process::LineSink;
use crate::runtime::Profile;
use crate::shell::{self, ShellOptions, ShellTarget};
use crate::state::{AppState, Preferences, RunPreset, Selection, Settings, Status};
use crate::terminal::{self, TerminalApp};
use crate::tray;

pub const EVENT_STATUS: &str = "status";
pub const EVENT_LOG: &str = "task-log";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogLine<'a> {
    task_id: &'a str,
    line: &'a str,
}

/// A sink that forwards process output to the UI as `task-log` events.
pub fn log_sink(app: &AppHandle, task_id: &str) -> LineSink {
    let app = app.clone();
    let task_id = task_id.to_string();
    Arc::new(move |line: &str| {
        let _ = app.emit(
            EVENT_LOG,
            LogLine {
                task_id: &task_id,
                line,
            },
        );
    })
}

/// Recompute status, push it to the tray and the UI.
pub async fn refresh_status(app: &AppHandle) -> Result<Status> {
    let status = app.state::<AppState>().status().await?;
    tray::update(app, &status);
    let _ = app.emit(EVENT_STATUS, &status);
    Ok(status)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VmAction {
    Start,
    Stop,
    Restart,
}

impl VmAction {
    fn label(self) -> &'static str {
        match self {
            VmAction::Start => "Starting",
            VmAction::Stop => "Stopping",
            VmAction::Restart => "Restarting",
        }
    }
}

pub async fn run_vm_action(app: &AppHandle, action: VmAction, task_id: &str) -> Result<()> {
    let state = app.state::<AppState>();
    let (provider, profile) = state.current()?;
    let guard = state.begin(action.label())?;
    let _ = refresh_status(app).await;

    let sink = log_sink(app, task_id);
    sink(&format!(
        "{} {} profile `{profile}`…",
        action.label(),
        provider.name()
    ));
    let result = match action {
        VmAction::Start => provider.start(&profile, sink.clone()).await,
        VmAction::Stop => provider.stop(&profile, sink.clone()).await,
        VmAction::Restart => provider.restart(&profile, sink.clone()).await,
    };
    match &result {
        Ok(()) => sink("Done."),
        Err(e) => sink(&format!("Failed: {e}")),
    }

    drop(guard);
    let _ = refresh_status(app).await;
    result
}

/// Run a prune from the tray, reporting into the activity log.
pub async fn run_prune(app: &AppHandle, what: Prune, task_id: &str) -> Result<String> {
    let sink = log_sink(app, task_id);
    let docker = app.state::<AppState>().docker().await?;
    sink(what.label());
    let result = match what {
        Prune::DanglingImages => docker.prune_images(false).await,
        Prune::UnusedImages => docker.prune_images(true).await,
        Prune::BuildCache => docker.prune_build_cache(false).await,
    };
    match &result {
        Ok(out) => out.lines().for_each(|l| sink(l)),
        Err(e) => sink(&format!("Failed: {e}")),
    }
    result
}

#[derive(Debug, Clone, Copy)]
pub enum Prune {
    DanglingImages,
    UnusedImages,
    BuildCache,
}

impl Prune {
    fn label(self) -> &'static str {
        match self {
            Prune::DanglingImages => "Removing dangling images…",
            Prune::UnusedImages => "Removing all unused images…",
            Prune::BuildCache => "Pruning build cache…",
        }
    }
}

/// A docker client for pulling/running `image`: when it is an ECR image and
/// an AWS profile is given, docker runs with `AWS_PROFILE` and is logged in
/// if needed. The profile is remembered for the registry.
pub async fn docker_for_image(
    app: &AppHandle,
    image: &str,
    aws_profile: Option<&str>,
    sink: &LineSink,
) -> Result<engine::Docker> {
    let state = app.state::<AppState>();
    let docker = state.docker().await?;
    let profile = aws_profile.map(str::trim).filter(|p| !p.is_empty());
    match (aws::ecr_registry(image), profile) {
        (Some(registry), Some(profile)) => {
            let docker = docker.with_aws_profile(Some(profile));
            aws::ensure_login(&docker, &registry, profile, sink).await?;
            let _ = state.update_settings(|s| {
                s.ecr_profiles
                    .insert(registry.host.clone(), profile.to_string());
            });
            Ok(docker)
        }
        _ => Ok(docker),
    }
}

/// Open an integrated shell window: `docker exec -it` into a running
/// container, as the saved "Shell as" user/shell.
pub fn open_container_shell(app: &AppHandle, container: &str) -> Result<()> {
    shell::open(
        app,
        ShellTarget::Exec {
            container: container.to_string(),
        },
        None,
    )
}

/// Run a container. Interactive requests open a shell window; detached ones
/// return the new container id.
pub async fn start_container(app: &AppHandle, request: &RunRequest) -> Result<Option<String>> {
    if request.interactive {
        let options = ShellOptions {
            user: request.user.clone(),
            shell: app.state::<AppState>().preferences().shell,
        };
        let target = ShellTarget::Run {
            request: request.clone(),
            shell_entrypoint: false,
        };
        shell::open(app, target, Some(options))?;
        Ok(None)
    } else {
        app.state::<AppState>()
            .docker()
            .await?
            .run_detached(request)
            .await
            .map(Some)
    }
}

pub async fn run_preset_by_index(app: &AppHandle, index: usize) -> Result<Option<String>> {
    let preset = app
        .state::<AppState>()
        .settings()
        .presets
        .get(index)
        .cloned()
        .ok_or_else(|| crate::error::AppError::msg("preset not found"))?;
    start_container(app, &preset.request).await
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    id: &'static str,
    name: &'static str,
    manages_vm: bool,
    installed: bool,
}

#[tauri::command]
pub fn list_providers(state: State<'_, AppState>) -> Vec<ProviderInfo> {
    state
        .providers
        .iter()
        .map(|p| ProviderInfo {
            id: p.id(),
            name: p.name(),
            manages_vm: p.manages_vm(),
            installed: p.is_installed(),
        })
        .collect()
}

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>, provider: String) -> Result<Vec<Profile>> {
    state.provider(&provider)?.profiles().await
}

#[tauri::command]
pub fn get_selection(state: State<'_, AppState>) -> Selection {
    state.selection()
}

#[tauri::command]
pub async fn set_selection(app: AppHandle, selection: Selection) -> Result<Status> {
    app.state::<AppState>().set_selection(selection)?;
    refresh_status(&app).await
}

#[tauri::command]
pub async fn get_status(app: AppHandle) -> Result<Status> {
    refresh_status(&app).await
}

#[tauri::command]
pub async fn vm_action(app: AppHandle, action: VmAction, task_id: String) -> Result<()> {
    run_vm_action(&app, action, &task_id).await
}

#[tauri::command]
pub async fn list_images(state: State<'_, AppState>) -> Result<Vec<ImageInfo>> {
    state.docker().await?.images().await
}

#[tauri::command]
pub async fn list_containers(state: State<'_, AppState>) -> Result<Vec<ContainerInfo>> {
    state.docker().await?.containers().await
}

#[tauri::command]
pub async fn disk_usage(state: State<'_, AppState>) -> Result<Vec<DiskUsage>> {
    state.docker().await?.disk_usage().await
}

#[tauri::command]
pub async fn remove_images(
    state: State<'_, AppState>,
    ids: Vec<String>,
    force: bool,
) -> Result<String> {
    state.docker().await?.remove_images(&ids, force).await
}

#[tauri::command]
pub async fn prune_images(app: AppHandle, all: bool, task_id: String) -> Result<String> {
    let what = if all {
        Prune::UnusedImages
    } else {
        Prune::DanglingImages
    };
    run_prune(&app, what, &task_id).await
}

#[tauri::command]
pub async fn prune_build_cache(app: AppHandle, task_id: String) -> Result<String> {
    run_prune(&app, Prune::BuildCache, &task_id).await
}

#[tauri::command]
pub async fn container_action(
    state: State<'_, AppState>,
    id: String,
    action: ContainerAction,
) -> Result<()> {
    state.docker().await?.container_action(&id, action).await
}

#[tauri::command]
pub async fn build_image(app: AppHandle, request: BuildRequest, task_id: String) -> Result<()> {
    let docker = app.state::<AppState>().docker().await?;
    let sink = log_sink(&app, &task_id);
    let result = docker.build(&request, sink.clone()).await;
    match &result {
        Ok(()) => sink("Build finished."),
        Err(e) => sink(&format!("Build failed: {e}")),
    }
    result
}

#[tauri::command]
pub async fn pull_image(
    app: AppHandle,
    image: String,
    aws_profile: Option<String>,
    task_id: String,
) -> Result<()> {
    let sink = log_sink(&app, &task_id);
    let result = match docker_for_image(&app, &image, aws_profile.as_deref(), &sink).await {
        Ok(docker) => docker.pull(image.trim(), sink.clone()).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &result {
        sink(&format!("Pull failed: {e}"));
    }
    result
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    terminals: Vec<TerminalApp>,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    SettingsView {
        settings: state.settings(),
        terminals: terminal::available(),
    }
}

#[tauri::command]
pub fn set_preferences(app: AppHandle, preferences: Preferences) -> Result<()> {
    app.state::<AppState>()
        .update_settings(|s| s.preferences = preferences)?;
    tray::rebuild(&app);
    Ok(())
}

/// Insert or replace (by name) a run preset.
#[tauri::command]
pub fn save_preset(app: AppHandle, preset: RunPreset) -> Result<Vec<RunPreset>> {
    let state = app.state::<AppState>();
    state.update_settings(
        |s| match s.presets.iter_mut().find(|p| p.name == preset.name) {
            Some(existing) => *existing = preset,
            None => s.presets.push(preset),
        },
    )?;
    tray::rebuild(&app);
    Ok(state.settings().presets)
}

#[tauri::command]
pub fn delete_preset(app: AppHandle, name: String) -> Result<Vec<RunPreset>> {
    let state = app.state::<AppState>();
    state.update_settings(|s| s.presets.retain(|p| p.name != name))?;
    tray::rebuild(&app);
    Ok(state.settings().presets)
}

/// The equivalent docker CLI command for a run request (for display / copy).
#[tauri::command]
pub fn preview_run(request: RunRequest) -> Result<String> {
    let args = engine::run_args(&request)?;
    let env = request
        .aws_profile
        .as_deref()
        .filter(|p| !p.trim().is_empty() && aws::ecr_registry(&request.image).is_some())
        .map(|p| format!("AWS_PROFILE={}", engine::shell_quote(p.trim())));
    Ok(env
        .into_iter()
        .chain(std::iter::once("docker".to_string()))
        .chain(args.iter().map(|a| engine::shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" "))
}

#[tauri::command]
pub async fn run_container(app: AppHandle, request: RunRequest) -> Result<Option<String>> {
    start_container(&app, &request).await
}

#[tauri::command]
pub fn open_shell(app: AppHandle, container: String) -> Result<()> {
    open_container_shell(&app, &container)
}

/// `docker run -it --rm --user <pref> --entrypoint <shell> IMAGE` in a shell window.
#[tauri::command]
pub fn image_shell(app: AppHandle, image: String) -> Result<()> {
    let request = RunRequest {
        image,
        remove: true,
        interactive: true,
        ..Default::default()
    };
    shell::open(
        &app,
        ShellTarget::Run {
            request,
            shell_entrypoint: true,
        },
        None,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcrInfo {
    registry: EcrRegistry,
    profiles: Vec<String>,
    /// Suggested profile: last used for this registry, else `AWS_PROFILE`,
    /// else `default`.
    profile: Option<String>,
}

/// AWS details for an image reference, or null when it isn't an ECR image.
#[tauri::command]
pub fn ecr_info(state: State<'_, AppState>, image: String) -> Option<EcrInfo> {
    let registry = aws::ecr_registry(&image)?;
    let profiles = aws::profiles();
    let remembered = state.settings().ecr_profiles.get(&registry.host).cloned();
    let profile = remembered
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .or_else(|| profiles.iter().find(|p| *p == "default").cloned())
        .or_else(|| profiles.first().cloned());
    Some(EcrInfo {
        registry,
        profiles,
        profile,
    })
}

#[tauri::command]
pub async fn aws_sso_login(app: AppHandle, profile: String, task_id: String) -> Result<()> {
    aws::sso_login(&profile, log_sink(&app, &task_id)).await
}
