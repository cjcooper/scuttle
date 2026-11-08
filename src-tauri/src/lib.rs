mod about;
mod app_icon;
mod aws;
mod colima_config;
mod commands;
mod engine;
mod error;
mod history;
mod menu;
mod process;
mod runtime;
mod shell;
mod state;
mod terminal;
mod tray;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

pub fn show_dashboard(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        menu::update_dock(app, None);
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn run() {
    process::inherit_login_env();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(app_icon::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_providers,
            commands::list_profiles,
            commands::get_selection,
            commands::set_selection,
            commands::get_status,
            commands::vm_action,
            commands::list_images,
            commands::list_containers,
            commands::disk_usage,
            commands::remove_images,
            commands::prune_images,
            commands::prune_build_cache,
            commands::container_action,
            commands::build_image,
            commands::pull_image,
            commands::get_settings,
            commands::set_preferences,
            commands::save_preset,
            commands::delete_preset,
            commands::preview_run,
            commands::run_container,
            commands::open_shell,
            commands::image_shell,
            commands::ecr_info,
            commands::colima_config_get,
            commands::colima_config_defaults,
            commands::colima_config_save,
            commands::colima_config_save_raw,
            commands::colima_apply,
            commands::colima_create,
            commands::colima_delete,
            commands::host_info,
            history::history_list,
            history::history_delete,
            history::history_clear,
            commands::colima_data_usage,
            commands::colima_delete_all,
            commands::colima_prune,
            commands::aws_sso_login,
            about::about_info,
            about::close_about,
            shell::shell_start,
            shell::shell_input,
            shell::shell_resize,
            shell::shell_pop_out,
        ])
        .setup(|app| {
            let settings = app
                .path()
                .app_config_dir()
                .ok()
                .map(|d| d.join("settings.json"));
            app.manage(state::AppState::new(settings));
            app.manage(shell::ShellSessions::default());
            app.manage(history::History::load(
                app.path()
                    .app_config_dir()
                    .ok()
                    .map(|d| d.join("history.json")),
            ));

            // Menu bar app with a Dock icon and full menu while a window is open.
            menu::install(app.handle())?;
            menu::update_dock(app.handle(), None);

            tray::init(app.handle())?;
            tray::spawn_status_poller(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the dashboard hides it; the app lives in the menu bar.
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let _ = window.hide();
                menu::update_dock(window.app_handle(), Some("main"));
            }
            WindowEvent::ThemeChanged(theme) => {
                app_icon::theme_changed(window.app_handle(), window.label(), *theme)
            }
            WindowEvent::Destroyed => {
                // A shell window closed: end its session.
                if window.label().starts_with(shell::LABEL_PREFIX) {
                    window
                        .state::<shell::ShellSessions>()
                        .stop(window.app_handle(), window.label());
                }
                menu::update_dock(window.app_handle(), Some(window.label()));
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app, event| match event {
        // Dock icon clicked.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_dashboard(app),
        // Closing the last window keeps the app in the menu bar; only Quit
        // (which passes an exit code) ends it.
        RunEvent::ExitRequested {
            api, code: None, ..
        } => api.prevent_exit(),
        _ => {}
    });
}
