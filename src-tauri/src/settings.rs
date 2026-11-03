//! The "Cài đặt" window: a small page bundled in the app (src-tauri/settings.html), not the AMS site,
//! so it works offline and needs no FE change. Opened from the macOS app menu (AMS ▸ Cài đặt…, Cmd+,)
//! and from the tray on every OS (Windows has no menu bar).
//!
//! A new setting: a row in settings.html, its field in `Settings`, and a `set_*` command here
//! (registered in lib.rs), guarded by `from_settings`.

use serde::Serialize;
use tauri::http::{header::CONTENT_TYPE, Response, StatusCode};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, Window};
use tauri_plugin_autostart::ManagerExt as _;

pub const LABEL: &str = "settings";
/// Registered in lib.rs. A page served by an app-registered protocol counts as local, so it can
/// invoke the commands below; capabilities/settings.json grants the window `core:default` only.
pub const PROTOCOL: &str = "amssettings";

const PAGE: &str = include_str!("../settings.html");

/// macOS serves custom protocols as `<scheme>://localhost`, Windows (WebView2) as `http://<scheme>.localhost`.
fn page_url() -> Url {
  #[cfg(windows)]
  let url = format!("http://{PROTOCOL}.localhost/");
  #[cfg(not(windows))]
  let url = format!("{PROTOCOL}://localhost/");
  url.parse().expect("settings page url is valid")
}

pub fn serve(request: &tauri::http::Request<Vec<u8>>) -> Response<Vec<u8>> {
  let path = request.uri().path();
  if path == "/" || path == "/index.html" {
    Response::builder()
      .header(CONTENT_TYPE, "text/html; charset=utf-8")
      .body(PAGE.as_bytes().to_vec())
      .unwrap_or_default()
  } else {
    Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()).unwrap_or_default()
  }
}

/// One settings window: a second open brings it forward.
pub fn open(app: &AppHandle) {
  if let Some(window) = app.get_webview_window(LABEL) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    return;
  }
  let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::CustomProtocol(page_url()))
    .title("Cài đặt")
    .inner_size(560.0, 360.0)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .center()
    .build();
  if let Err(e) = built {
    log::error!("opening the settings window failed: {e}");
  }
}

/// The AMS site (main/chat windows) can reach these commands too; only the settings page may use them.
fn from_settings(window: &Window) -> Result<(), String> {
  if window.label() == LABEL {
    Ok(())
  } else {
    Err("not allowed".into())
  }
}

#[derive(Serialize)]
pub struct Settings {
  autostart: bool,
  version: String,
}

#[tauri::command]
pub fn get_settings(app: AppHandle, window: Window) -> Result<Settings, String> {
  from_settings(&window)?;
  Ok(Settings {
    autostart: app.autolaunch().is_enabled().unwrap_or(false),
    version: app.package_info().version.to_string(),
  })
}

/// macOS: a LaunchAgent; Windows: the Run registry key. Returns the state actually in effect.
#[tauri::command]
pub fn set_autostart(app: AppHandle, window: Window, enabled: bool) -> Result<bool, String> {
  from_settings(&window)?;
  let launcher = app.autolaunch();
  let result = if enabled { launcher.enable() } else { launcher.disable() };
  result.map_err(|e| e.to_string())?;
  Ok(launcher.is_enabled().unwrap_or(enabled))
}
