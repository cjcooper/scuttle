//! The dashboard window, built from its `tauri.conf.json` entry
//! (`"create": false`) because only a window made transparent at creation
//! can show the OS material behind it.
//!
//! On macOS it gets the sidebar material (dimmed in inactive windows, like
//! Finder) and draws its own title bar under the window controls; the page
//! sets `data-vibrancy` and draws the sidebar and title bar clear over the
//! material, keeping the content pane opaque. Elsewhere it stays opaque.

use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Effect, EffectState};
use tauri::{App, WebviewWindowBuilder};

pub const MAIN: &str = "main";

/// Whether this system has a material to show through.
pub fn vibrancy() -> bool {
    cfg!(target_os = "macos")
}

fn effects() -> WindowEffectsConfig {
    WindowEffectsConfig {
        effects: vec![Effect::Sidebar],
        state: Some(EffectState::FollowsWindowActiveState),
        ..Default::default()
    }
}

pub fn build_main(app: &App) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN)
        .cloned()
        .expect("tauri.conf.json defines the main window");
    let mut builder = WebviewWindowBuilder::from_config(app.handle(), &config)?;
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    if vibrancy() {
        builder = builder.transparent(true).effects(effects());
    }
    builder.build()?;
    Ok(())
}

/// Whether the page should draw its sidebar and title bar clear.
#[tauri::command]
pub fn window_vibrancy() -> bool {
    vibrancy()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_gets_the_sidebar_material() {
        assert_eq!(vibrancy(), cfg!(target_os = "macos"));
        let e = effects();
        assert_eq!(e.effects, vec![Effect::Sidebar]);
        assert_eq!(e.state, Some(EffectState::FollowsWindowActiveState));
    }
}
