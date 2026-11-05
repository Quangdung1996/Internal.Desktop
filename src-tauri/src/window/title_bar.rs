//! Every window's title bar in the AMS brand teal (#067179), registered as an inline plugin's
//! `on_window_ready` in lib.rs so main, chat, popups and Settings all get it.
//! - macOS: a transparent title bar shows the window background (teal); the title is hidden since
//!   AppKit draws it dark, unreadable on teal. The webview starts below the bar as before.
//! - Windows 11: DWM caption + white caption text. Windows 10 has no caption color: the call fails
//!   and the default bar stays, which is fine.

use tauri::Window;

#[cfg(target_os = "macos")]
pub fn paint(window: Window) {
  use objc2::msg_send;
  use objc2::runtime::AnyObject;
  use tauri::window::Color;

  let _ = window.set_title_bar_style(tauri::TitleBarStyle::Transparent);
  // macOS: applies to the window only (not implemented for the webview), i.e. just the bar.
  let _ = window.set_background_color(Some(Color(0x06, 0x71, 0x79, 0xff)));
  // No Window setter for hidden title; AppKit calls must run on the main thread.
  let target = window.clone();
  let _ = window.run_on_main_thread(move || {
    if let Ok(ns_window) = target.ns_window() {
      // SAFETY: tauri hands out the live NSWindow; NSWindowTitleHidden = 1.
      unsafe {
        let ns_window = &*ns_window.cast::<AnyObject>();
        let _: () = msg_send![ns_window, setTitleVisibility: 1isize];
      }
    }
  });
}

#[cfg(windows)]
pub fn paint(window: Window) {
  use std::ffi::c_void;

  #[link(name = "dwmapi")]
  unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: *mut c_void, attribute: u32, value: *const c_void, size: u32) -> i32;
  }
  const DWMWA_CAPTION_COLOR: u32 = 35;
  const DWMWA_TEXT_COLOR: u32 = 36;
  // COLORREF is 0x00BBGGRR.
  const TEAL: u32 = 0x0079_7106;
  const WHITE: u32 = 0x00FF_FFFF;

  let Ok(hwnd) = window.hwnd() else { return };
  for (attribute, color) in [(DWMWA_CAPTION_COLOR, TEAL), (DWMWA_TEXT_COLOR, WHITE)] {
    // SAFETY: valid HWND from tauri; value points to a u32 COLORREF of the size passed.
    // The HRESULT is ignored on purpose: it fails only on Windows 10, which keeps the default bar.
    unsafe {
      DwmSetWindowAttribute(hwnd.0, attribute, (&raw const color).cast(), size_of::<u32>() as u32);
    }
  }
}

/// Linux is not shipped; keeps the crate building there.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn paint(_window: Window) {}
