//! The "Cài đặt" window: a small page bundled in the app (src-tauri/settings.html), not the AMS site,
//! so it works offline and needs no FE change. Opened from the macOS app menu (AMS ▸ Cài đặt…, Cmd+,)
//! and from the tray on every OS (Windows has no menu bar).
//!
//! The choices live in `<app config dir>/settings.json` (macOS: ~/Library/Application Support/
//! vn.com.agari.ams, Windows: %APPDATA%\vn.com.agari.ams); autostart itself is the OS's own entry.
//!
//! A new on/off setting: a field in `Prefs` (+ its default), its key in `set_setting`, a row in
//! settings.html, and the code that reads `prefs(app).<field>`.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::http::{header::CONTENT_TYPE, Response, StatusCode};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, Window};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

pub const LABEL: &str = "settings";
/// Registered in lib.rs. A page served by an app-registered protocol counts as local, so it can
/// invoke the commands below; capabilities/settings.json grants the window `core:default` only.
pub const PROTOCOL: &str = "amssettings";
/// Given to the OS autostart entry (lib.rs plugin setup): tells a login launch from a manual one.
pub const AUTOSTART_ARG: &str = "--autostart";

const PAGE: &str = include_str!("../settings.html");

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
  /// The OS notification plays the system sound.
  pub notification_sound: bool,
  /// The notification shows the message text; off: only who sent it ("Có tin nhắn mới").
  pub notification_preview: bool,
  /// A login launch (autostart) stays in the tray instead of opening the window.
  pub start_hidden: bool,
  /// Check GitHub releases at startup and every 6 h.
  pub auto_update: bool,
  /// Closing the main window keeps AMS in the tray (notifications keep arriving); off: it quits.
  pub close_to_tray: bool,
}

impl Default for Prefs {
  fn default() -> Self {
    Self {
      notification_sound: true,
      notification_preview: true,
      start_hidden: false,
      auto_update: true,
      close_to_tray: true,
    }
  }
}

pub struct PrefsState(Mutex<Prefs>);

fn prefs_path(app: &AppHandle) -> Option<PathBuf> {
  app.path().app_config_dir().ok().map(|dir| dir.join("settings.json"))
}

/// Missing or unreadable file: the defaults (a broken file must never stop the app from starting).
pub fn load(app: &AppHandle) -> PrefsState {
  let prefs = prefs_path(app)
    .and_then(|path| std::fs::read_to_string(path).ok())
    .and_then(|text| serde_json::from_str(&text).ok())
    .unwrap_or_default();
  PrefsState(Mutex::new(prefs))
}

fn save(app: &AppHandle, prefs: &Prefs) -> Result<(), String> {
  let path = prefs_path(app).ok_or("no config directory")?;
  if let Some(dir) = path.parent() {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
  }
  let text = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
  std::fs::write(path, text).map_err(|e| e.to_string())
}

pub fn prefs(app: &AppHandle) -> Prefs {
  app.state::<PrefsState>().0.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn launched_by_autostart() -> bool {
  std::env::args().any(|arg| arg == AUTOSTART_ARG)
}

/// Entries made before AUTOSTART_ARG existed lack it: writing the entry again adds it.
pub fn refresh_autostart(app: &AppHandle) {
  let launcher = app.autolaunch();
  if launcher.is_enabled().unwrap_or(false)
    && let Err(e) = launcher.enable()
  {
    log::error!("refreshing the autostart entry failed: {e}");
  }
}

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
    .inner_size(600.0, 440.0)
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
#[serde(rename_all = "camelCase")]
pub struct Settings {
  autostart: bool,
  version: String,
  #[serde(flatten)]
  prefs: Prefs,
}

#[tauri::command]
pub fn get_settings(app: AppHandle, window: Window) -> Result<Settings, String> {
  from_settings(&window)?;
  Ok(Settings {
    autostart: app.autolaunch().is_enabled().unwrap_or(false),
    version: app.package_info().version.to_string(),
    prefs: prefs(&app),
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

/// The on/off `Prefs`, by their camelCase name. Saved at once; read where they apply.
#[tauri::command]
pub fn set_setting(app: AppHandle, window: Window, key: String, value: bool) -> Result<(), String> {
  from_settings(&window)?;
  let state = app.state::<PrefsState>();
  let mut prefs = state.0.lock().unwrap_or_else(|e| e.into_inner());
  let mut next = prefs.clone();
  match key.as_str() {
    "notificationSound" => next.notification_sound = value,
    "notificationPreview" => next.notification_preview = value,
    "startHidden" => next.start_hidden = value,
    "autoUpdate" => next.auto_update = value,
    "closeToTray" => next.close_to_tray = value,
    _ => return Err(format!("unknown setting {key}")),
  }
  save(&app, &next)?;
  *prefs = next;
  Ok(())
}

/// "Gửi thông báo thử": the same path as a chat popup (sound and preview settings included), so
/// "nothing showed" means the OS blocks AMS, which `open_notification_settings` then fixes.
#[tauri::command]
pub async fn test_notification(app: AppHandle, window: Window) -> Result<(), String> {
  from_settings(&window)?;
  // A fresh tag each time: the duplicate guard must not swallow a second test.
  let tag = format!("test-{:?}", std::time::SystemTime::now());
  let body = "Đây là thông báo thử. Bạn thấy nó nghĩa là thông báo đã hoạt động.".to_string();
  crate::deliver_notification(app, "AMS".to_string(), body, tag).await;
  Ok(())
}

/// The OS page where AMS's notifications are allowed (or not).
#[tauri::command]
pub fn open_notification_settings(app: AppHandle, window: Window) -> Result<(), String> {
  from_settings(&window)?;
  #[cfg(target_os = "macos")]
  let url = "x-apple.systempreferences:com.apple.Notifications-Settings.extension";
  #[cfg(windows)]
  let url = "ms-settings:notifications";
  #[cfg(not(any(target_os = "macos", windows)))]
  return Err("unsupported".into());
  #[cfg(any(target_os = "macos", windows))]
  app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

/// The log files (lib.rs: log plugin, every build) for the user to send to IT.
#[tauri::command]
pub fn open_log_folder(app: AppHandle, window: Window) -> Result<(), String> {
  from_settings(&window)?;
  let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
  std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
  app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// "Xoá dữ liệu và đăng nhập lại", after a confirmation: cookies, cache and storage of the AMS
/// pages, then the main window starts over at the login. Returns whether it ran.
#[tauri::command]
pub async fn clear_data(app: AppHandle, window: Window) -> Result<bool, String> {
  from_settings(&window)?;
  let ask = app.clone();
  let confirmed = tauri::async_runtime::spawn_blocking(move || {
    ask
      .dialog()
      .message("AMS sẽ xoá cookie, bộ nhớ đệm và dữ liệu đã lưu của trang, rồi tải lại. Bạn sẽ phải đăng nhập lại.")
      .title("Xoá dữ liệu và đăng nhập lại")
      .kind(MessageDialogKind::Warning)
      .buttons(MessageDialogButtons::OkCancelCustom("Xoá và đăng nhập lại".into(), "Huỷ".into()))
      .blocking_show()
  })
  .await
  .map_err(|e| e.to_string())?;
  if !confirmed {
    return Ok(false);
  }
  crate::reset_web_data(&app).map_err(|e| e.to_string())?;
  Ok(true)
}

/// What the user reads; the updater's own (English) error goes to the log for IT.
fn update_error(error: tauri_plugin_updater::Error) -> String {
  log::error!("update check failed: {error}");
  match error {
    // latest.json missing (404: no release published yet) or unreadable.
    tauri_plugin_updater::Error::ReleaseNotFound => "Chưa có bản phát hành nào trên máy chủ cập nhật.".into(),
    _ => "Không kết nối được máy chủ cập nhật. Kiểm tra mạng rồi thử lại.".into(),
  }
}

#[derive(Serialize)]
pub struct UpdateCheck {
  /// The newer version found (the install prompt is then on screen), or None when up to date.
  available: Option<String>,
}

/// "Kiểm tra ngay": the same check and prompt as the 6-hourly one, whatever `auto_update` says.
#[tauri::command]
pub async fn check_update(app: AppHandle, window: Window) -> Result<UpdateCheck, String> {
  from_settings(&window)?;
  let Some(update) = crate::find_update(&app).await.map_err(update_error)? else {
    return Ok(UpdateCheck { available: None });
  };
  let available = Some(update.version.clone());
  // The prompt blocks until answered: its own thread, so the page gets its answer now.
  std::thread::spawn(move || crate::offer_update(&app, update));
  Ok(UpdateCheck { available })
}
