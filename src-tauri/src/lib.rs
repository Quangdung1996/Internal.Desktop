use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(target_os = "macos")]
use tauri::menu::{AboutMetadata, HELP_SUBMENU_ID};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::webview::{NewWindowFeatures, NewWindowResponse, PermissionKind, PermissionResponse};

#[cfg(target_os = "macos")]
mod location;
#[cfg(target_os = "macos")]
mod mac_notify;
mod settings;
mod title_bar;
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
use tauri::{
  AppHandle, Manager, Url, UserAttentionType, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};
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
    // Brand-teal title bar on every window (title_bar.rs).
    .plugin(tauri::plugin::Builder::<Wry, ()>::new("title-bar").on_window_ready(title_bar::paint).build())
    .invoke_handler(tauri::generate_handler![
      open_link,
      set_badge,
      notify,
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
      page_alive,
      page_leaving
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
      mac_notify::present_in_foreground();
      // A click on a notification does what the page's onclick does (on_notification_click).
      // ponytail: with a handler the plugin keeps one thread per notification waiting for its click
      // until it goes away (macOS: until cleared from Notification Center); fine at chat volume.
      let handle = app.handle().clone();
      app.notification().on_action(move |performed| {
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
          // macOS: hiding a fullscreen window leaves its Space open and black. Hide the app instead
          // (⌘H): macOS then leaves the Space as for any app; bring_forward unhides it.
          #[cfg(target_os = "macos")]
          if window.is_fullscreen().unwrap_or(false) {
            let _ = window.app_handle().hide();
            return;
          }
          let _ = window.hide();
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

fn show_main(app: &AppHandle) {
  if let Some(window) = app.get_webview_window(MAIN) {
    bring_forward(&window);
  }
}

fn bring_forward(window: &WebviewWindow) {
  // Undoes the app-level hide used when closing a fullscreen window (on_window_event).
  #[cfg(target_os = "macos")]
  let _ = window.app_handle().show();
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
  let builder = WebviewWindowBuilder::new(app, CHAT, WebviewUrl::External(url.clone()))
    .title("AMS Chat")
    .inner_size(1100.0, 760.0)
    .min_inner_size(720.0, 480.0)
    // Same as the main window: keep HTML5 file drop working for chat attachments on Windows.
    .disable_drag_drop_handler()
    .zoom_hotkeys_enabled(true);
  match with_handlers(builder, app).build() {
    // Windows: its own taskbar button gets the current count overlay.
    Ok(_) => {
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
fn main_url(app: &AppHandle) -> Option<Url> {
  let url = app.get_webview_window(MAIN).and_then(|w| w.url().ok());
  url.filter(is_site_page).or_else(|| site_url(app))
}

/// The configured site: frontendDist, or devUrl under `tauri dev`.
fn site_url(app: &AppHandle) -> Option<Url> {
  let build = &app.config().build;
  if tauri::is_dev() {
    build.dev_url.clone()
  } else if let Some(tauri::utils::config::FrontendDist::Url(url)) = &build.frontend_dist {
    Some(url.clone())
  } else {
    None
  }
}

/// `ams://<path>?<query>` (e.g. ams://chat/<roomId>) opens that page of the site: chat paths in the
/// chat window, the rest in the main window. Mirrors plugins.deep-link in tauri.conf.json. Nothing
/// emits these links yet: emails and the FE must generate them (plain https links open the browser).
const DEEP_LINK_SCHEME_PREFIX: &str = "ams:";

/// Only the path, query and fragment are taken: the origin is always the site's, so a crafted link
/// (ams:///evil.com/x) cannot load another site into an app window holding the session.
fn open_deep_link(app: &AppHandle, link: &Url) {
  if !link.as_str().starts_with(DEEP_LINK_SCHEME_PREFIX) {
    return;
  }
  let Some(mut target) = main_url(app) else {
    return;
  };
  // ams://chat/1 parses "chat" as the host, ams:///chat/1 does not: both mean /chat/1.
  let path = format!("{}{}", link.host_str().unwrap_or(""), link.path());
  target.set_path(&format!("/{}", path.trim_start_matches('/')));
  target.set_query(link.query());
  target.set_fragment(link.fragment());
  if is_chat_url(app, &target) {
    open_chat(app, Some(target));
  } else if let Some(main) = app.get_webview_window(MAIN) {
    let _ = main.navigate(target);
    bring_forward(&main);
  }
}

/// A page of the AMS site itself (not blob:, not the bundled pages).
fn is_site_page(url: &Url) -> bool {
  matches!(url.scheme(), "http" | "https") && is_app_url(url)
}

/// A main or chat page that never loads (offline at launch; on Windows a reload while offline) would
/// leave a blank or browser error page. Every document pings `page_alive` with its address
/// (DESKTOP_SCRIPT); building the window, leaving a page (`page_leaving`) or, on Windows, a load
/// starting (`with_handlers`) starts a wait. Without a ping from an AMS page within LOAD_TIMEOUT the
/// window is asked once more (PROBE_GRACE: a ping that raced a late `page_leaving`), then shows the
/// bundled offline page, which returns once the site answers. Detection lives in the page because
/// macOS reports no failed load (wry's Started fires on commit).
/// ponytail: a site slower than LOAD_TIMEOUT to answer lands there too; the page brings it back.
const LOAD_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_GRACE: Duration = Duration::from_secs(2);
const PROBE_SCRIPT: &str = r#"window.__TAURI_INTERNALS__?.invoke("page_alive", { url: location.href })"#;
static LOAD_WAITS: Mutex<Vec<(String, u64, Url)>> = Mutex::new(Vec::new());
static LOAD_SEQ: AtomicU64 = AtomicU64::new(0);

fn watch_load(app: &AppHandle, label: &str, fallback: Url) {
  if label != MAIN && label != CHAT {
    return;
  }
  let seq = LOAD_SEQ.fetch_add(1, Ordering::Relaxed);
  {
    let mut waits = LOAD_WAITS.lock().unwrap_or_else(|e| e.into_inner());
    waits.retain(|(l, _, _)| l != label);
    waits.push((label.to_string(), seq, fallback));
  }
  let app = app.clone();
  let label = label.to_string();
  std::thread::spawn(move || {
    // This wait, if no ping cleared or replaced it; `take` ends it.
    let still_waiting = |take: bool| {
      let mut waits = LOAD_WAITS.lock().unwrap_or_else(|e| e.into_inner());
      let at = waits.iter().position(|(l, s, _)| *l == label && *s == seq)?;
      Some(if take { waits.remove(at).2 } else { waits[at].2.clone() })
    };
    std::thread::sleep(LOAD_TIMEOUT);
    let Some(window) = app.get_webview_window(&label) else {
      return;
    };
    if still_waiting(false).is_none() {
      return;
    }
    let _ = window.eval(PROBE_SCRIPT);
    std::thread::sleep(PROBE_GRACE);
    let Some(fallback) = still_waiting(true) else {
      return;
    };
    // Windows keeps the failed address as the url. macOS keeps the previous page (no better than
    // `fallback`), and `url()` panics in wry when no page ever loaded (chat window opened offline).
    #[cfg(windows)]
    let to = window.url().ok().filter(is_site_page).unwrap_or(fallback);
    #[cfg(not(windows))]
    let to = fallback;
    log::warn!("{label}: no page within {LOAD_TIMEOUT:?}, showing the offline page");
    let _ = window.navigate(settings::offline_url(&to));
  });
}

/// DESKTOP_SCRIPT / PROBE_SCRIPT: a document is up in this window (`watch_load`). Only an AMS page or
/// the offline page counts: WebView2's error page and WKWebView's blank page run the script too.
#[tauri::command]
fn page_alive(window: WebviewWindow, url: String) {
  let Ok(url) = url.parse::<Url>() else {
    return;
  };
  if is_site_page(&url) || settings::offline_target(&url).is_some() {
    LOAD_WAITS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(l, _, _)| l != window.label());
  }
}

/// DESKTOP_SCRIPT on `pagehide`: this page is being left (`watch_load`). Leaving the offline page
/// means heading back to its `to`.
#[tauri::command]
fn page_leaving(app: AppHandle, window: WebviewWindow, url: String) {
  let Ok(url) = url.parse::<Url>() else {
    return;
  };
  let back_to = if is_site_page(&url) { Some(url) } else { settings::offline_target(&url) };
  if let Some(url) = back_to.filter(is_site_page) {
    watch_load(&app, window.label(), url);
  }
}

/// A /chat page of the FE itself (not a chat-api file URL that happens to share the path).
fn is_chat_url(app: &AppHandle, url: &Url) -> bool {
  (url.path() == "/chat" || url.path().starts_with("/chat/"))
    && main_url(app).is_some_and(|main| main.host_str() == url.host_str())
}

/// Built here instead of from tauri.conf.json ("create": false) to attach the popup and download handlers.
fn build_main_window(app: &AppHandle, visible: bool) -> tauri::Result<()> {
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
  with_handlers(builder, app).build()?;
  watch_load(app, MAIN, site);
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
  // A click on the OS notification runs the page's own onclick (the FE opens the room): `notify` passes clickId
  // along, Rust calls __amsNotificationClick(clickId) in this window. The last 50 are kept.
  const shown = new Map();
  let nextClickId = 0;
  function DesktopNotification(title, options) {
    const o = options || {};
    this.title = String(title ?? "");
    this.body = String(o.body ?? "");
    this.tag = o.tag == null ? "" : String(o.tag);
    this.onclick = null;
    this.onclose = null;
    const clickId = ++nextClickId;
    shown.set(clickId, this);
    if (shown.size > 50) shown.delete(shown.keys().next().value);
    invoke("notify", { title: this.title, body: this.body, tag: this.tag, clickId });
  }
  Object.defineProperty(window, "__amsNotificationClick", {
    value: (clickId) => {
      const n = shown.get(clickId);
      shown.delete(clickId);
      if (n && typeof n.onclick === "function") n.onclick.call(n, new Event("click"));
      // The page reloaded since (instance gone): at least open the chat, as the FE's own click would.
      else if (!n && !location.pathname.startsWith("/chat")) window.open("/chat", "ams-chat");
    },
  });
  DesktopNotification.prototype.close = function () {};
  Object.defineProperty(DesktopNotification, "permission", { get: () => "granted" });
  DesktopNotification.requestPermission = (callback) => {
    if (typeof callback === "function") callback("granted");
    return Promise.resolve("granted");
  };
  window.Notification = DesktopNotification;
  // Lost connection on a loaded page: keep the page (like a PWA), dim it with a warning until it is back.
  // `offline` only sees the network going down; a failed fetch also catches "Wi-Fi but no internet",
  // then the site is probed until it answers. The bundled start/offline page warns on its own.
  if (/^https?:$/.test(location.protocol) && location.hostname !== "amssettings.localhost") {
    let overlay = null;
    let probe = 0;
    const setOffline = (down) => {
      if (down === !!overlay) return;
      if (down) {
        // Dims and blurs the page (kept as is, nothing navigates) and holds clicks until it is back.
        overlay = document.createElement("div");
        overlay.setAttribute("role", "alert");
        overlay.style.cssText = "position:fixed;inset:0;z-index:2147483647;display:flex;justify-content:center;" +
          "align-items:flex-start;padding-top:24px;background:rgba(255,255,255,.45);" +
          "backdrop-filter:blur(3px) grayscale(.6);-webkit-backdrop-filter:blur(3px) grayscale(.6)";
        const note = document.createElement("div");
        note.textContent = "⚠ Mất kết nối internet. AMS tự kết nối lại khi có mạng.";
        note.style.cssText = "padding:10px 16px;border-radius:8px;background:#b45309;color:#fff;" +
          "font:600 14px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;box-shadow:0 2px 8px rgba(0,0,0,.25)";
        overlay.appendChild(note);
        (document.body || document.documentElement).appendChild(overlay);
        probe = setInterval(() => {
          if (navigator.onLine) fetchNative(location.origin + "/favicon.ico", { method: "HEAD", cache: "no-store" })
            .then(() => setOffline(false), () => {});
        }, 5000);
      } else {
        overlay.remove();
        overlay = null;
        clearInterval(probe);
      }
    };
    const fetchNative = window.fetch.bind(window);
    window.fetch = (...args) => fetchNative(...args).then(
      (response) => (setOffline(false), response),
      (error) => {
        // TypeError = network failure; AbortError (cancelled request) says nothing about the network.
        // Another server being down shows it too, until the probe of this site clears it (5 s).
        if (error instanceof TypeError) setOffline(true);
        throw error;
      },
    );
    addEventListener("offline", () => setOffline(true));
    addEventListener("online", () => setOffline(false));
    addEventListener("DOMContentLoaded", () => setOffline(!navigator.onLine));
  }
  // The offline watch (watch_load in lib.rs): this document loaded; leaving it starts a new wait.
  // pageshow too: a page restored from the back/forward cache runs no init script.
  invoke("page_alive", { url: location.href });
  addEventListener("pageshow", () => invoke("page_alive", { url: location.href }));
  addEventListener("pagehide", () => invoke("page_leaving", { url: location.href }));
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
async fn notify(app: AppHandle, window: WebviewWindow, title: String, body: String, tag: String, click_id: Option<u32>) {
  let origin = click_id.map(|id| (window.label().to_string(), id));
  deliver_notification(app, title, body, tag, origin).await;
}

/// Notification id → where it came from (window label, the page's clickId), so a click runs that
/// page's onclick. Bounded: a click on an older one falls back (`on_notification_click`).
static NOTIFICATION_ORIGINS: Mutex<Vec<(i32, String, u32)>> = Mutex::new(Vec::new());
static NOTIFICATION_SEQ: AtomicI32 = AtomicI32::new(1);
const NOTIFICATION_ORIGINS_KEPT: usize = 100;

/// A click on a notification (registered in setup through the plugin's `on_action`). Windows reports
/// it only while the toast is on screen, not from the Action Center.
fn on_notification_click(app: &AppHandle, id: Option<i32>) {
  let origin = id.and_then(|id| {
    let mut origins = NOTIFICATION_ORIGINS.lock().unwrap_or_else(|e| e.into_inner());
    let at = origins.iter().position(|(n, _, _)| *n == id)?;
    let (_, label, click_id) = origins.remove(at);
    Some((label, click_id))
  });
  // Settings ▸ "Gửi thông báo thử", or one too old to remember.
  let Some((label, click_id)) = origin else {
    show_main(app);
    return;
  };
  let Some(window) = app.get_webview_window(&label) else {
    open_chat(app, None);
    return;
  };
  // The chat page only switches rooms in place: bring its window up. The main window's onclick opens
  // the chat window itself (window.open → open_chat), so a hidden main window stays hidden.
  if label != MAIN {
    bring_forward(&window);
  }
  let _ = window.eval(format!("window.__amsNotificationClick?.({click_id})"));
}

/// `notify`'s work, shared with Settings ▸ "Gửi thông báo thử".
pub(crate) async fn deliver_notification(app: AppHandle, title: String, body: String, tag: String, origin: Option<(String, u32)>) {
  if is_duplicate(format!("{tag}\u{1f}{title}\u{1f}{body}")) {
    return;
  }
  // Settings ▸ Thông báo. The title (who wrote, which room) stays; only the message text is hidden.
  let prefs = settings::prefs(&app);
  let body = if prefs.notification_preview || body.is_empty() { body } else { "Có tin nhắn mới".to_string() };
  let id = NOTIFICATION_SEQ.fetch_add(1, Ordering::Relaxed);
  if let Some((label, click_id)) = origin {
    let mut origins = NOTIFICATION_ORIGINS.lock().unwrap_or_else(|e| e.into_inner());
    if origins.len() >= NOTIFICATION_ORIGINS_KEPT {
      origins.remove(0);
    }
    origins.push((id, label, click_id));
  }
  let mut builder = app.notification().builder().id(id).title(&title).body(&body);
  // No sound name = silent on both OSes (Windows marks the toast silent).
  if prefs.notification_sound
    && let Some(sound) = NOTIFICATION_SOUND
  {
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
  BADGE.store(count, Ordering::Relaxed);
  apply_badge(&app);
  let count = (count > 0).then_some(i64::from(count));
  if let Some(tray) = app.tray_by_id(MAIN) {
    let tooltip = count.map_or_else(|| "AMS".to_string(), |n| format!("AMS ({n})"));
    let _ = tray.set_tooltip(Some(tooltip));
  }
}

/// The last count from the page: the chat window, opened later, gets it too (Windows overlay).
static BADGE: AtomicU32 = AtomicU32::new(0);

/// macOS: the Dock badge (app-wide). Windows: no badge API; each taskbar button (main, chat) gets the
/// count drawn as its overlay icon.
fn apply_badge(app: &AppHandle) {
  let count = BADGE.load(Ordering::Relaxed);
  #[cfg(windows)]
  for label in [MAIN, CHAT] {
    if let Some(window) = app.get_webview_window(label) {
      let _ = window.set_overlay_icon((count > 0).then(|| badge_icon(count)));
    }
  }
  #[cfg(not(windows))]
  if let Some(window) = app.get_webview_window(MAIN) {
    let _ = window.set_badge_count((count > 0).then_some(i64::from(count)));
  }
}

/// Windows taskbar overlay: a red dot with the count in white, "99+" above 99. 32 px, drawn with a
/// 3×5 pixel font (no font or image crate needed).
#[cfg_attr(not(windows), allow(dead_code))]
fn badge_icon(count: u32) -> tauri::image::Image<'static> {
  const SIZE: usize = 32;
  // Rows of 3 bits, top to bottom: digits 0-9, then "+".
  const GLYPHS: [[u8; 5]; 11] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
    [0b000, 0b010, 0b111, 0b010, 0b000],
  ];
  let text: Vec<usize> = if count > 99 {
    vec![9, 9, 10]
  } else {
    count.to_string().bytes().map(|b| usize::from(b - b'0')).collect()
  };
  let scale = match text.len() {
    1 => 4,
    2 => 3,
    _ => 2,
  };
  let mut rgba = vec![0u8; SIZE * SIZE * 4];
  let center = (SIZE as f32 - 1.0) / 2.0;
  let radius = SIZE as f32 / 2.0;
  for y in 0..SIZE {
    for x in 0..SIZE {
      let distance = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
      let alpha = (radius - distance).clamp(0.0, 1.0);
      if alpha > 0.0 {
        let i = (y * SIZE + x) * 4;
        rgba[i..i + 4].copy_from_slice(&[0xE5, 0x39, 0x35, (alpha * 255.0) as u8]);
      }
    }
  }
  let gap = scale;
  let width = text.len() * 3 * scale + (text.len() - 1) * gap;
  let (left, top) = ((SIZE - width) / 2, (SIZE - 5 * scale) / 2);
  for (k, &glyph) in text.iter().enumerate() {
    let glyph_left = left + k * (3 * scale + gap);
    for (row, bits) in GLYPHS[glyph].iter().enumerate() {
      for col in 0..3 {
        if (bits >> (2 - col)) & 1 == 0 {
          continue;
        }
        for dy in 0..scale {
          for dx in 0..scale {
            let i = ((top + row * scale + dy) * SIZE + glyph_left + col * scale + dx) * 4;
            rgba[i..i + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
          }
        }
      }
    }
  }
  tauri::image::Image::new_owned(rgba, SIZE as u32, SIZE as u32)
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
  // Clicks are handled by the app-wide `on_menu_event` (`on_menu`), which also sees the macOS menu bar.
  let menu = Menu::with_items(
    app,
    &[
      &MenuItem::with_id(app, "open", "Mở AMS", true, None::<&str>)?,
      &MenuItem::with_id(app, "chat", "Mở Chat", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::with_id(app, SETTINGS_MENU, "Cài đặt…", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::with_id(app, "quit", "Thoát", true, None::<&str>)?,
    ],
  )?;
  tray.set_menu(Some(menu))?;
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

const SETTINGS_MENU: &str = "settings";
const CONTACT_MENU: &str = "contact";
/// Help ▸ contact (macOS) and the Settings footer (every OS) for bug reports.
const SUPPORT_EMAIL: &str = "dung.bui@agari.com.vn";

/// Every menu click, tray and macOS menu bar alike.
fn on_menu(app: &AppHandle, id: &str) {
  match id {
    "open" => show_main(app),
    "chat" => open_chat(app, None),
    SETTINGS_MENU => settings::open(app),
    CONTACT_MENU => {
      if let Err(e) = app.opener().open_url(format!("mailto:{SUPPORT_EMAIL}"), None::<&str>) {
        log::error!("opening the mail app failed: {e}");
      }
    }
    "quit" => app.exit(0),
    _ => {}
  }
}

/// macOS: the default menu bar (Edit keeps copy/paste working) plus "Cài đặt…" (Cmd+,) under the
/// app menu, after About, as Mac apps do. Windows has no menu bar: the tray has "Cài đặt…" instead.
#[cfg(target_os = "macos")]
fn setup_app_menu(app: &AppHandle) -> tauri::Result<()> {
  let menu = Menu::default(app)?;
  if let Some(app_menu) = menu.items()?.first().and_then(|item| item.as_submenu().cloned()) {
    let settings = MenuItem::with_id(app, SETTINGS_MENU, "Cài đặt…", true, Some("CmdOrCtrl+,"))?;
    app_menu.insert_items(&[&settings, &PredefinedMenuItem::separator(app)?], 2)?;
  }
  // Help (empty in the default menu): the version at a glance, who to contact, the standard About panel.
  if let Some(help) = menu.get(HELP_SUBMENU_ID).and_then(|item| item.as_submenu().cloned()) {
    let info = app.package_info();
    let version = info.version.to_string();
    let about = AboutMetadata { name: Some(info.name.clone()), version: Some(version.clone()), ..Default::default() };
    help.append_items(&[
      &MenuItem::new(app, format!("Phiên bản {version}"), false, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::with_id(app, CONTACT_MENU, format!("Gặp lỗi? Liên hệ: {SUPPORT_EMAIL}"), true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &PredefinedMenuItem::about(app, Some("Giới thiệu AMS"), Some(about))?,
    ])?;
  }
  app.set_menu(menu)?;
  Ok(())
}

/// Settings ▸ Cập nhật ▸ "Tự động kiểm tra cập nhật" is read on every round, so turning it off or on
/// takes effect at the next check without a restart.
fn spawn_updater(app: AppHandle) {
  std::thread::spawn(move || {
    loop {
      if settings::prefs(&app).auto_update {
        match tauri::async_runtime::block_on(find_update(&app)) {
          Ok(Some(update)) => offer_update(&app, update),
          Ok(None) => {}
          Err(e) => log::error!("update check failed: {e}"),
        }
      }
      std::thread::sleep(UPDATE_EVERY);
    }
  });
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
  main.navigate(home)?;
  set_badge(app.clone(), 0);
  bring_forward(&main);
  Ok(())
}

/// A newer published release, if any (also "Kiểm tra ngay" in Settings).
pub(crate) async fn find_update(app: &AppHandle) -> tauri_plugin_updater::Result<Option<Update>> {
  app.updater()?.check().await
}

/// Asks first: installing restarts the app (on Windows the installer closes it). Blocks on the
/// dialog: call it from a plain thread, never from the main thread or an async task.
pub(crate) fn offer_update(app: &AppHandle, update: Update) {
  let accepted = app
    .dialog()
    .message(format!("Đã có AMS phiên bản {}. Cập nhật và khởi động lại ngay?", update.version))
    .title("Cập nhật AMS")
    .buttons(MessageDialogButtons::OkCancelCustom("Cập nhật".into(), "Để sau".into()))
    .blocking_show();
  if !accepted {
    return;
  }
  match tauri::async_runtime::block_on(update.download_and_install(|_, _| {}, || {})) {
    Ok(()) => app.restart(),
    Err(e) => log::error!("installing the update failed: {e}"),
  }
}
