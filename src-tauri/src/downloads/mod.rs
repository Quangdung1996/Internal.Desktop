//! Downloads: every download goes to the download folder (menu ▸ "Thư mục lưu file tải về…",
//! default the OS Downloads folder) under a free name; when done the page shows "Đã tải xong" (the
//! folder is not opened: menu ▸ "Mở thư mục tải về" does that).
//! macOS needs a handler (WKWebView cancels downloads without one) and reports no path when done,
//! hence DOWNLOADS. On Windows the handler replaces WebView2's download flyout; file links have the
//! page's own notice (links/link.js).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::webview::DownloadEvent;
use tauri::{Manager, Webview};

use crate::settings;

/// Downloads in progress: (url, where it is saved), for macOS's pathless Finished event.
static DOWNLOADS: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// The `on_download` hook of every window (`window::with_handlers`).
pub(crate) fn on_download(webview: Webview, event: DownloadEvent) -> bool {
  match event {
    DownloadEvent::Requested { url, destination } => {
      let name = destination.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "download".into());
      let dir = settings::download_dir(webview.app_handle());
      let _ = std::fs::create_dir_all(&dir);
      let path = free_path(&dir, Path::new(&name));
      let mut downloads = DOWNLOADS.lock().unwrap_or_else(|e| e.into_inner());
      downloads.push((url.to_string(), path.clone()));
      if downloads.len() > 50 {
        downloads.remove(0);
      }
      *destination = path;
    }
    DownloadEvent::Finished { url, path, success } => {
      let mut downloads = DOWNLOADS.lock().unwrap_or_else(|e| e.into_inner());
      let started = downloads.iter().position(|(u, _)| *u == url.as_str()).map(|at| downloads.remove(at).1);
      drop(downloads);
      let name = path.or(started).and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
      let text = match (success, name) {
        (true, Some(name)) => format!("✓ Đã tải xong {name}"),
        (true, None) => "✓ Đã tải xong".to_string(),
        (false, Some(name)) => format!("⚠ Tải không thành công {name}"),
        (false, None) => "⚠ Tải không thành công".to_string(),
      };
      let end = if success { "done" } else { "error" };
      // links/link.js defines __amsNotice in every app window.
      let _ = webview.eval(format!("window.__amsNotice?.({text:?}, {end:?})"));
    }
    _ => {}
  }
  true
}

/// `dir/name`, or `dir/name (1).ext`, `(2)`… if taken: a download never overwrites a file.
fn free_path(dir: &Path, name: &Path) -> PathBuf {
  let path = dir.join(name);
  if !path.exists() {
    return path;
  }
  let stem = name.file_stem().unwrap_or_default().to_string_lossy();
  let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
  (1..)
    .map(|n| dir.join(format!("{stem} ({n}){ext}")))
    .find(|p| !p.exists())
    .expect("an unused name exists")
}

#[cfg(test)]
mod tests {
  use super::free_path;
  use std::path::Path;

  #[test]
  fn free_path_never_overwrites() {
    let dir = std::env::temp_dir().join(format!("ams-free-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let name = Path::new("Báo cáo tháng.xlsx");
    assert_eq!(free_path(&dir, name), dir.join("Báo cáo tháng.xlsx"));
    std::fs::write(dir.join("Báo cáo tháng.xlsx"), "").unwrap();
    assert_eq!(free_path(&dir, name), dir.join("Báo cáo tháng (1).xlsx"));
    std::fs::write(dir.join("Báo cáo tháng (1).xlsx"), "").unwrap();
    assert_eq!(free_path(&dir, name), dir.join("Báo cáo tháng (2).xlsx"));
    assert_eq!(free_path(&dir, Path::new("README")), dir.join("README"));
    std::fs::remove_dir_all(&dir).unwrap();
  }
}
