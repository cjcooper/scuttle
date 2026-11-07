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

#[derive(Default)]
struct TrayState {
    items: Mutex<Option<TrayItems>>,
    /// (id, name) of running containers shown in the "Open Shell" submenu.
    containers: Mutex<Vec<(String, String)>>,
    last_status: Mutex<Option<Status>>,
}

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    app.manage(TrayState::default());
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true)
        .tooltip("Container Desktop")
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
            &sep()?,
            &items.prune_dangling,
            &items.prune_cache,
            &sep()?,
            &item("open", "Open Dashboard…", true)?,
            &item("about", "About Container Desktop", true)?,
            &sep()?,
            &PredefinedMenuItem::quit(app, Some("Quit Container Desktop"))?,
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
    if id == "about" {
        let _ = crate::about::open(app);
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

fn apply_status(app: &AppHandle, status: &Status) {
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
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}
