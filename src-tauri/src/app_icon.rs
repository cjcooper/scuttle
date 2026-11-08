//! The window icon follows the window's light or dark appearance on Windows,
//! where it shows in the title bar and taskbar. Elsewhere it stays light:
//! macOS takes the Dock icon from the bundle (`icons/AppIcon.icon`, which the
//! system switches itself), and Linux desktops take it from the desktop entry,
//! whose icon theme has no dark version.

use tauri::image::Image;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, Theme, Window};

const DARK: &[u8] = include_bytes!("../icons/dark-256x256.png");

/// Whether this system's window icons follow the window's appearance.
const FOLLOWS: bool = cfg!(windows);

/// Sets each window's icon as it opens; `theme_changed` sets it again when
/// its appearance changes.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("app-icon")
        .on_window_ready(|window| follow(&window, FOLLOWS, None))
        .build()
}

/// Window `label`'s appearance changed.
pub fn theme_changed<R: Runtime>(app: &AppHandle<R>, label: &str, theme: Theme) {
    if let Some(webview) = app.get_webview_window(label) {
        follow(&webview.as_ref().window(), FOLLOWS, Some(theme));
    }
}

/// The icon version for a window's appearance: dark only for a dark window on
/// a system whose icons follow it; light when unknown.
fn version(follows: bool, theme: Option<Theme>) -> Theme {
    match theme {
        Some(Theme::Dark) if follows => Theme::Dark,
        _ => Theme::Light,
    }
}

/// Where icons follow the appearance (`follows`), sets `window`'s for
/// `theme`, or for its current appearance when that's `None`.
fn follow<R: Runtime>(window: &Window<R>, follows: bool, theme: Option<Theme>) {
    if !follows {
        return;
    }
    let theme = theme.or_else(|| window.theme().ok());
    let _ = match version(follows, theme) {
        Theme::Dark => Image::from_bytes(DARK).and_then(|icon| window.set_icon(icon)),
        _ => match window.app_handle().default_window_icon() {
            Some(icon) => window.set_icon(icon.clone()),
            None => Ok(()),
        },
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_only_where_icons_follow_the_appearance() {
        assert_eq!(version(true, Some(Theme::Dark)), Theme::Dark);
        assert_eq!(version(true, Some(Theme::Light)), Theme::Light);
        assert_eq!(version(false, Some(Theme::Dark)), Theme::Light);
        assert_eq!(version(true, None), Theme::Light);
    }

    #[test]
    fn dark_icon_decodes() {
        let icon = Image::from_bytes(DARK).unwrap();
        assert_eq!((icon.width(), icon.height()), (256, 256));
    }
}
