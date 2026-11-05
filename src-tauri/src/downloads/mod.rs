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
      let default_dir = webview.app_handle().path().download_dir().unwrap_or_default();
      let name = original_name(destination, &default_dir);
      let dir = settings::download_dir(webview.app_handle());
      let _ = std::fs::create_dir_all(&dir);
      let path = free_path(&dir, &name);
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

/// The name the page asked for. wry (macOS) and WebView2 (Windows) hand over a destination already
/// made unique in the OS Downloads folder ("x.pdf" taken there -> "x (1).pdf"), before the folder
/// chosen here is known. So "base (N).ext" whose "base.ext" exists in that default folder is taken
/// back to "base.ext"; `free_path` then numbers it only if the chosen folder has it. (wry splits the
/// extension at the first dot, WebView2 at the last: the lazy `base` matches both.)
/// ponytail: a file really named "x (1).pdf" while "x.pdf" sits in Downloads is saved as "x.pdf".
fn original_name(destination: &Path, default_dir: &Path) -> PathBuf {
  let Some(name) = destination.file_name().map(|n| n.to_string_lossy().into_owned()) else {
    return PathBuf::from("download");
  };
  let numbered = name.find(" (").and_then(|start| {
    let close = start + name[start..].find(')')?;
    let digits = &name[start + 2..close];
    let rest = &name[close + 1..];
    let is_number = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    (is_number && (rest.is_empty() || rest.starts_with('.'))).then(|| format!("{}{rest}", &name[..start]))
  });
  match numbered {
    Some(original) if default_dir.join(&original).exists() => PathBuf::from(original),
    _ => PathBuf::from(name),
  }
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
  use super::{free_path, original_name};
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

  #[test]
  fn original_name_undoes_the_engine_numbering() {
    let downloads = std::env::temp_dir().join(format!("ams-original-name-{}", std::process::id()));
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(downloads.join("Báo cáo.pdf"), "").unwrap();
    std::fs::write(downloads.join("bang.luong.xlsx"), "").unwrap();
    let name = |n: &str| original_name(&downloads.join(n), &downloads);
    // numbered because Downloads has the original -> the original
    assert_eq!(name("Báo cáo (1).pdf"), Path::new("Báo cáo.pdf"));
    assert_eq!(name("Báo cáo (12).pdf"), Path::new("Báo cáo.pdf"));
    assert_eq!(name("bang (1).luong.xlsx"), Path::new("bang.luong.xlsx")); // wry: first dot
    assert_eq!(name("bang.luong (1).xlsx"), Path::new("bang.luong.xlsx")); // WebView2: last dot
    // not the engine's doing -> unchanged
    assert_eq!(name("Hợp đồng (1).pdf"), Path::new("Hợp đồng (1).pdf")); // no "Hợp đồng.pdf" there
    assert_eq!(name("Đơn (bản nháp).pdf"), Path::new("Đơn (bản nháp).pdf"));
    assert_eq!(name("Báo cáo.pdf"), Path::new("Báo cáo.pdf"));
    std::fs::remove_dir_all(&downloads).unwrap();
  }
}
