use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_window_state::StateFlags;

const MAIN: &str = "main";
/// capabilities/default.json grants "popup-*" the same page permissions as the main window.
const POPUP_PREFIX: &str = "popup-";
/// The app lives in the tray for weeks, so checking only at startup is not enough.
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

static POPUP_SEQ: AtomicU32 = AtomicU32::new(0);

type WindowBuilder<'a> = WebviewWindowBuilder<'a, Wry, AppHandle>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    // Must be the first plugin: a second launch focuses the running app instead.
    .plugin(tauri_plugin_single_instance::init(|app, _, _| show_main(app)))
    // Replaces window.Notification in the page, so the web chat popups become OS notifications.
    .plugin(tauri_plugin_notification::init())
    .plugin(tauri_plugin_opener::init())
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_updater::Builder::new().build())
    .plugin(tauri_plugin_autostart::Builder::new().build())
    .plugin(
      tauri_plugin_window_state::Builder::default()
        // No VISIBLE flag: quitting from the tray while hidden must not start the next launch hidden.
        .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
        .with_filter(|label| label == MAIN)
        .build(),
    )
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      } else {
        spawn_updater(app.handle().clone());
      }
      build_main_window(app.handle())?;
      setup_tray(app.handle())?;
      Ok(())
    })
    .on_window_event(|window, event| {
      // Closing the main window hides it to the tray so chat notifications keep arriving.
      if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == MAIN
      {
        api.prevent_close();
        let _ = window.hide();
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

fn show_main(app: &AppHandle) {
  if let Some(window) = app.get_webview_window(MAIN) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
  }
}

/// Built here instead of from tauri.conf.json ("create": false) to attach the popup and download handlers.
fn build_main_window(app: &AppHandle) -> tauri::Result<()> {
  let config = app
    .config()
    .app
    .windows
    .iter()
    .find(|w| w.label == MAIN)
    .expect("main window declared in tauri.conf.json");
  with_handlers(WebviewWindowBuilder::from_config(app, config)?, app).build()?;
  Ok(())
}

fn with_handlers<'a>(builder: WindowBuilder<'a>, app: &AppHandle) -> WindowBuilder<'a> {
  let app = app.clone();
  let builder = builder.on_new_window(move |url, features| open_new_window(&app, url, features));
  // WKWebView cancels every download unless a handler exists (wry then saves to ~/Downloads).
  // Windows keeps WebView2's own download flyout, which a handler would hide.
  #[cfg(target_os = "macos")]
  let builder = builder.on_download(|_, event| {
    if let tauri::webview::DownloadEvent::Finished { path: Some(path), success: true, .. } = event {
      let _ = tauri_plugin_opener::reveal_item_in_dir(path);
    }
    true
  });
  builder
}

/// window.open / target="_blank". Without a handler both engines silently drop the request.
/// AMS pages (chat tab, payment print, attachments) open in an app window that keeps the session
/// and window.opener; meeting links and other sites go to the default browser.
fn open_new_window(app: &AppHandle, url: Url, features: NewWindowFeatures) -> NewWindowResponse<Wry> {
  if !is_app_url(&url) {
    if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
      log::error!("opening {url} in the browser failed: {e}");
    }
    return NewWindowResponse::Deny;
  }
  let label = format!("{POPUP_PREFIX}{}", POPUP_SEQ.fetch_add(1, Ordering::Relaxed));
  // The engine loads `url` into the returned window; about:blank is only the placeholder.
  let blank = WebviewUrl::External("about:blank".parse().expect("about:blank is a valid url"));
  let builder = WebviewWindowBuilder::new(app, label, blank)
    .title("AMS")
    .inner_size(1100.0, 800.0)
    .window_features(features)
    .on_document_title_changed(|window, title| {
      let _ = window.set_title(&title);
    });
  match with_handlers(builder, app).build() {
    Ok(window) => NewWindowResponse::Create { window },
    Err(e) => {
      log::error!("opening {url} in a popup failed: {e}");
      NewWindowResponse::Deny
    }
  }
}

/// AMS hosts (FE, API, chat), the local dev server, and blob: URLs created by the page.
fn is_app_url(url: &Url) -> bool {
  url.scheme() == "blob"
    || matches!(url.host_str(), Some(h) if h == "localhost" || h == "agari.com.vn" || h.ends_with(".agari.com.vn"))
}

/// The tray icon itself comes from tauri.conf.json (app.trayIcon): the Tauri CLI keeps the
/// `tray-icon` Cargo feature in sync with that key. Only the menu and clicks are wired here.
fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
  let Some(tray) = app.tray_by_id(MAIN) else {
    return Ok(());
  };
  let autostart = app.autolaunch().is_enabled().unwrap_or(false);
  let menu = Menu::with_items(
    app,
    &[
      &MenuItem::with_id(app, "open", "Mở AMS", true, None::<&str>)?,
      &CheckMenuItem::with_id(app, "autostart", "Khởi động cùng máy", true, autostart, None::<&str>)?,
      &MenuItem::with_id(app, "quit", "Thoát", true, None::<&str>)?,
    ],
  )?;
  tray.set_menu(Some(menu))?;
  tray.on_menu_event(|app, event| match event.id.as_ref() {
    "open" => show_main(app),
    "autostart" => toggle_autostart(app),
    "quit" => app.exit(0),
    _ => {}
  });
  tray.on_tray_icon_event(|tray, event| {
    if let TrayIconEvent::Click {
      button: MouseButton::Left,
      button_state: MouseButtonState::Up,
      ..
    } = event
    {
      show_main(tray.app_handle());
    }
  });
  Ok(())
}

fn toggle_autostart(app: &AppHandle) {
  let launcher = app.autolaunch();
  let result = if launcher.is_enabled().unwrap_or(false) {
    launcher.disable()
  } else {
    launcher.enable()
  };
  if let Err(e) = result {
    log::error!("toggling autostart failed: {e}");
  }
}

fn spawn_updater(app: AppHandle) {
  std::thread::spawn(move || {
    loop {
      if let Err(e) = tauri::async_runtime::block_on(update(&app)) {
        log::error!("update check failed: {e}");
      }
      std::thread::sleep(UPDATE_EVERY);
    }
  });
}

/// Asks first: installing restarts the app (on Windows the installer closes it).
async fn update(app: &AppHandle) -> tauri_plugin_updater::Result<()> {
  let Some(update) = app.updater()?.check().await? else {
    return Ok(());
  };
  let accepted = app
    .dialog()
    .message(format!("Đã có AMS phiên bản {}. Cập nhật và khởi động lại ngay?", update.version))
    .title("Cập nhật AMS")
    .buttons(MessageDialogButtons::OkCancelCustom("Cập nhật".into(), "Để sau".into()))
    .blocking_show();
  if accepted {
    update.download_and_install(|_, _| {}, || {}).await?;
    app.restart();
  }
  Ok(())
}
