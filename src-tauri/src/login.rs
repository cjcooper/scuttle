//! Open at login: registers the app with the OS (a LaunchAgent on macOS,
//! the Run key on Windows, an autostart .desktop file on Linux) via the
//! auto-launch crate. Login launches pass `--autostart`; the app then stays
//! in the menu bar and, if enabled, starts the selected provider.
//!
//! On macOS the LaunchAgent names the app's bundle identifier in
//! `AssociatedBundleIdentifiers`, so System Settings › Login Items lists it
//! as "Scuttle" with its icon rather than by the signing
//! certificate's name.

use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::commands::{self, VmAction};
use crate::error::{AppError, Result};
use crate::runtime::VmState;
use crate::state::AppState;

/// The argument the login item launches the app with.
pub const ARG: &str = "--autostart";

/// The OS login item for this app.
pub struct Launcher(AutoLaunch);

impl Launcher {
    pub fn new(app: &AppHandle) -> Result<Self> {
        Self::named(app, &app.package_info().name)
    }

    /// A login item registered under `name` (for this app or its old name).
    fn named(app: &AppHandle, name: &str) -> Result<Self> {
        let exe = std::env::current_exe()?;
        let mut b = AutoLaunchBuilder::new();
        b.set_app_name(name).set_args(&[ARG]);
        #[cfg(target_os = "macos")]
        {
            b.set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
                .set_bundle_identifiers(&[app.config().identifier.as_str()])
                .set_app_path(&exe.canonicalize()?.display().to_string());
        }
        #[cfg(target_os = "linux")]
        {
            let path = app
                .env()
                .appimage
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| exe.display().to_string());
            b.set_app_path(&path);
        }
        #[cfg(windows)]
        b.set_app_path(&exe.display().to_string());
        b.build()
            .map(Launcher)
            .map_err(|e| AppError::msg(format!("login item: {e}")))
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled().unwrap_or(false)
    }

    fn set(&self, enabled: bool) -> Result<()> {
        let r = if enabled {
            self.0.enable()
        } else {
            self.0.disable()
        };
        r.map_err(|e| AppError::msg(format!("could not update the login item: {e}")))
    }
}

/// Rewrite an enabled login item so it carries the current settings (bundle
/// identifier, app location), e.g. one written by an earlier version.
pub fn refresh(app: &AppHandle) {
    // Dev builds would repoint the user's login item at target/debug.
    if cfg!(debug_assertions) {
        return;
    }
    let launcher = app.state::<Launcher>();
    // The app used to be "Container Desktop": replace its login item.
    let mut enable = launcher.is_enabled();
    if let Ok(old) = Launcher::named(app, crate::migrate::OLD_NAME) {
        if old.is_enabled() {
            let _ = old.set(false);
            enable = true;
        }
    }
    if enable {
        let _ = launcher.set(true);
    }
}

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
        enabled: app.state::<Launcher>().is_enabled(),
        start_provider: app
            .state::<AppState>()
            .preferences()
            .start_provider_at_login,
    }
}

pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<()> {
    app.state::<Launcher>().set(enabled)?;
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
    if app.state::<Launcher>().is_enabled() != enabled {
        set_enabled(&app, enabled)?;
    }
    Ok(status(&app))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// Writes into $HOME/Library/LaunchAgents, so run with a scratch HOME:
    /// `HOME=<tmp> cargo test login_plist -- --ignored`
    #[test]
    #[ignore]
    fn login_plist() {
        let home = std::env::var("HOME").unwrap();
        assert!(
            !home.starts_with("/Users/"),
            "set HOME to a scratch directory"
        );
        let mut b = auto_launch::AutoLaunchBuilder::new();
        b.set_app_name("Scuttle")
            .set_args(&[super::ARG])
            .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
            .set_bundle_identifiers(&["com.it-problems.scuttle"])
            .set_app_path("/Applications/Scuttle.app/Contents/MacOS/scuttle");
        let launcher = super::Launcher(b.build().unwrap());
        launcher.set(true).unwrap();
        assert!(launcher.is_enabled());
        let plist =
            std::fs::read_to_string(format!("{home}/Library/LaunchAgents/Scuttle.plist")).unwrap();
        println!("{plist}");
        assert!(plist.contains("<array><string>com.it-problems.scuttle</string></array>"));
        launcher.set(false).unwrap();
        assert!(!launcher.is_enabled());
    }
}
