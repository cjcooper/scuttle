//! The app menu (shown while a window is open, along with the Dock icon):
//! Tauri's default menu with our own About window in place of the system's,
//! a Dashboard item, and ⌘Q closing the windows rather than quitting: the
//! app keeps running in the menu bar, whose own menu has the real Quit.
//!
//! The app lives in the menu bar; it switches to a regular app (Dock icon and
//! menu) only while one of its windows is visible.

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager};

pub const MAKER: &str = "it-problems";
const ABOUT_ID: &str = "app-about";
const DASHBOARD_ID: &str = "app-dashboard";
const CLOSE_ID: &str = "app-close";

const MAC: bool = cfg!(target_os = "macos");

fn submenus(menu: &Menu<tauri::Wry>) -> tauri::Result<Vec<(String, Submenu<tauri::Wry>)>> {
    Ok(menu
        .items()?
        .into_iter()
        .filter_map(|i| i.as_submenu().cloned())
        .map(|s| (s.text().unwrap_or_default(), s))
        .collect())
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::default(app)?;
    let subs = submenus(&menu)?;

    // About: first item of the app menu on macOS, of Help elsewhere.
    let about = MenuItem::with_id(
        app,
        ABOUT_ID,
        format!("About {}", app.package_info().name),
        true,
        None::<&str>,
    )?;
    let index = if MAC { 0 } else { subs.len().saturating_sub(1) };
    if let Some((_, sub)) = subs.get(index) {
        if sub.remove_at(0)?.is_some() {
            sub.insert(&about, 0)?;
        }
    }

    // ⌘Q closes the windows; quitting is in the menu bar menu.
    let close = MenuItem::with_id(
        app,
        CLOSE_ID,
        format!("Close {}", app.package_info().name),
        true,
        Some("CmdOrCtrl+Q"),
    )?;
    let mut replaced = false;
    for (_, sub) in &subs {
        for (i, item) in sub.items()?.iter().enumerate() {
            let text = item
                .as_predefined_menuitem()
                .and_then(|p| p.text().ok())
                .unwrap_or_default();
            if text.starts_with("Quit") || text == "Exit" {
                sub.remove_at(i)?;
                sub.insert(&close, i)?;
                replaced = true;
                break;
            }
        }
        if replaced {
            break;
        }
    }
    if !replaced {
        if let Some((_, first)) = subs.first() {
            first.append(&close)?;
        }
    }

    // Dashboard (⌘0) under Window.
    let dashboard = MenuItem::with_id(app, DASHBOARD_ID, "Dashboard", true, Some("CmdOrCtrl+0"))?;
    if let Some((_, window)) = subs.iter().find(|(t, _)| t == "Window") {
        window.append(&PredefinedMenuItem::separator(app)?)?;
        window.append(&dashboard)?;
        #[cfg(target_os = "macos")]
        window.set_as_windows_menu_for_nsapp()?;
    }

    app.set_menu(menu)?;
    app.on_menu_event(on_menu_event);
    Ok(())
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        ABOUT_ID => {
            let _ = crate::about::open(app);
        }
        DASHBOARD_ID => crate::show_dashboard(app),
        CLOSE_ID => close_windows(app),
        _ => {}
    }
}

/// Close the app's windows and go back to the menu bar: the dashboard is
/// hidden (it keeps its state), shell windows and About are closed.
pub fn close_windows(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label == "main" {
            let _ = w.hide();
        } else {
            let _ = w.close();
        }
    }
    update_dock(app, Some("main"));
}

/// Show the Dock icon and menu while any window is visible; otherwise be a
/// menu-bar-only app. `closing` is a window that is going away.
pub fn update_dock(app: &AppHandle, closing: Option<&str>) {
    #[cfg(target_os = "macos")]
    {
        let visible = app
            .webview_windows()
            .values()
            .any(|w| Some(w.label()) != closing && w.is_visible().unwrap_or(false));
        let policy = if visible {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }
    let _ = (app, closing);
}
