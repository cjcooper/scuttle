//! Open at login: registers the app with the OS (LaunchAgent on macOS, the
//! Run key on Windows, an autostart .desktop file on Linux) via
//! tauri-plugin-autostart. Login launches pass `--autostart`; the app then
//! stays in the menu bar and, if enabled, starts the selected provider.

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::commands::{self, VmAction};
use crate::error::{AppError, Result};
use crate::runtime::VmState;
use crate::state::AppState;

/// The argument the login item launches the app with.
pub const ARG: &str = "--autostart";

/// Whether this process was started by the login item.
pub fn launched_at_login() -> bool {
    std::env::args().any(|a| a == ARG)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginItem {
    /// Registered to open at login.
    pub enabled: bool,
    /// Start the selected provider/profile when opened at login.
    pub start_provider: bool,
}

pub fn status(app: &AppHandle) -> LoginItem {
    LoginItem {
        enabled: app.autolaunch().is_enabled().unwrap_or(false),
        start_provider: app
            .state::<AppState>()
            .preferences()
            .start_provider_at_login,
    }
}

pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<()> {
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| AppError::msg(format!("could not update the login item: {e}")))?;
    crate::tray::rebuild(app);
    Ok(())
}

/// After a login launch: start the selected provider's VM if it manages one
/// and it isn't running.
pub fn on_login_launch(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.preferences().start_provider_at_login {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Ok((provider, profile)) = state.current() else {
            return;
        };
        if !provider.manages_vm() || !provider.is_installed() {
            return;
        }
        let running =
            matches!(provider.profile(&profile).await, Ok(Some(p)) if p.state == VmState::Running);
        if !running {
            let _ = commands::run_vm_action(&app, VmAction::Start, "login").await;
        }
    });
}

#[tauri::command]
pub fn login_item_get(app: AppHandle) -> LoginItem {
    status(&app)
}

#[tauri::command]
pub fn login_item_set(app: AppHandle, enabled: bool, start_provider: bool) -> Result<LoginItem> {
    app.state::<AppState>()
        .update_settings(|s| s.preferences.start_provider_at_login = start_provider)?;
    if app.autolaunch().is_enabled().unwrap_or(false) != enabled {
        set_enabled(&app, enabled)?;
    }
    Ok(status(&app))
}
