//! Menu bar (macOS) / system tray (Windows, Linux) icon and menu.
//!
//! The menu is rebuilt when its dynamic parts change (running containers,
//! presets, shell-user preference); status text is updated in place.

use std::sync::Mutex;
use std::time::Duration;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::commands::{self, Prune, VmAction};
use crate::runtime::VmState;
use crate::state::{AppState, Status};

const TRAY_ID: &str = "main";
const TRAY_TASK: &str = "tray";
const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Menu items whose text/enabled state changes with the runtime status.
struct TrayItems {
    status: MenuItem<Wry>,
    engine: MenuItem<Wry>,
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
    restart: MenuItem<Wry>,
    prune_dangling: MenuItem<Wry>,
    prune_cache: MenuItem<Wry>,
}

#[derive(Debug, Clone, PartialEq)]
struct StackSummary {
    id: String,
    name: String,
    running: usize,
    total: usize,
    /// Status unknown (engine down or compose failed).
    unknown: bool,
}

#[derive(Default)]
struct TrayState {
    items: Mutex<Option<TrayItems>>,
    /// (id, name) of running containers shown in the "Open Shell" submenu.
    containers: Mutex<Vec<(String, String)>>,
    last_status: Mutex<Option<Status>>,
    /// Compose stacks for the "Compose" submenu: (id, name, running, total).
    stacks: Mutex<Vec<StackSummary>>,
    /// The badge currently shown, so the icon is only replaced on change.
    indicator: Mutex<Option<Indicator>>,
    /// Whether the busy animation task is running.
    animating: std::sync::atomic::AtomicBool,
}

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    app.manage(TrayState::default());
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(Indicator::Error.png())?)
        .icon_as_template(true)
        .tooltip("Scuttle")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu(app, event.id.as_ref()))
        .build(app)?;
    Ok(())
}

/// Rebuild the menu after presets / preferences / containers change.
pub fn rebuild(app: &AppHandle) {
    let Ok(menu) = build_menu(app) else { return };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu));
    }
    let last = app.state::<TrayState>().last_status.lock().unwrap().clone();
    if let Some(status) = last {
        apply_status(app, &status);
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let item = |id: &str, text: &str, enabled: bool| {
        MenuItem::with_id(app, id, text, enabled, None::<&str>)
    };
    let sep = || PredefinedMenuItem::separator(app);
    let tray_state = app.state::<TrayState>();
    let settings = app.state::<AppState>().settings();
    let prefs = &settings.preferences;

    let items = TrayItems {
        status: item("status", "Checking…", false)?,
        engine: item("engine", " ", false)?,
        start: item("start", "Start", false)?,
        stop: item("stop", "Stop", false)?,
        restart: item("restart", "Restart", false)?,
        prune_dangling: item("prune-dangling", "Remove Dangling Images", false)?,
        prune_cache: item("prune-cache", "Prune Build Cache", false)?,
    };

    // Open Shell ▸ running containers
    let containers = tray_state.containers.lock().unwrap().clone();
    let shell_items: Vec<MenuItem<Wry>> = if containers.is_empty() {
        vec![item("none", "No running containers", false)?]
    } else {
        containers
            .iter()
            .map(|(id, name)| item(&format!("shell:{id}"), name, true))
            .collect::<tauri::Result<_>>()?
    };
    let shell_menu = Submenu::with_items(app, "Open Shell", true, &as_refs(&shell_items))?;

    // Shell User ▸ quick switch
    let current = prefs.shell_user.as_str();
    let mut user_items = vec![
        CheckMenuItem::with_id(
            app,
            "user:",
            "Image Default",
            true,
            current.is_empty(),
            None::<&str>,
        )?,
        CheckMenuItem::with_id(
            app,
            "user:root",
            "root",
            true,
            current == "root",
            None::<&str>,
        )?,
    ];
    if cfg!(unix) {
        user_items.push(CheckMenuItem::with_id(
            app,
            "user:host",
            "My User (uid:gid)",
            true,
            current == "host",
            None::<&str>,
        )?);
    }
    if !prefs.custom_user.trim().is_empty() {
        user_items.push(CheckMenuItem::with_id(
            app,
            "user:custom",
            prefs.custom_user.trim(),
            true,
            current == "custom",
            None::<&str>,
        )?);
    }
    let user_menu = Submenu::with_items(app, "Shell User", true, &as_refs(&user_items))?;

    // Run Preset ▸
    let preset_items: Vec<MenuItem<Wry>> = if settings.presets.is_empty() {
        vec![item(
            "none",
            "No presets — save one from Containers ▸ Run",
            false,
        )?]
    } else {
        settings
            .presets
            .iter()
            .enumerate()
            .map(|(i, p)| item(&format!("preset:{i}"), &p.name, true))
            .collect::<tauri::Result<_>>()?
    };
    let preset_menu = Submenu::with_items(app, "Run Preset", true, &as_refs(&preset_items))?;

    // Compose ▸ <stack> ▸ Up / Stop / Restart / Down… / Logs
    let summaries = tray_state.stacks.lock().unwrap().clone();
    let compose_menu = Submenu::with_id(app, "compose-menu", "Compose", true)?;
    if summaries.is_empty() {
        compose_menu.append(&item(
            "none",
            "No stacks — add one on the Compose page",
            false,
        )?)?;
    }
    for s in &summaries {
        let (mark, detail) = match (s.unknown, s.running, s.total) {
            (true, _, _) => ("○", "".to_string()),
            (_, _, 0) => ("○", " — not created".to_string()),
            (_, r, t) if r == t => ("●", format!(" — running ({t})")),
            (_, 0, _) => ("○", " — stopped".to_string()),
            (_, r, t) => ("◐", format!(" — {r}/{t} running")),
        };
        let all_up = !s.unknown && s.total > 0 && s.running == s.total;
        let any_up = !s.unknown && s.running > 0;
        let id = &s.id;
        let sub = Submenu::with_items(
            app,
            format!("{mark} {}{detail}", s.name),
            true,
            &[
                &item(&format!("compose:up:{id}"), "Up", !all_up)?,
                &item(&format!("compose:stop:{id}"), "Stop", any_up)?,
                &item(&format!("compose:restart:{id}"), "Restart", any_up)?,
                &item(
                    &format!("compose:down:{id}"),
                    "Down…",
                    !s.unknown && s.total > 0,
                )?,
                &sep()?,
                &item(&format!("compose:logs:{id}"), "Logs", !s.unknown)?,
            ],
        )?;
        compose_menu.append(&sub)?;
    }
    compose_menu.append(&sep()?)?;
    compose_menu.append(&item("compose-open", "Open Compose…", true)?)?;

    let menu = Menu::with_items(
        app,
        &[
            &items.status,
            &items.engine,
            &sep()?,
            &items.start,
            &items.stop,
            &items.restart,
            &sep()?,
            &shell_menu,
            &user_menu,
            &preset_menu,
            &compose_menu,
            &sep()?,
            &items.prune_dangling,
            &items.prune_cache,
            &sep()?,
            &item("open", "Open Dashboard…", true)?,
            &CheckMenuItem::with_id(
                app,
                "login",
                "Open at Login",
                true,
                crate::login::status(app).enabled,
                None::<&str>,
            )?,
            &item("about", "About Scuttle", true)?,
            &sep()?,
            &PredefinedMenuItem::quit(app, Some("Quit Scuttle"))?,
        ],
    )?;
    *tray_state.items.lock().unwrap() = Some(items);
    Ok(menu)
}

fn as_refs<T: IsMenuItem<Wry>>(v: &[T]) -> Vec<&dyn IsMenuItem<Wry>> {
    v.iter().map(|i| i as &dyn IsMenuItem<Wry>).collect()
}

fn handle_menu(app: &AppHandle, id: &str) {
    if id == "open" {
        crate::show_dashboard(app);
        return;
    }
    if id == "login" {
        let enable = !crate::login::status(app).enabled;
        if let Err(e) = crate::login::set_enabled(app, enable) {
            commands::log_sink(app, TRAY_TASK)(&e.to_string());
        }
        let _ = tauri::Emitter::emit(app, "login-item", crate::login::status(app));
        return;
    }
    if id == "about" {
        let _ = crate::about::open(app);
        return;
    }
    if id == "compose-open" {
        crate::show_dashboard(app);
        let _ = tauri::Emitter::emit(app, "navigate", "compose");
        return;
    }
    if let Some(rest) = id.strip_prefix("compose:") {
        compose_menu_action(app, rest);
        return;
    }
    if let Some(user) = id.strip_prefix("user:") {
        let state = app.state::<AppState>();
        let _ = state.update_settings(|s| s.preferences.shell_user = user.to_string());
        let _ = tauri::Emitter::emit(app, "preferences", state.preferences());
        rebuild(app);
        return;
    }
    let app = app.clone();
    let id = id.to_string();
    tauri::async_runtime::spawn(async move {
        let result = if let Some(container) = id.strip_prefix("shell:") {
            commands::open_container_shell(&app, container)
        } else if let Some(i) = id.strip_prefix("preset:").and_then(|i| i.parse().ok()) {
            commands::run_preset_by_index(&app, i).await.map(|_| ())
        } else {
            match id.as_str() {
                "start" => commands::run_vm_action(&app, VmAction::Start, TRAY_TASK).await,
                "stop" => commands::run_vm_action(&app, VmAction::Stop, TRAY_TASK).await,
                "restart" => commands::run_vm_action(&app, VmAction::Restart, TRAY_TASK).await,
                "prune-dangling" => commands::run_prune(&app, Prune::DanglingImages, TRAY_TASK)
                    .await
                    .map(|_| ()),
                "prune-cache" => commands::run_prune(&app, Prune::BuildCache, TRAY_TASK)
                    .await
                    .map(|_| ()),
                _ => Ok(()),
            }
        };
        if let Err(e) = result {
            commands::log_sink(&app, TRAY_TASK)(&format!("{id}: {e}"));
        }
    });
}

/// Reflect `status` in the tray menu and tooltip.
pub fn update(app: &AppHandle, status: &Status) {
    let Some(ts) = app.try_state::<TrayState>() else {
        return;
    };
    *ts.last_status.lock().unwrap() = Some(status.clone());
    apply_status(app, status);
}

/// The menu bar icon's status badge (see `icons/make_tray_icon.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    Running,
    Stopped,
    /// Two frames, alternated while starting/stopping/restarting.
    BusyA,
    BusyB,
    /// Engine unreachable, profile missing, not installed, ...
    Error,
}

impl Indicator {
    fn png(self) -> &'static [u8] {
        match self {
            Indicator::Running => include_bytes!("../icons/tray/running.png"),
            Indicator::Stopped => include_bytes!("../icons/tray/stopped.png"),
            Indicator::BusyA => include_bytes!("../icons/tray/busy-a.png"),
            Indicator::BusyB => include_bytes!("../icons/tray/busy-b.png"),
            Indicator::Error => include_bytes!("../icons/tray/error.png"),
        }
    }

    pub fn for_status(s: &Status) -> Self {
        match (s.busy.is_some(), s.vm_state, s.engine_version.is_some()) {
            (true, _, _) => Indicator::BusyA,
            (_, VmState::Stopped, _) => Indicator::Stopped,
            (_, _, true) => Indicator::Running,
            _ => Indicator::Error,
        }
    }
}

fn set_indicator(app: &AppHandle, indicator: Indicator) {
    let ts = app.state::<TrayState>();
    let mut cur = ts.indicator.lock().unwrap();
    if *cur == Some(indicator) {
        return;
    }
    if let (Some(tray), Ok(img)) = (app.tray_by_id(TRAY_ID), Image::from_bytes(indicator.png())) {
        let _ = tray.set_icon(Some(img));
        let _ = tray.set_icon_as_template(true);
        *cur = Some(indicator);
    }
}

/// Alternate the busy frames until the status is no longer busy.
fn animate_busy(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    let ts = app.state::<TrayState>();
    if ts.animating.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut frame = false;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let busy = app.state::<AppState>().busy().is_some();
            if !busy {
                break;
            }
            frame = !frame;
            set_indicator(
                &app,
                if frame {
                    Indicator::BusyB
                } else {
                    Indicator::BusyA
                },
            );
        }
        app.state::<TrayState>()
            .animating
            .store(false, Ordering::SeqCst);
        // Settle on the real state.
        let last = app.state::<TrayState>().last_status.lock().unwrap().clone();
        if let Some(s) = last.filter(|s| s.busy.is_none()) {
            set_indicator(&app, Indicator::for_status(&s));
        }
    });
}

fn apply_status(app: &AppHandle, status: &Status) {
    let indicator = Indicator::for_status(status);
    set_indicator(app, indicator);
    if indicator == Indicator::BusyA {
        animate_busy(app);
    }
    let ts = app.state::<TrayState>();
    let guard = ts.items.lock().unwrap();
    let Some(items) = guard.as_ref() else { return };
    let busy = status.busy.is_some();
    let state_text = match (&status.busy, status.vm_state) {
        (Some(b), _) => format!("{b}…"),
        (None, VmState::Running) => "Running".into(),
        (None, VmState::Stopped) => "Stopped".into(),
        (None, VmState::Unknown) => "Unknown".into(),
    };
    let headline = format!(
        "{} ({}): {}",
        status.provider_name, status.profile, state_text
    );
    let engine = match (&status.engine_version, &status.engine_error) {
        (Some(v), _) => format!("Engine {v}"),
        (None, Some(e)) if status.vm_state != VmState::Stopped => truncate(e, 60),
        _ => "Engine not reachable".into(),
    };
    let running = status.vm_state == VmState::Running;
    let engine_up = status.engine_version.is_some();

    let _ = items.status.set_text(&headline);
    let _ = items.engine.set_text(&engine);
    let _ = items
        .start
        .set_enabled(status.manages_vm && !busy && !running);
    let _ = items
        .stop
        .set_enabled(status.manages_vm && !busy && running);
    let _ = items
        .restart
        .set_enabled(status.manages_vm && !busy && running);
    let _ = items.prune_dangling.set_enabled(engine_up && !busy);
    let _ = items.prune_cache.set_enabled(engine_up && !busy);

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&headline));
    }
}

fn truncate(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or_default();
    if first.chars().count() <= max {
        first.to_string()
    } else {
        format!("{}…", first.chars().take(max).collect::<String>())
    }
}

/// Refresh the running-container list behind "Open Shell"; rebuilds the
/// menu only when it changed.
async fn refresh_containers(app: &AppHandle, engine_up: bool) {
    let mut list: Vec<(String, String)> = Vec::new();
    if engine_up {
        if let Ok(docker) = app.state::<AppState>().docker().await {
            if let Ok(cs) = docker.containers().await {
                list = cs
                    .into_iter()
                    .filter(|c| c.state == "running")
                    .map(|c| (c.id, c.names))
                    .collect();
                list.sort_by(|a, b| a.1.cmp(&b.1));
            }
        }
    }
    let changed = {
        let ts = app.state::<TrayState>();
        let mut cur = ts.containers.lock().unwrap();
        let changed = *cur != list;
        *cur = list;
        changed
    };
    if changed {
        rebuild(app);
    }
}

/// Poll the runtime periodically so the tray stays current even when the VM
/// or containers are changed from a terminal.
pub fn spawn_status_poller(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let up = commands::refresh_status(&app)
                .await
                .map(|s| s.engine_version.is_some())
                .unwrap_or(false);
            refresh_containers(&app, up).await;
            refresh_stacks(&app).await;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

/// `compose:<action>:<stack id>` from the Compose submenu.
fn compose_menu_action(app: &AppHandle, rest: &str) {
    let Some((action, id)) = rest.split_once(':') else {
        return;
    };
    let (action, id) = (action.to_string(), id.to_string());
    if action == "logs" {
        if let Err(e) = crate::compose::open_logs(app, &id) {
            commands::log_sink(app, TRAY_TASK)(&e.to_string());
        }
        return;
    }
    let run = {
        let app = app.clone();
        let (action, id) = (action.clone(), id.clone());
        move || {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::compose::run_action(&app, &id, &action, TRAY_TASK).await {
                    commands::log_sink(&app, TRAY_TASK)(&format!("compose {action}: {e}"));
                }
            });
        }
    };
    if action != "down" {
        run();
        return;
    }
    // Down removes containers and networks: confirm, as the dashboard does.
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let name = app
        .state::<TrayState>()
        .stacks
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "this stack".into());
    app.dialog()
        .message(format!(
            "Take down “{name}”? Its containers and networks are removed (volumes are kept)."
        ))
        .title("Compose down")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Down".into(),
            "Cancel".into(),
        ))
        .show(move |ok| {
            if ok {
                run();
            }
        });
}

/// Refresh the Compose submenu's stack statuses; rebuilds the menu only
/// when something changed.
pub async fn refresh_stacks(app: &AppHandle) {
    let state = app.state::<AppState>();
    let stacks = state.settings().compose_stacks;
    let engine_up = matches!(
        app.state::<TrayState>().last_status.lock().unwrap().as_ref(),
        Some(s) if s.engine_version.is_some()
    );
    let mut summaries = Vec::with_capacity(stacks.len());
    for stack in &stacks {
        let services = if engine_up {
            crate::compose::services(app, stack).await.ok()
        } else {
            None
        };
        summaries.push(StackSummary {
            id: stack.id.clone(),
            name: stack.name.clone(),
            running: services
                .as_ref()
                .map_or(0, |s| s.iter().filter(|s| s.state == "running").count()),
            total: services.as_ref().map_or(0, Vec::len),
            unknown: services.is_none(),
        });
    }
    let changed = {
        let ts = app.state::<TrayState>();
        let mut cur = ts.stacks.lock().unwrap();
        let changed = *cur != summaries;
        *cur = summaries;
        changed
    };
    if changed {
        rebuild(app);
    }
}
