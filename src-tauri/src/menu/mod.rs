//! Menus: the macOS menu bar, the Windows window menu bar, the tray; their clicks (`on_menu`).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;
#[cfg(target_os = "macos")]
use tauri::menu::{AboutMetadata, HELP_SUBMENU_ID};
use crate::offline::LOADED;
use crate::window::{MAIN, is_site_page, open_chat, show_main};
use crate::settings;

/// Trợ giúp: the version and who to contact. In the tray (every OS) and the Windows menu bar.
pub(crate) fn help_submenu(app: &AppHandle) -> tauri::Result<Submenu<Wry>> {
  Submenu::with_items(
    app,
    "Trợ giúp",
    true,
    &[
      &MenuItem::with_id(app, GUIDE_MENU, "Hướng dẫn sử dụng", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::new(app, format!("Phiên bản {}", app.package_info().version), false, None::<&str>)?,
      &MenuItem::with_id(app, CONTACT_MENU, format!("Gặp lỗi? Liên hệ: {SUPPORT_EMAIL}"), true, None::<&str>)?,
    ],
  )
}

/// Xem: browser-like navigation, as the app has no address bar or back button. macOS: in the
/// default View menu, with ⌘R / ⌘[ / ⌘] (WKWebView has none). Windows: WebView2 already handles F5
/// and Alt+←/→ itself, so the items only show those keys (after a tab, the Win32 shortcut column)
/// instead of registering them a second time (Back would go back twice).
pub(crate) fn view_items(app: &AppHandle) -> tauri::Result<Vec<MenuItem<Wry>>> {
  let mac = cfg!(target_os = "macos");
  let item = |id: &str, text: &str, mac_key: &str, win_key: &str| {
    let (text, accelerator) = match (mac, win_key.is_empty()) {
      (true, _) => (text.to_string(), (!mac_key.is_empty()).then_some(mac_key)),
      (false, true) => (text.to_string(), None),
      (false, false) => (format!("{text}\t{win_key}"), None),
    };
    MenuItem::with_id(app, id, text, true, accelerator)
  };
  Ok(vec![
    item(RELOAD_MENU, "Tải lại trang", "CmdOrCtrl+R", "F5")?,
    item(BACK_MENU, "Quay lại", "CmdOrCtrl+[", "Alt+←")?,
    item(FORWARD_MENU, "Tiến tới", "CmdOrCtrl+]", "Alt+→")?,
    item(OPEN_IN_BROWSER_MENU, "Mở trang này bằng trình duyệt", "", "")?,
    item(COPY_LINK_MENU, "Sao chép link trang", "", "")?,
  ])
}

/// Xem ▸ … on the window in front (main if none is).
pub(crate) fn navigate_menu(app: &AppHandle, id: &str) {
  let windows = app.webview_windows();
  let Some(window) = windows
    .values()
    .find(|w| w.is_focused().unwrap_or(false))
    .or_else(|| windows.get(MAIN))
  else {
    return;
  };
  match id {
    RELOAD_MENU => {
      let _ = window.reload();
    }
    BACK_MENU => {
      let _ = window.eval("history.back()");
    }
    FORWARD_MENU => {
      let _ = window.eval("history.forward()");
    }
    _ => {
      let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|l| l == window.label());
      // The AMS page shown, or the one the start/offline page is heading to; never a bundled page.
      let Some(url) = loaded
        .then(|| window.url().ok())
        .flatten()
        .and_then(|url| if is_site_page(&url) { Some(url) } else { settings::offline_target(&url) })
      else {
        return;
      };
      if id == OPEN_IN_BROWSER_MENU {
        if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
          log::error!("opening {url} in the browser failed: {e}");
        }
      } else {
        let done = copy_text(url.as_str()).is_ok();
        let text = if done { "✓ Đã sao chép link trang" } else { "⚠ Không sao chép được link" };
        let _ = window.eval(format!("{COPIED_SCRIPT}({text:?})"));
      }
    }
  }
}

/// A short notice in the page after Xem ▸ "Sao chép link trang".
pub(crate) const COPIED_SCRIPT: &str = r#"((text) => {
  const el = document.createElement("div");
  el.setAttribute("role", "status");
  el.textContent = text;
  el.style.cssText = "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:2147483646;" +
    "padding:10px 16px;border-radius:8px;background:rgba(17,24,39,.9);color:#fff;" +
    "font:500 13px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;pointer-events:none";
  (document.body || document.documentElement).appendChild(el);
  setTimeout(() => el.remove(), 2000);
})"#;

/// The system clipboard via the OS tool (pbcopy / clip): no extra dependency for one string.
/// A Url is ASCII (percent-encoded), so clip.exe's code page does not matter.
pub(crate) fn copy_text(text: &str) -> std::io::Result<()> {
  use std::io::Write;
  #[cfg(target_os = "macos")]
  let mut command = std::process::Command::new("pbcopy");
  #[cfg(windows)]
  let mut command = {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("clip");
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    command
  };
  #[cfg(not(any(target_os = "macos", windows)))]
  return Err(std::io::Error::other("unsupported"));
  #[cfg(any(target_os = "macos", windows))]
  {
    let mut child = command.stdin(std::process::Stdio::piped()).spawn()?;
    child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?.write_all(text.as_bytes())?;
    if child.wait()?.success() { Ok(()) } else { Err(std::io::Error::other("copy failed")) }
  }
}

/// Windows: a menu bar under the title bar of the main and chat windows ("AMS", "Xem", "Trợ giúp"), as
/// Windows apps have; popups and Settings keep none. macOS uses the screen menu bar
/// (`setup_app_menu`). A new Menu per window: each window owns its menu bar. Compiled on every OS
/// so a macOS build type-checks it; only Windows attaches it.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn window_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
  let ams = Submenu::with_items(
    app,
    "AMS",
    true,
    &[
      &MenuItem::with_id(app, "open", "Mở AMS", true, None::<&str>)?,
      &MenuItem::with_id(app, "chat", "Mở Chat", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      // No Ctrl+, here: WebView2 keeps keyboard focus, so a menu-bar shortcut may never fire.
      &MenuItem::with_id(app, SETTINGS_MENU, "Cài đặt…", true, None::<&str>)?,
      &MenuItem::with_id(app, DOWNLOAD_DIR_MENU, "Thư mục lưu file tải về…", true, None::<&str>)?,
      &MenuItem::with_id(app, OPEN_DOWNLOADS_MENU, "Mở thư mục tải về", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::with_id(app, "quit", "Thoát", true, None::<&str>)?,
    ],
  )?;
  let items = view_items(app)?;
  let view = Submenu::with_items(
    app,
    "Xem",
    true,
    &[
      &items[0],
      &items[1],
      &items[2],
      &PredefinedMenuItem::separator(app)?,
      &items[3],
      &items[4],
    ],
  )?;
  Menu::with_items(app, &[&ams, &view, &help_submenu(app)?])
}

/// The tray icon itself comes from tauri.conf.json (app.trayIcon): the Tauri CLI keeps the
/// `tray-icon` Cargo feature in sync with that key. Only the menu and clicks are wired here.
pub(crate) fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
  let Some(tray) = app.tray_by_id(MAIN) else {
    return Ok(());
  };
  // Clicks are handled by the app-wide `on_menu_event` (`on_menu`), which also sees the window and
  // macOS menu bars.
  let help = help_submenu(app)?;
  let menu = Menu::with_items(
    app,
    &[
      &MenuItem::with_id(app, "open", "Mở AMS", true, None::<&str>)?,
      &MenuItem::with_id(app, "chat", "Mở Chat", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
      &MenuItem::with_id(app, SETTINGS_MENU, "Cài đặt…", true, None::<&str>)?,
      &help,
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

pub(crate) const SETTINGS_MENU: &str = "settings";

pub(crate) const CONTACT_MENU: &str = "contact";

pub(crate) const DOWNLOAD_DIR_MENU: &str = "download-dir";

pub(crate) const GUIDE_MENU: &str = "guide";

pub(crate) const RELOAD_MENU: &str = "reload";

pub(crate) const BACK_MENU: &str = "back";

pub(crate) const FORWARD_MENU: &str = "forward";

pub(crate) const OPEN_IN_BROWSER_MENU: &str = "open-in-browser";

pub(crate) const COPY_LINK_MENU: &str = "copy-link";

pub(crate) const OPEN_DOWNLOADS_MENU: &str = "open-downloads";

/// Help ▸ contact (macOS), tray ▸ Trợ giúp and the Settings footer (every OS) for bug reports.
pub(crate) const SUPPORT_EMAIL: &str = "dung.bui@agari.com.vn";

/// Every menu click, tray and macOS menu bar alike.
pub(crate) fn on_menu(app: &AppHandle, id: &str) {
  match id {
    "open" => show_main(app),
    "chat" => open_chat(app, None),
    SETTINGS_MENU => settings::open(app),
    GUIDE_MENU => settings::open_guide(app),
    DOWNLOAD_DIR_MENU => settings::choose_download_dir(app),
    OPEN_DOWNLOADS_MENU => settings::open_download_dir(app),
    RELOAD_MENU | BACK_MENU | FORWARD_MENU | OPEN_IN_BROWSER_MENU | COPY_LINK_MENU => navigate_menu(app, id),
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
/// app menu, after About, as Mac apps do. Windows: a menu bar in the window instead (`window_menu`).
#[cfg(target_os = "macos")]
pub(crate) fn setup_app_menu(app: &AppHandle) -> tauri::Result<()> {
  let menu = Menu::default(app)?;
  if let Some(app_menu) = menu.items()?.first().and_then(|item| item.as_submenu().cloned()) {
    let settings = MenuItem::with_id(app, SETTINGS_MENU, "Cài đặt…", true, Some("CmdOrCtrl+,"))?;
    let download_dir = MenuItem::with_id(app, DOWNLOAD_DIR_MENU, "Thư mục lưu file tải về…", true, None::<&str>)?;
    let open_downloads = MenuItem::with_id(app, OPEN_DOWNLOADS_MENU, "Mở thư mục tải về", true, None::<&str>)?;
    app_menu.insert_items(&[&settings, &download_dir, &open_downloads, &PredefinedMenuItem::separator(app)?], 2)?;
  }
  // View (only "Enter Full Screen" by default): the navigation items first.
  if let Some(view) = menu.items()?.iter().filter_map(|item| item.as_submenu().cloned()).find(|m| m.text().is_ok_and(|t| t == "View")) {
    let items = view_items(app)?;
    view.insert_items(
      &[
        &items[0],
        &items[1],
        &items[2],
        &PredefinedMenuItem::separator(app)?,
        &items[3],
        &items[4],
        &PredefinedMenuItem::separator(app)?,
      ],
      0,
    )?;
  }
  // Help (empty in the default menu): the version at a glance, who to contact, the standard About panel.
  if let Some(help) = menu.get(HELP_SUBMENU_ID).and_then(|item| item.as_submenu().cloned()) {
    let info = app.package_info();
    let version = info.version.to_string();
    let about = AboutMetadata { name: Some(info.name.clone()), version: Some(version.clone()), ..Default::default() };
    help.append_items(&[
      &MenuItem::with_id(app, GUIDE_MENU, "Hướng dẫn sử dụng", true, None::<&str>)?,
      &PredefinedMenuItem::separator(app)?,
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
