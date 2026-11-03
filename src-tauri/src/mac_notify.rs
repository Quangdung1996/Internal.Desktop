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

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::sel;

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
