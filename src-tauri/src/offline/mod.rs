//! The offline watch: a main or chat page that never loads is replaced by the bundled
//! start/offline page (`watch_load`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration};
use tauri::{AppHandle, Manager, Url, WebviewWindow};
use crate::window::{CHAT, MAIN, is_site_page};
use crate::settings;

/// A main or chat page that never loads (offline at launch; on Windows a reload while offline) would
/// leave a blank or browser error page. Every document pings `page_alive` with its address
/// (DESKTOP_SCRIPT); building the window, leaving a page (`page_leaving`) or, on Windows, a load
/// starting (`with_handlers`) starts a wait. Without a ping from an AMS page within LOAD_TIMEOUT the
/// window is asked once more (PROBE_GRACE: a ping that raced a late `page_leaving`), then shows the
/// bundled offline page, which returns once the site answers. Detection lives in the page because
/// macOS reports no failed load (wry's Started fires on commit).
/// ponytail: a site slower than LOAD_TIMEOUT to answer lands there too; the page brings it back.
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const PROBE_GRACE: Duration = Duration::from_secs(2);

pub(crate) const PROBE_SCRIPT: &str = r#"window.__TAURI_INTERNALS__?.invoke("page_alive", { url: location.href })"#;

pub(crate) static LOAD_WAITS: Mutex<Vec<(String, u64, Url)>> = Mutex::new(Vec::new());

pub(crate) static LOAD_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) fn watch_load(app: &AppHandle, label: &str, fallback: Url) {
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
pub(crate) fn page_alive(window: WebviewWindow, url: String) {
  let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
  if !loaded.iter().any(|l| l == window.label()) {
    loaded.push(window.label().to_string());
  }
  drop(loaded);
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
pub(crate) fn page_leaving(app: AppHandle, window: WebviewWindow, url: String) {
  let Ok(url) = url.parse::<Url>() else {
    return;
  };
  let back_to = if is_site_page(&url) { Some(url) } else { settings::offline_target(&url) };
  if let Some(url) = back_to.filter(is_site_page) {
    watch_load(&app, window.label(), url);
  }
}

/// Windows that loaded a document: `url()` is safe on them (macOS: wry panics on one that never did).
pub(crate) static LOADED: Mutex<Vec<String>> = Mutex::new(Vec::new());
