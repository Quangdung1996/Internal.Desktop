//! The app windows: main (opens on the bundled start page), chat (one, like a messenger),
//! popups; show / hide to the tray; the handlers every window gets (`with_handlers`).

pub(crate) mod title_bar;

use std::sync::atomic::AtomicU32;
use tauri::webview::{PermissionKind, PermissionResponse};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};
#[cfg(windows)]
use tauri::{PhysicalPosition, Window};
use crate::downloads;
use crate::links::{is_app_url, link_script, open_new_window};
#[cfg(windows)]
use crate::menu::window_menu;
use crate::notifications::{apply_badge, set_badge};
use crate::offline::watch_load;
use crate::settings;
#[cfg(target_os = "macos")]
use crate::location;

pub(crate) const MAIN: &str = "main";

/// The single chat window (see `open_chat`).
pub(crate) const CHAT: &str = "chat";

/// capabilities/default.json grants "chat" and "popup-*" the same page permissions as the main window.
pub(crate) const POPUP_PREFIX: &str = "popup-";

pub(crate) static POPUP_SEQ: AtomicU32 = AtomicU32::new(0);

pub(crate) type WindowBuilder<'a> = WebviewWindowBuilder<'a, Wry, AppHandle>;

/// ⌥⌘A (macOS) / Ctrl+Alt+A (Windows).
pub(crate) const TOGGLE_SHORTCUT: &str = "CommandOrControl+Alt+A";

/// The shortcut: AMS in front goes to the tray, otherwise it comes forward.
pub(crate) fn toggle_main(app: &AppHandle) {
  let Some(window) = app.get_webview_window(MAIN) else {
    return;
  };
  if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
    hide_to_tray(app);
  } else {
    bring_forward(&window);
  }
}

/// The main window to the tray (close with "close to tray" on, or the shortcut).
pub(crate) fn hide_to_tray(app: &AppHandle) {
  let Some(window) = app.get_webview_window(MAIN) else {
    return;
  };
  // macOS: hiding a fullscreen window leaves its Space open and black. Hide the app instead
  // (⌘H): macOS then leaves the Space as for any app; bring_forward unhides it.
  #[cfg(target_os = "macos")]
  if window.is_fullscreen().unwrap_or(false) {
    let _ = window.app_handle().hide();
    return;
  }
  let _ = window.hide();
}

pub(crate) fn show_main(app: &AppHandle) {
  if let Some(window) = app.get_webview_window(MAIN) {
    bring_forward(&window);
  }
}

pub(crate) fn bring_forward(window: &WebviewWindow) {
  // Undoes the app-level hide used when closing a fullscreen window (on_window_event).
  #[cfg(target_os = "macos")]
  let _ = window.app_handle().show();
  log::info!(
    "bring forward {}: minimized {:?}, visible {:?}, focused {:?}",
    window.label(),
    window.is_minimized().ok(),
    window.is_visible().ok(),
    window.is_focused().ok()
  );
  let _ = window.unminimize();
  let _ = window.show();
  #[cfg(windows)]
  pull_on_screen(&window.as_ref().window());
  // Windows' foreground lock: SetForegroundWindow from an app that is not in front (back from
  // another app, a toast, the tray) only flashes the taskbar button and the window stays behind.
  // Topmost for a moment raises it regardless; set_focus then gives it the keyboard when allowed.
  #[cfg(windows)]
  let _ = window.set_always_on_top(true);
  let _ = window.set_focus();
  #[cfg(windows)]
  let _ = window.set_always_on_top(false);
}

#[cfg(windows)]
const MIN_REACHABLE_TITLE_BAR_WIDTH: i32 = 100;
#[cfg(windows)]
const TITLE_BAR_STRIP_HEIGHT: i32 = 40;
#[cfg(windows)]
const MIN_REACHABLE_TITLE_BAR_HEIGHT: i32 = 20;

#[cfg(windows)]
fn overlap(start_a: i32, end_a: i32, start_b: i32, end_b: i32) -> i32 {
  end_a.min(end_b) - start_a.max(start_b)
}

#[cfg(windows)]
pub(crate) fn pull_on_screen(window: &Window) {
  let placed_by_os = [window.is_minimized(), window.is_maximized(), window.is_fullscreen()]
    .into_iter()
    .any(|state| state.unwrap_or(true));
  if placed_by_os {
    return;
  }
  let (Ok(position), Ok(size), Ok(monitors)) = (window.outer_position(), window.outer_size(), window.available_monitors())
  else {
    return;
  };
  let title_bar_reachable = monitors.iter().any(|monitor| {
    let area = monitor.work_area();
    let (left, top) = (area.position.x, area.position.y);
    let (right, bottom) = (left + area.size.width as i32, top + area.size.height as i32);
    overlap(position.x, position.x + size.width as i32, left, right) >= MIN_REACHABLE_TITLE_BAR_WIDTH
      && overlap(position.y, position.y + TITLE_BAR_STRIP_HEIGHT, top, bottom) >= MIN_REACHABLE_TITLE_BAR_HEIGHT
  });
  if title_bar_reachable {
    return;
  }
  let Ok(Some(primary)) = window.primary_monitor() else {
    return;
  };
  let area = primary.work_area();
  let centered = |start: i32, available: u32, length: u32| start + ((available as i32 - length as i32) / 2).max(0);
  log::warn!("{} off screen at {position:?}: moved to the primary monitor", window.label());
  let _ = window.set_position(PhysicalPosition {
    x: centered(area.position.x, area.size.width, size.width),
    y: centered(area.position.y, area.size.height, size.height),
  });
}

/// The chat as its own window, like a desktop messenger. The FE opens the full chat in a new tab
/// (sidebar `target="_blank"`, "open in Chat" `window.open(.., "ams-chat")`); here all of them land in
/// this one window. A bare /chat only brings it forward (keeps the open room); a room link navigates it.
/// `url` is None from the tray.
pub(crate) fn open_chat(app: &AppHandle, url: Option<Url>) {
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
  let builder = WebviewWindowBuilder::new(app, CHAT, WebviewUrl::External(url.clone()))
    .title("AMS Chat")
    .inner_size(1100.0, 760.0)
    .min_inner_size(720.0, 480.0)
    // Same as the main window: keep HTML5 file drop working for chat attachments on Windows.
    .disable_drag_drop_handler()
    .zoom_hotkeys_enabled(true);
  #[cfg(windows)]
  let builder = match window_menu(app) {
    Ok(menu) => builder.menu(menu),
    Err(e) => {
      log::error!("building the chat menu bar failed: {e}");
      builder
    }
  };
  match with_handlers(builder, app).build() {
    // Windows: its own taskbar button gets the current count overlay.
    Ok(_chat) => {
      #[cfg(windows)]
      title_bar::paint_menu_bar(&_chat);
      apply_badge(app);
      watch_load(app, CHAT, url);
    }
    Err(e) => log::error!("opening the chat window failed: {e}"),
  }
}

/// Where the main window is: the prod site, or the dev server under `tauri dev`. While it shows the
/// start/offline page (or before it exists), the configured site, so chat and reset links keep the
/// site's origin. macOS: `url()` panics in wry on a window that never loaded a page; the main window
/// always starts on the bundled start page, so it has one.
pub(crate) fn main_url(app: &AppHandle) -> Option<Url> {
  let url = app.get_webview_window(MAIN).and_then(|w| w.url().ok());
  url.filter(is_site_page).or_else(|| site_url(app))
}

/// The configured site: frontendDist, or devUrl under `tauri dev`.
pub(crate) fn site_url(app: &AppHandle) -> Option<Url> {
  let build = &app.config().build;
  if tauri::is_dev() {
    build.dev_url.clone()
  } else if let Some(tauri::utils::config::FrontendDist::Url(url)) = &build.frontend_dist {
    Some(url.clone())
  } else {
    None
  }
}

/// The start page's own check would override a navigation sent while it is up (a deep link on a cold
/// start): point the start page at `url` instead, it goes there once the site answers.
pub(crate) fn navigate_main(main: &WebviewWindow, url: Url) -> tauri::Result<()> {
  let on_start_page = main.url().ok().and_then(|u| settings::offline_target(&u)).is_some();
  main.navigate(if on_start_page { settings::offline_url(&url) } else { url })
}

/// A page of the AMS site itself (not blob:, not the bundled pages).
pub(crate) fn is_site_page(url: &Url) -> bool {
  matches!(url.scheme(), "http" | "https") && is_app_url(url)
}

/// A /chat page of the FE itself (not a chat-api file URL that happens to share the path).
pub(crate) fn is_chat_url(app: &AppHandle, url: &Url) -> bool {
  (url.path() == "/chat" || url.path().starts_with("/chat/"))
    && main_url(app).is_some_and(|main| main.host_str() == url.host_str())
}

/// Built here instead of from tauri.conf.json ("create": false) to attach the popup and download handlers.
pub(crate) fn build_main_window(app: &AppHandle, visible: bool) -> tauri::Result<()> {
  let config = app
    .config()
    .app
    .windows
    .iter()
    .find(|w| w.label == MAIN)
    .expect("main window declared in tauri.conf.json");
  let Some(site) = site_url(app) else {
    return Ok(());
  };
  // Opens on the bundled start page (logo + loading bar, instant), which goes on to the site once it
  // answers and warns while offline. It stays painted until the site's first page commits.
  let start = settings::offline_url(&site);
  let mut config = config.clone();
  config.url = if matches!(start.scheme(), "http" | "https") {
    WebviewUrl::External(start)
  } else {
    WebviewUrl::CustomProtocol(start)
  };
  // Hidden or not, the page loads: chat, notifications and the badge run from it.
  let builder = WebviewWindowBuilder::from_config(app, &config)?.visible(visible);
  #[cfg(windows)]
  let builder = builder.menu(window_menu(app)?);
  let _main = with_handlers(builder, app).build()?;
  #[cfg(windows)]
  title_bar::paint_menu_bar(&_main);
  watch_load(app, MAIN, site);
  Ok(())
}

/// `window.desktopApp` is the FE's "running in the desktop app" flag: absent in a browser, so FE code
/// behind `window.desktopApp?.` never runs on the web.
pub(crate) const DESKTOP_SCRIPT: &str = include_str!("desktop.js");

pub(crate) fn with_handlers<'a>(builder: WindowBuilder<'a>, app: &AppHandle) -> WindowBuilder<'a> {
  let app = app.clone();
  let builder = builder
    .initialization_script(DESKTOP_SCRIPT)
    .initialization_script(link_script())
    // Windows (WebView2): AMS pages get location (attendance check-in) without WebView2's own prompt;
    // Windows still applies its Location privacy setting. macOS never reaches here (wry does not
    // forward geolocation there): GEO_SCRIPT + location.rs answer instead.
    .on_permission_request(|webview, kind| match kind {
      PermissionKind::Geolocation if webview.url().is_ok_and(|url| is_app_url(&url)) => PermissionResponse::Allow,
      _ => PermissionResponse::Default,
    })
    .on_new_window(move |url, features| open_new_window(&app, url, features));
  // Windows: every load (error pages too) starts the offline watch; the `pagehide` ping alone can be
  // lost at unload. macOS reports only loads that commit, so there the page pings are all it has.
  #[cfg(windows)]
  let builder = builder.on_page_load(|window, payload| {
    if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) && is_site_page(payload.url()) {
      watch_load(window.app_handle(), window.label(), payload.url().clone());
    }
  });
  #[cfg(target_os = "macos")]
  let builder = builder.initialization_script(location::GEO_SCRIPT);
  builder.on_download(downloads::on_download)
}

/// Settings ▸ "Xoá dữ liệu và đăng nhập lại": the chat window and popups close, the shared web data
/// (cookies, cache, storage: one store for all windows on both OSes) is cleared, and the main
/// window reloads the site's home, which sends a signed-out user to the login.
pub(crate) fn reset_web_data(app: &AppHandle) -> tauri::Result<()> {
  for (label, window) in app.webview_windows() {
    if label == CHAT || label.starts_with(POPUP_PREFIX) {
      let _ = window.destroy();
    }
  }
  let Some(main) = app.get_webview_window(MAIN) else {
    return Ok(());
  };
  main.clear_all_browsing_data()?;
  let Some(mut home) = main_url(app) else {
    return Ok(());
  };
  home.set_path("/");
  home.set_query(None);
  home.set_fragment(None);
  navigate_main(&main, home)?;
  set_badge(app.clone(), 0);
  bring_forward(&main);
  Ok(())
}
