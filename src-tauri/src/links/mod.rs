//! Where links open: AMS pages in an app window, other sites in the default browser, S3 file
//! links downloaded in place (link.js); `ams://` deep links.

use std::sync::atomic::{Ordering};
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_opener::OpenerExt;
use crate::window::{MAIN, POPUP_PREFIX, POPUP_SEQ, bring_forward, is_chat_url, main_url, navigate_main, open_chat, with_handlers};

/// `ams://<path>?<query>` (e.g. ams://chat/<roomId>) opens that page of the site: chat paths in the
/// chat window, the rest in the main window. Mirrors plugins.deep-link in tauri.conf.json. Nothing
/// emits these links yet: emails and the FE must generate them (plain https links open the browser).
pub(crate) const DEEP_LINK_SCHEME_PREFIX: &str = "ams:";

/// Only the path, query and fragment are taken: the origin is always the site's, so a crafted link
/// (ams:///evil.com/x) cannot load another site into an app window holding the session.
pub(crate) fn open_deep_link(app: &AppHandle, link: &Url) {
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
    let _ = navigate_main(&main, target);
    bring_forward(&main);
  }
}

/// WKWebView never reports `<a target="_blank" rel="noopener">` (the FE sidebar Chat link) to
/// `on_new_window`: the click is dropped silently. This script catches such clicks, and chat
/// `window.open` calls, and hands them to `open_link`. Without IPC the page navigates in place.
/// Files: the FE opens S3 presigned links (attachments, chat files, certificates) in a new tab,
/// which here meant the default browser (another host than AMS). They are downloaded in place
/// instead: fetch (the bucket's CORS allows the site) -> blob -> a[download], which the app saves
/// (macOS: ~/Downloads, revealed in Finder; Windows: WebView2's download flyout).
pub(crate) const LINK_SCRIPT: &str = include_str!("link.js");

/// LINK_SCRIPT with the file host from build.rs (AMS_FILE_HOST, never in the repo).
pub(crate) fn link_script() -> &'static str {
  static SCRIPT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| LINK_SCRIPT.replace("__AMS_FILE_HOST__", option_env!("AMS_FILE_HOST").unwrap_or("")));
  &SCRIPT
}

/// Called by LINK_SCRIPT: the same routing as `open_new_window`, minus window.opener (the FE links
/// it covers are noopener). async: building a window inside a sync command deadlocks on Windows.
#[tauri::command]
pub(crate) async fn open_link(app: AppHandle, url: String) -> Result<(), String> {
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

/// window.open / target="_blank". Without a handler both engines silently drop the request.
/// AMS pages (chat tab, payment print, attachments) open in an app window that keeps the session
/// and window.opener; meeting links and other sites go to the default browser.
pub(crate) fn open_new_window(app: &AppHandle, url: Url, features: NewWindowFeatures) -> NewWindowResponse<Wry> {
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
pub(crate) fn is_app_url(url: &Url) -> bool {
  url.scheme() == "blob"
    || matches!(url.host_str(), Some(h) if h == "localhost" || h == "agari.com.vn" || h.ends_with(".agari.com.vn"))
}
