//! AMS desktop shell: the deployed AMS site in a Tauri window, plus what a desktop app adds
//! (tray, notifications, downloads, offline page, menus, auto update). One module per concern;
//! `run` wires them together.

mod downloads;
mod links;
#[cfg(target_os = "macos")]
mod location;
mod menu;
mod notifications;
mod offline;
mod settings;
mod updater;
mod window;

/// Windows / Linux: the webview's own geolocation (see `on_permission_request`); GEO_SCRIPT, the only
/// caller of these, is injected on macOS only.
#[cfg(not(target_os = "macos"))]
mod location {
  #[tauri::command]
  pub async fn get_location() -> Result<(), String> {
    Err("unavailable".into())
  }
  #[tauri::command]
  pub async fn location_permission() -> Result<String, String> {
    Ok("prompt".into())
  }
}

use tauri::{Manager, WindowEvent, Wry};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_window_state::StateFlags;

use crate::links::{DEEP_LINK_SCHEME_PREFIX, open_deep_link};
#[cfg(target_os = "macos")]
use crate::menu::setup_app_menu;
use crate::menu::{on_menu, setup_tray};
#[cfg(not(target_os = "macos"))]
use crate::notifications::on_notification_click;
use crate::updater::spawn_updater;
use crate::window::{CHAT, MAIN, TOGGLE_SHORTCUT, build_main_window, hide_to_tray, show_main, toggle_main};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    // Must be the first plugin: a second launch focuses the running app instead.
    .plugin(tauri_plugin_single_instance::init(|app, args, _| {
      // An ams:// link is opened by the deep-link plugin (open_deep_link), maybe in the chat window.
      if !args.iter().any(|arg| arg.starts_with(DEEP_LINK_SCHEME_PREFIX)) {
        show_main(app);
      }
    }))
    // After single-instance (its deep-link feature forwards a second instance's link here).
    .plugin(tauri_plugin_deep_link::init())
    // Replaces window.Notification in the page, so the web chat popups become OS notifications.
    .plugin(tauri_plugin_notification::init())
    .plugin(tauri_plugin_opener::init())
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_updater::Builder::new().build())
    // macOS LaunchAgent / Windows Run key; the flag lets a login launch start hidden (Settings).
    .plugin(tauri_plugin_autostart::Builder::new().args([settings::AUTOSTART_ARG]).build())
    .plugin(
      tauri_plugin_window_state::Builder::default()
        // No VISIBLE flag: quitting from the tray while hidden must not start the next launch hidden.
        .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
        .with_filter(|label| label == MAIN || label == CHAT)
        .build(),
    )
    // TOGGLE_SHORTCUT (registered in setup): show / hide AMS from any app.
    .plugin(
      tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _, event| {
          if event.state() == ShortcutState::Pressed {
            toggle_main(app);
          }
        })
        .build(),
    )
    // Brand-teal title bar on every window (window/title_bar.rs).
    .plugin(tauri::plugin::Builder::<Wry, ()>::new("title-bar").on_window_ready(window::title_bar::paint).build())
    .invoke_handler(tauri::generate_handler![
      links::open_link,
      downloads::download_source,
      downloads::file_saved,
      downloads::show_saved_file,
      notifications::set_badge,
      notifications::notify,
      location::get_location,
      location::location_permission,
      settings::get_settings,
      settings::set_autostart,
      settings::set_setting,
      settings::check_update,
      settings::test_notification,
      settings::open_notification_settings,
      settings::open_log_folder,
      settings::clear_data,
      offline::page_alive,
      offline::page_leaving
    ])
    .register_uri_scheme_protocol(settings::PROTOCOL, |_ctx, request| settings::serve(&request))
    .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
    .setup(|app| {
      // Every build: the log file is what IT asks for (Settings ▸ Nâng cao ▸ Mở thư mục log).
      // macOS ~/Library/Logs/vn.com.agari.ams, Windows %LOCALAPPDATA%\vn.com.agari.ams\logs.
      app.handle().plugin(
        tauri_plugin_log::Builder::default()
          .level(log::LevelFilter::Info)
          .max_file_size(2_000_000)
          .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
          .build(),
      )?;
      app.manage(settings::load(app.handle()));
      #[cfg(target_os = "macos")]
      notifications::mac::present_in_foreground();
      // A click on a notification does what the page's onclick does (on_notification_click).
      // macOS: the plugin's click tracking never reports it (mac-notification-sys stops waiting
      // after delivery), so notifications::mac takes the click from the notification delegate instead, and
      // no plugin listener is registered (it would also keep a waiting thread per notification).
      #[cfg(target_os = "macos")]
      notifications::mac::handle_clicks(app.handle().clone());
      // Windows: the plugin reports a click on a toast still on screen (not from the Action Center).
      #[cfg(not(target_os = "macos"))]
      let handle = app.handle().clone();
      #[cfg(not(target_os = "macos"))]
      app.notification().on_action(move |performed| {
        log::info!(
          "notification action {:?} on #{:?}",
          performed.action_id(),
          performed.notification().map(|n| n.id())
        );
        if performed.action_id() == "tap" {
          on_notification_click(&handle, performed.notification().map(|n| n.id()));
        }
      })?;
      if !cfg!(debug_assertions) {
        // Release only: a dev run must not repoint the autostart entry at the dev binary.
        settings::refresh_autostart(app.handle());
        spawn_updater(app.handle().clone());
      }
      // Settings ▸ "Mở ẩn": a login launch stays in the tray (macOS Dock click / tray opens it).
      let hidden = settings::launched_by_autostart() && settings::prefs(app.handle()).start_hidden;
      build_main_window(app.handle(), !hidden)?;
      // ams:// links: a cold start on Windows carries one in its arguments (get_current); later ones,
      // and every one on macOS, arrive as events.
      let handle = app.handle().clone();
      app.deep_link().on_open_url(move |event| {
        for link in event.urls() {
          open_deep_link(&handle, &link);
        }
      });
      if let Ok(Some(links)) = app.deep_link().get_current() {
        for link in links {
          open_deep_link(app.handle(), &link);
        }
      }
      #[cfg(target_os = "macos")]
      setup_app_menu(app.handle())?;
      setup_tray(app.handle())?;
      // Taken by another app: AMS works without it.
      if let Err(e) = app.global_shortcut().register(TOGGLE_SHORTCUT) {
        log::warn!("shortcut {TOGGLE_SHORTCUT} unavailable: {e}");
      }
      Ok(())
    })
    .on_window_event(|window, event| {
      // Closing the main window hides it to the tray so chat notifications keep arriving, unless
      // Settings ▸ Chung turned that off: then closing it quits, as people who expect X to quit want.
      if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == MAIN
      {
        if settings::prefs(window.app_handle()).close_to_tray {
          api.prevent_close();
          hide_to_tray(window.app_handle());
        } else {
          window.app_handle().exit(0);
        }
      }
    })
    .build(tauri::generate_context!())
    .expect("error while building tauri application")
    .run(|_app, _event| {
      // macOS: clicking the Dock icon brings the hidden main window back.
      #[cfg(target_os = "macos")]
      {
        if let tauri::RunEvent::Reopen { .. } = _event {
          show_main(_app);
        }
      }
    });
}
