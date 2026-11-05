//! Chat notifications (the page's `new Notification` goes to `notify`), their clicks, and the
//! Dock / taskbar badge.

/// macOS: banners while AMS is in front, and clicks from the notification delegate.
#[cfg(target_os = "macos")]
pub(crate) mod mac;

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, UserAttentionType, WebviewWindow};
use tauri_plugin_notification::NotificationExt;
use crate::window::{CHAT, MAIN, bring_forward, open_chat, show_main};
use crate::settings;

/// One popup per message: the main window (RMS pages) and the chat window both receive it, both
/// call `notify` with the same title, body and tag within a moment.
pub(crate) static RECENT_NOTIFICATIONS: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

pub(crate) const DUPLICATE_WINDOW: Duration = Duration::from_secs(3);

/// The OS default sound: notify-rust plays none without a name, and Windows then marks the toast silent.
#[cfg(target_os = "macos")]
pub(crate) const NOTIFICATION_SOUND: Option<&str> = Some("NSUserNotificationDefaultSoundName");

#[cfg(windows)]
pub(crate) const NOTIFICATION_SOUND: Option<&str> = Some("Default");

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) const NOTIFICATION_SOUND: Option<&str> = None;

pub(crate) fn is_duplicate(key: String) -> bool {
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
pub(crate) async fn notify(
  app: AppHandle,
  window: WebviewWindow,
  title: String,
  body: String,
  tag: String,
  click_id: Option<u32>,
  kind: Option<String>,
) {
  let origin = click_id.map(|id| Origin {
    label: window.label().to_string(),
    click_id: id,
    call: kind.as_deref() == Some("call"),
  });
  deliver_notification(app, title, body, tag, origin).await;
}

/// Notification id → where it came from (window label, the page's clickId) and the text it showed
/// (`text_key`), so a click runs that page's onclick. Bounded: a click on an older one falls back
/// (`on_notification_click`).
pub(crate) static NOTIFICATION_ORIGINS: Mutex<Vec<(i32, Origin, String)>> = Mutex::new(Vec::new());

/// Which page made a notification: its window, its clickId (`__amsNotificationClick`), and whether it
/// is an incoming call (FE `data: { kind: "call" }`).
pub(crate) struct Origin {
  label: String,
  click_id: u32,
  call: bool,
}

pub(crate) static NOTIFICATION_SEQ: AtomicI32 = AtomicI32::new(1);

pub(crate) const NOTIFICATION_ORIGINS_KEPT: usize = 100;

pub(crate) fn text_key(title: &str, body: &str) -> String {
  format!("{title}\u{1f}{body}")
}

/// macOS (notifications::mac): the clicked notification is known only by what it showed; the newest AMS
/// notification with that title and text is the one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn on_notification_click_by_text(app: &AppHandle, title: &str, body: &str) {
  let key = text_key(title, body);
  let id = NOTIFICATION_ORIGINS
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .iter()
    .rev()
    .find(|(_, _, k)| *k == key)
    .map(|(id, _, _)| *id);
  on_notification_click(app, id);
}

/// A click on a notification: Windows through the plugin's `on_action` (only while the toast is on
/// screen, not from the Action Center), macOS through `on_notification_click_by_text`.
pub(crate) fn on_notification_click(app: &AppHandle, id: Option<i32>) {
  let origin = id.and_then(|id| {
    let mut origins = NOTIFICATION_ORIGINS.lock().unwrap_or_else(|e| e.into_inner());
    let at = origins.iter().position(|(n, _, _)| *n == id)?;
    Some(origins.remove(at).1)
  });
  // An incoming call rings in every open AMS window (the call banner with answer/decline). Bring up
  // the chat window if it is open (where the user chats), else the main one; never open a new chat
  // window for it: a page loaded after the ring started has no incoming banner (it is set by the
  // live event only).
  if origin.as_ref().is_some_and(|o| o.call) {
    if let Some(chat) = app.get_webview_window(CHAT) {
      log::info!("notification #{id:?} clicked: a call, showing the chat window");
      bring_forward(&chat);
    } else {
      log::info!("notification #{id:?} clicked: a call, showing the main window");
      show_main(app);
    }
    return;
  }
  // Settings ▸ "Gửi thông báo thử", or one too old to remember.
  let Some(Origin { label, click_id, .. }) = origin else {
    log::info!("notification #{id:?} clicked: no origin kept, showing the main window");
    show_main(app);
    return;
  };
  let Some(window) = app.get_webview_window(&label) else {
    log::info!("notification #{id:?} clicked: window {label} is gone, opening the chat");
    open_chat(app, None);
    return;
  };
  log::info!("notification #{id:?} clicked: running onclick {click_id} in {label}");
  // The chat page only switches rooms in place: bring its window up. The main window's onclick opens
  // the chat window itself (window.open → open_chat), so a hidden main window stays hidden.
  if label != MAIN {
    bring_forward(&window);
  }
  let _ = window.eval(format!("window.__amsNotificationClick?.({click_id})"));
}

/// `notify`'s work, shared with Settings ▸ "Gửi thông báo thử".
pub(crate) async fn deliver_notification(app: AppHandle, title: String, body: String, tag: String, origin: Option<Origin>) {
  if is_duplicate(format!("{tag}\u{1f}{title}\u{1f}{body}")) {
    return;
  }
  // Settings ▸ Thông báo. The title (who wrote, which room) stays; only the message text is hidden.
  let prefs = settings::prefs(&app);
  let body = if prefs.notification_preview || body.is_empty() { body } else { "Có tin nhắn mới".to_string() };
  let id = NOTIFICATION_SEQ.fetch_add(1, Ordering::Relaxed);
  if let Some(origin) = origin {
    let mut origins = NOTIFICATION_ORIGINS.lock().unwrap_or_else(|e| e.into_inner());
    if origins.len() >= NOTIFICATION_ORIGINS_KEPT {
      origins.remove(0);
    }
    origins.push((id, origin, text_key(&title, &body)));
  }
  let mut builder = app.notification().builder().id(id).title(&title).body(&body);
  // No sound name = silent on both OSes (Windows marks the toast silent).
  if prefs.notification_sound
    && let Some(sound) = NOTIFICATION_SOUND
  {
    builder = builder.sound(sound);
  }
  // Ids only, never the text (the log goes to IT).
  log::info!("notification #{id} shown");
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
pub(crate) fn set_badge(app: AppHandle, count: u32) {
  BADGE.store(count, Ordering::Relaxed);
  apply_badge(&app);
  let count = (count > 0).then_some(i64::from(count));
  if let Some(tray) = app.tray_by_id(MAIN) {
    let tooltip = count.map_or_else(|| "AMS".to_string(), |n| format!("AMS ({n})"));
    let _ = tray.set_tooltip(Some(tooltip));
  }
}

/// The last count from the page: the chat window, opened later, gets it too (Windows overlay).
pub(crate) static BADGE: AtomicU32 = AtomicU32::new(0);

/// macOS: the Dock badge (app-wide). Windows: no badge API; each taskbar button (main, chat) gets the
/// count drawn as its overlay icon.
pub(crate) fn apply_badge(app: &AppHandle) {
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
pub(crate) fn badge_icon(count: u32) -> tauri::image::Image<'static> {
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
