//! macOS: show notifications while AMS is the front app too.
//!
//! The notification plugin goes through notify-rust → mac-notification-sys, whose
//! NSUserNotificationCenter delegate (`NotificationCenterDelegate`) lacks
//! `userNotificationCenter:shouldPresentNotification:`. Without it macOS files a notification sent by
//! the frontmost app straight into Notification Center: no banner, no sound. That hid Settings ▸
//! "Gửi thông báo thử" and a chat message for another room while the chat window is in front.
//! Windows toasts show either way, so this is macOS only. Whether to notify at all stays the
//! page's call (the FE does not notify for the room being read).

use std::ffi::c_char;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{ffi, msg_send, sel};
use objc2_foundation::NSString;
use tauri::AppHandle;

/// A click on a notification (banner or Notification Center): AppKit calls the delegate's
/// `userNotificationCenter:didActivateNotification:`. mac-notification-sys reports it only to a
/// caller still blocked waiting on that one notification, which the notification plugin gives up on
/// (its click never reached `on_action`). So the click is taken here, matched to the AMS
/// notification by its shown title + text (`crate::on_notification_click_by_text`), and the
/// original method still runs (it clears the notification).
static APP: OnceLock<AppHandle> = OnceLock::new();
static ORIGINAL_DID_ACTIVATE: OnceLock<Imp> = OnceLock::new();

type DidActivate = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);

unsafe extern "C-unwind" fn did_activate(this: *mut AnyObject, cmd: Sel, center: *mut AnyObject, note: *mut AnyObject) {
  // SAFETY: AppKit passes a live NSUserNotification; title/informativeText are NSString properties.
  if let (Some(app), Some(note)) = (APP.get(), unsafe { note.as_ref() }) {
    let title: Option<Retained<NSString>> = unsafe { msg_send![note, title] };
    let body: Option<Retained<NSString>> = unsafe { msg_send![note, informativeText] };
    let text = |s: Option<Retained<NSString>>| s.map(|s| s.to_string()).unwrap_or_default();
    crate::on_notification_click_by_text(app, &text(title), &text(body));
  }
  if let Some(original) = ORIGINAL_DID_ACTIVATE.get() {
    // SAFETY: the implementation this method had before, with the same signature.
    let original: DidActivate = unsafe { std::mem::transmute::<Imp, DidActivate>(*original) };
    unsafe { original(this, cmd, center, note) };
  }
}

/// Swaps in `did_activate`, keeping the original. Same class-name caveat as `present_in_foreground`.
pub fn handle_clicks(app: AppHandle) {
  let _ = APP.set(app);
  let Some(class) = AnyClass::get(c"NotificationCenterDelegate") else {
    log::warn!("NotificationCenterDelegate not found: clicking a notification will not open its page");
    return;
  };
  let Some(method) = class.instance_method(sel!(userNotificationCenter:didActivateNotification:)) else {
    log::warn!("didActivateNotification not found: clicking a notification will not open its page");
    return;
  };
  // SAFETY: did_activate has the method's signature (self, _cmd, center, notification) -> void.
  unsafe {
    let imp = std::mem::transmute::<DidActivate, Imp>(did_activate);
    let original = method.set_implementation(imp);
    let _ = ORIGINAL_DID_ACTIVATE.set(original);
  }
}

/// `- (BOOL)userNotificationCenter:shouldPresentNotification:` → YES.
unsafe extern "C-unwind" fn should_present(_this: *mut AnyObject, _cmd: Sel, _center: *mut AnyObject, _note: *mut AnyObject) -> Bool {
  Bool::YES
}

/// Adds the method to mac-notification-sys's delegate class (registered when the binary loads).
/// Keyed on that crate's class name: if a future version renames it, banners stay hidden while
/// AMS is in front (the old behaviour) and the log says so.
pub fn present_in_foreground() {
  let Some(class) = AnyClass::get(c"NotificationCenterDelegate") else {
    log::warn!("NotificationCenterDelegate not found: notifications stay hidden while AMS is in front");
    return;
  };
  // BOOL is `bool` on Apple silicon, `signed char` on Intel.
  #[cfg(target_arch = "aarch64")]
  const TYPES: &std::ffi::CStr = c"B@:@@";
  #[cfg(not(target_arch = "aarch64"))]
  const TYPES: &std::ffi::CStr = c"c@:@@";
  // SAFETY: the function matches the selector's signature (self, _cmd, center, notification) -> BOOL;
  // class_addMethod leaves an existing implementation untouched (returns NO).
  unsafe {
    let imp: Imp = std::mem::transmute::<
      unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) -> Bool,
      Imp,
    >(should_present);
    ffi::class_addMethod(
      class as *const AnyClass as *mut AnyClass,
      sel!(userNotificationCenter:shouldPresentNotification:),
      imp,
      TYPES.as_ptr() as *const c_char,
    );
  }
}
