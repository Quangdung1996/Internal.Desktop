//! Auto update from the public releases repo: checks at startup and every 6 h, asks first.

use std::time::{Duration};
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};
use crate::settings;

/// The app lives in the tray for weeks, so checking only at startup is not enough.
pub(crate) const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Settings ▸ Cập nhật ▸ "Tự động kiểm tra cập nhật" is read on every round, so turning it off or on
/// takes effect at the next check without a restart.
pub(crate) fn spawn_updater(app: AppHandle) {
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
