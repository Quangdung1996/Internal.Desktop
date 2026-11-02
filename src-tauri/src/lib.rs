use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{
  AppHandle, Manager, Url, UserAttentionType, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_window_state::StateFlags;

const MAIN: &str = "main";
/// The single chat window (see `open_chat`).
const CHAT: &str = "chat";
/// capabilities/default.json grants "chat" and "popup-*" the same page permissions as the main window.
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
        .with_filter(|label| label == MAIN || label == CHAT)
        .build(),
    )
    .invoke_handler(tauri::generate_handler![open_link, set_badge, notify])
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
    bring_forward(&window);
  }
}

fn bring_forward(window: &WebviewWindow) {
  let _ = window.unminimize();
  let _ = window.show();
  let _ = window.set_focus();
}

/// The chat as its own window, like a desktop messenger. The FE opens the full chat in a new tab
/// (sidebar `target="_blank"`, "open in Chat" `window.open(.., "ams-chat")`); here all of them land in
/// this one window. A bare /chat only brings it forward (keeps the open room); a room link navigates it.
/// `url` is None from the tray.
fn open_chat(app: &AppHandle, url: Option<Url>) {
  if let Some(window) = app.get_webview_window(CHAT) {
    if let Some(url) = url.filter(|u| u.path() != "/chat" || u.query().is_some()) {
      let _ = window.navigate(url);
    }
    bring_forward(&window);
    return;
  }
  let Some(url) = url.or_else(|| main_url(app)?.join("/chat").ok()) else {
    return;
  };
  let builder = WebviewWindowBuilder::new(app, CHAT, WebviewUrl::External(url))
    .title("AMS Chat")
    .inner_size(1100.0, 760.0)
    .min_inner_size(720.0, 480.0)
    // Same as the main window: keep HTML5 file drop working for chat attachments on Windows.
    .disable_drag_drop_handler()
    .zoom_hotkeys_enabled(true);
  if let Err(e) = with_handlers(builder, app).build() {
    log::error!("opening the chat window failed: {e}");
  }
}

/// Where the main window is: the prod site, or the dev server under `tauri dev`.
fn main_url(app: &AppHandle) -> Option<Url> {
  app.get_webview_window(MAIN)?.url().ok()
}

/// A /chat page of the FE itself (not a chat-api file URL that happens to share the path).
fn is_chat_url(app: &AppHandle, url: &Url) -> bool {
  (url.path() == "/chat" || url.path().starts_with("/chat/"))
    && main_url(app).is_some_and(|main| main.host_str() == url.host_str())
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

/// WKWebView never reports `<a target="_blank" rel="noopener">` (the FE sidebar Chat link) to
/// `on_new_window`: the click is dropped silently. This script catches such clicks, and chat
/// `window.open` calls, and hands them to `open_link`. Without IPC the page navigates in place.
const LINK_SCRIPT: &str = r#"(() => {
  const isChat = (u) => u.origin === location.origin && (u.pathname === "/chat" || u.pathname.startsWith("/chat/"));
  const openLink = (href) => {
    const ipc = window.__TAURI_INTERNALS__;
    if (!ipc) { location.assign(href); return; }
    ipc.invoke("open_link", { url: href }).catch(() => location.assign(href));
  };
  document.addEventListener("click", (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest('a[target="_blank"]') : null;
    if (!a || !a.href || a.hasAttribute("download")) return;
    let url;
    try { url = new URL(a.href, location.href); } catch { return; }
    if (url.protocol !== "http:" && url.protocol !== "https:") return; // blob: stays with on_new_window
    e.preventDefault();
    openLink(url.href);
  }, true);
  const nativeOpen = window.open;
  window.open = function (href, ...rest) {
    try {
      const url = new URL(String(href ?? ""), location.href);
      if (isChat(url)) {
        if (isChat(new URL(location.href))) { location.assign(url.href); return window; }
        openLink(url.href);
        return null;
      }
    } catch {}
    return nativeOpen.call(window, href, ...rest);
  };
})();"#;

/// `window.desktopApp` is the FE's "running in the desktop app" flag: absent in a browser, so FE code
/// behind `window.desktopApp?.` never runs on the web.
const DESKTOP_SCRIPT: &str = r#"(() => {
  if (window.desktopApp) return;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__?.invoke(cmd, args).catch(() => {});
  Object.defineProperty(window, "desktopApp", {
    value: Object.freeze({
      /** Dock (macOS) / tray tooltip count; 0 clears it. */
      setBadge: (count) => invoke("set_badge", { count: Math.max(0, Math.floor(Number(count) || 0)) }),
    }),
  });
  // Replaces the notification plugin's window.Notification (its script runs before this one): the FE's chat popups
  // (`new Notification(title, { body, tag })`) go to `notify`, which adds the sound, the de-duplication across the
  // main and chat windows, and the Dock bounce / taskbar flash. Desktop apps need no permission: always granted.
  function DesktopNotification(title, options) {
    const o = options || {};
    this.title = String(title ?? "");
    this.body = String(o.body ?? "");
    this.tag = o.tag == null ? "" : String(o.tag);
    this.onclick = null;
    this.onclose = null;
    invoke("notify", { title: this.title, body: this.body, tag: this.tag });
  }
  DesktopNotification.prototype.close = function () {};
  Object.defineProperty(DesktopNotification, "permission", { get: () => "granted" });
  DesktopNotification.requestPermission = (callback) => {
    if (typeof callback === "function") callback("granted");
    return Promise.resolve("granted");
  };
  window.Notification = DesktopNotification;
})();"#;

/// One popup per message: the main window (RMS pages) and the chat window both receive it, both
/// call `notify` with the same title, body and tag within a moment.
static RECENT_NOTIFICATIONS: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
const DUPLICATE_WINDOW: Duration = Duration::from_secs(3);

/// The OS default sound: notify-rust plays none without a name, and Windows then marks the toast silent.
#[cfg(target_os = "macos")]
const NOTIFICATION_SOUND: Option<&str> = Some("NSUserNotificationDefaultSoundName");
#[cfg(windows)]
const NOTIFICATION_SOUND: Option<&str> = Some("Default");
#[cfg(not(any(target_os = "macos", windows)))]
const NOTIFICATION_SOUND: Option<&str> = None;

fn is_duplicate(key: String) -> bool {
  let now = Instant::now();
  let mut recent = RECENT_NOTIFICATIONS.lock().unwrap_or_else(|e| e.into_inner());
  recent.retain(|(_, at)| now.duration_since(*at) < DUPLICATE_WINDOW);
  if recent.iter().any(|(k, _)| *k == key) {
    return true;
  }
  recent.push((key, now));
  false
}

/// Called by the page's `new Notification(..)`: an OS notification with sound, and the app asks
/// for attention when none of its windows is in front. async: notify-rust blocks on macOS.
#[tauri::command]
async fn notify(app: AppHandle, title: String, body: String, tag: String) {
  if is_duplicate(format!("{tag}\u{1f}{title}\u{1f}{body}")) {
    return;
  }
  let mut builder = app.notification().builder().title(&title).body(&body);
  if let Some(sound) = NOTIFICATION_SOUND {
    builder = builder.sound(sound);
  }
  if let Err(e) = builder.show() {
    log::error!("showing a notification failed: {e}");
  }
  let in_front = app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false));
  if !in_front {
    let window = app.get_webview_window(CHAT).or_else(|| app.get_webview_window(MAIN));
    if let Some(window) = window {
      let _ = window.request_user_attention(Some(UserAttentionType::Informational));
    }
  }
}

/// Called by `window.desktopApp.setBadge`. Windows has no badge API (`set_badge_count` is a no-op
/// error there); the tray tooltip carries the count on every OS.
#[tauri::command]
fn set_badge(app: AppHandle, count: u32) {
  let count = (count > 0).then_some(i64::from(count));
  if let Some(window) = app.get_webview_window(MAIN) {
    let _ = window.set_badge_count(count);
  }
  if let Some(tray) = app.tray_by_id(MAIN) {
    let tooltip = count.map_or_else(|| "AMS".to_string(), |n| format!("AMS ({n})"));
    let _ = tray.set_tooltip(Some(tooltip));
  }
}

/// Called by LINK_SCRIPT: the same routing as `open_new_window`, minus window.opener (the FE links
/// it covers are noopener). async: building a window inside a sync command deadlocks on Windows.
#[tauri::command]
async fn open_link(app: AppHandle, url: String) -> Result<(), String> {
  let url: Url = url.parse().map_err(|e| format!("{e}"))?;
  if !is_app_url(&url) {
    return app.opener().open_url(url.as_str(), None::<&str>).map_err(|e| e.to_string());
  }
  if is_chat_url(&app, &url) {
    open_chat(&app, Some(url));
    return Ok(());
  }
  let label = format!("{POPUP_PREFIX}{}", POPUP_SEQ.fetch_add(1, Ordering::Relaxed));
  let builder = WebviewWindowBuilder::new(&app, label, WebviewUrl::External(url))
    .title("AMS")
    .inner_size(1100.0, 800.0)
    .on_document_title_changed(|window, title| {
      let _ = window.set_title(&title);
    });
  with_handlers(builder, &app).build().map(|_| ()).map_err(|e| e.to_string())
}

fn with_handlers<'a>(builder: WindowBuilder<'a>, app: &AppHandle) -> WindowBuilder<'a> {
  let app = app.clone();
  let builder = builder
    .initialization_script(DESKTOP_SCRIPT)
    .initialization_script(LINK_SCRIPT)
    .on_new_window(move |url, features| open_new_window(&app, url, features));
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
  if is_chat_url(app, &url) {
    open_chat(app, Some(url));
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
      &MenuItem::with_id(app, "chat", "Mở Chat", true, None::<&str>)?,
      &CheckMenuItem::with_id(app, "autostart", "Khởi động cùng máy", true, autostart, None::<&str>)?,
      &MenuItem::with_id(app, "quit", "Thoát", true, None::<&str>)?,
    ],
  )?;
  tray.set_menu(Some(menu))?;
  tray.on_menu_event(|app, event| match event.id.as_ref() {
    "open" => show_main(app),
    "chat" => open_chat(app, None),
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
