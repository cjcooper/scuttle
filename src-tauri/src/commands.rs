//! IPC commands for the dashboard, plus the shared actions the tray menu uses.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::aws::{self, EcrRegistry};
use crate::colima_config;
use crate::engine::{
    self, BuildRequest, ContainerAction, ContainerInfo, DiskUsage, ImageInfo, RunRequest,
};
use crate::error::Result;
use crate::process::LineSink;
use crate::runtime::Profile;
use crate::runtime::Provider as _;
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

/// A docker client for pulling/running `image`. When docker routes the
/// image's registry to `docker-credential-ecr-login`, docker runs with
/// `AWS_PROFILE` (the given profile, else the one last used for the
/// registry), and, if the image will be pulled, the helper is checked first
/// so an expired SSO session is reported instead of docker's "no basic auth
/// credentials". The profile is remembered for the registry.
pub async fn docker_for_image(
    app: &AppHandle,
    image: &str,
    aws_profile: Option<&str>,
    pull: bool,
    sink: &LineSink,
) -> Result<engine::Docker> {
    let state = app.state::<AppState>();
    let docker = state.docker().await?;
    let Some(registry) = aws::ecr_registry(image) else {
        return Ok(docker);
    };
    let profile = aws_profile
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .or_else(|| state.settings().ecr_profiles.get(&registry.host).cloned());
    let Some(profile) = profile else {
        // No profile chosen: docker runs with the default AWS credentials.
        return Ok(docker);
    };
    let docker = docker.with_aws_profile(Some(&profile));
    if pull || !docker.image_exists(image).await {
        aws::check_credentials(&registry, &profile, sink).await?;
    }
    let _ = state.update_settings(|s| {
        s.ecr_profiles
            .insert(registry.host.clone(), profile.clone());
    });
    Ok(docker)
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
    let result = match docker_for_image(&app, &image, aws_profile.as_deref(), true, &sink).await {
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
    /// The error prefix for expired/invalid credentials (see aws.rs).
    sign_in_required: &'static str,
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
        sign_in_required: aws::SIGN_IN_REQUIRED,
        profiles,
        profile,
    })
}

#[tauri::command]
pub async fn aws_sso_login(app: AppHandle, profile: String, task_id: String) -> Result<()> {
    aws::sso_login(&profile, log_sink(&app, &task_id)).await
}

// ------------------------------------------------------------ colima config

/// Run a colima command for `profile` as a lifecycle operation (busy state,
/// streamed log, status refresh).
async fn colima_lifecycle(
    app: &AppHandle,
    label: &str,
    args: Vec<String>,
    task_id: &str,
) -> Result<()> {
    let state = app.state::<AppState>();
    let guard = state.begin(label)?;
    let _ = refresh_status(app).await;
    let sink = log_sink(app, task_id);
    sink(&format!("$ colima {}", args.join(" ")));
    let result = async {
        let mut cmd = crate::process::command("colima")?;
        cmd.args(&args);
        crate::process::stream(cmd, sink.clone()).await
    }
    .await;
    match &result {
        Ok(()) => sink("Done."),
        Err(e) => sink(&format!("Failed: {e}")),
    }
    drop(guard);
    let _ = refresh_status(app).await;
    result
}

#[tauri::command]
pub fn colima_config_get(profile: String) -> Result<colima_config::ConfigFile> {
    colima_config::read(&profile)
}

#[tauri::command]
pub fn colima_config_defaults() -> colima_config::ColimaConfig {
    colima_config::ColimaConfig::default()
}

#[tauri::command]
pub fn colima_config_save(
    profile: String,
    config: colima_config::ColimaConfig,
) -> Result<colima_config::ConfigFile> {
    colima_config::write(&profile, &config)?;
    colima_config::read(&profile)
}

#[tauri::command]
pub fn colima_config_save_raw(profile: String, raw: String) -> Result<colima_config::ConfigFile> {
    colima_config::write_raw(&profile, &raw)?;
    colima_config::read(&profile)
}

/// Start (if stopped) or restart (if running) a profile so saved settings
/// take effect.
#[tauri::command]
pub async fn colima_apply(app: AppHandle, profile: String, task_id: String) -> Result<()> {
    let running = crate::runtime::Colima
        .profile(&profile)
        .await?
        .is_some_and(|p| p.state == crate::runtime::VmState::Running);
    let verb = if running { "restart" } else { "start" };
    let label = if running { "Restarting" } else { "Starting" };
    colima_lifecycle(
        &app,
        label,
        vec![verb.into(), "--profile".into(), profile],
        &task_id,
    )
    .await
}

#[tauri::command]
pub async fn colima_create(
    app: AppHandle,
    profile: String,
    config: colima_config::ColimaConfig,
    task_id: String,
) -> Result<()> {
    if crate::runtime::Colima.profile(&profile).await?.is_some() {
        return Err(crate::error::AppError::msg(format!(
            "profile `{profile}` already exists"
        )));
    }
    let args = colima_config::create_args(&profile, &config)?;
    colima_lifecycle(&app, "Creating", args, &task_id).await?;
    // Daemon config and provision scripts have no flags: save them to the
    // file Colima just wrote, then restart to apply.
    if colima_config::needs_file_settings(&config) {
        let mut saved = colima_config::read(&profile)?.config;
        saved.docker = config.docker.clone();
        saved.provision = config.provision.clone();
        colima_config::write(&profile, &saved)?;
        let args = vec!["restart".into(), "--profile".into(), profile];
        colima_lifecycle(&app, "Restarting", args, &task_id).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn colima_delete(
    app: AppHandle,
    profile: String,
    delete_data: bool,
    task_id: String,
) -> Result<()> {
    colima_config::config_path(&profile)?; // validates the name
    let mut args = vec![
        "delete".into(),
        "--profile".into(),
        profile,
        "--force".into(),
    ];
    if delete_data {
        args.push("--data".into());
    }
    colima_lifecycle(&app, "Deleting", args, &task_id).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    cpus: usize,
    /// Bytes, when known.
    memory: Option<u64>,
}

/// Host CPU count and memory, for sizing VMs.
#[tauri::command]
pub async fn host_info() -> HostInfo {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let memory = if cfg!(target_os = "macos") {
        let mut c = tokio::process::Command::new("/usr/sbin/sysctl");
        c.args(["-n", "hw.memsize"]);
        crate::process::output(c, crate::process::QUICK)
            .await
            .ok()
            .and_then(|s| s.trim().parse().ok())
    } else {
        std::fs::read_to_string("/proc/meminfo").ok().and_then(|m| {
            let kb: u64 = m
                .lines()
                .find(|l| l.starts_with("MemTotal:"))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some(kb * 1024)
        })
    };
    HostInfo { cpus, memory }
}

// ------------------------------------------------------------- colima data

/// Disk space actually used under `path` (sparse VM disks count as used
/// blocks, not their nominal size).
async fn disk_used(path: &std::path::Path) -> Option<u64> {
    if !path.exists() {
        return Some(0);
    }
    let mut c = tokio::process::Command::new("du");
    c.arg("-sk").arg(path);
    let out = crate::process::output(c, std::time::Duration::from_secs(60))
        .await
        .ok()?;
    out.split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUsage {
    name: String,
    state: crate::runtime::VmState,
    vm_bytes: Option<u64>,
    data_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColimaUsage {
    profiles: Vec<ProfileUsage>,
    cache_bytes: Option<u64>,
    cache_path: Option<String>,
}

#[tauri::command]
pub async fn colima_data_usage() -> Result<ColimaUsage> {
    let mut profiles = Vec::new();
    for p in crate::runtime::Colima.profiles().await? {
        let (vm, data) = colima_config::profile_paths(&p.name)?;
        profiles.push(ProfileUsage {
            vm_bytes: disk_used(&vm).await,
            data_bytes: disk_used(&data).await,
            state: p.state,
            name: p.name,
        });
    }
    let cache = colima_config::cache_dir();
    let cache_bytes = match &cache {
        Some(c) => disk_used(c).await,
        None => None,
    };
    Ok(ColimaUsage {
        profiles,
        cache_bytes,
        cache_path: cache.map(|c| c.display().to_string()),
    })
}

/// `colima delete --data` for every profile, then optionally `colima prune
/// --all` to clear downloaded images.
#[tauri::command]
pub async fn colima_delete_all(app: AppHandle, clear_cache: bool, task_id: String) -> Result<()> {
    let profiles = crate::runtime::Colima.profiles().await?;
    let sink = log_sink(&app, &task_id);
    sink(&format!(
        "Deleting {} Colima profile(s) and their data: {}",
        profiles.len(),
        profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let mut failed = Vec::new();
    for p in profiles {
        let args = vec![
            "delete".into(),
            "--profile".into(),
            p.name.clone(),
            "--force".into(),
            "--data".into(),
        ];
        if colima_lifecycle(&app, "Deleting", args, &task_id)
            .await
            .is_err()
        {
            failed.push(p.name);
        }
    }
    if clear_cache {
        let args = vec!["prune".into(), "--all".into(), "--force".into()];
        if colima_lifecycle(&app, "Clearing cache", args, &task_id)
            .await
            .is_err()
        {
            failed.push("download cache".into());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(crate::error::AppError::msg(format!(
            "could not delete: {}",
            failed.join(", ")
        )))
    }
}

/// `colima prune --all`: clear cached downloads (VM images).
#[tauri::command]
pub async fn colima_prune(app: AppHandle, task_id: String) -> Result<()> {
    let args = vec!["prune".into(), "--all".into(), "--force".into()];
    colima_lifecycle(&app, "Clearing cache", args, &task_id).await
}
