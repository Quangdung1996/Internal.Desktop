//! Downloads: every download first goes to a temporary folder; when it is done a "Lưu" dialog asks
//! where to keep it (starting at the last folder saved to, settings::download_dir), the file is
//! moved there and the page shows "Đã lưu". Cancel: the file is deleted.
//! macOS needs a handler (WKWebView cancels downloads without one) and reports no path when done,
//! hence DOWNLOADS. On Windows the handler replaces WebView2's download flyout; file links have the
//! page's own progress notice (links/link.js).
//! Saved files are remembered by their S3 object (SAVED_FILES), so the FE file card can show "Đã có
//! trên máy" and a button that opens the folder (desktop.js: desktopApp.fileSaved / showFile).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Manager, Url, Webview};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::settings;

/// Downloads in progress: (url, where it is saved), for macOS's pathless Finished event.
static DOWNLOADS: Mutex<Vec<(String, PathBuf)>> = Mutex::new(Vec::new());

/// link.js downloads a file link through a blob: (blob url, the file link's key), told before the
/// download starts (`download_source`), so the save can be remembered under the file, not the blob.
static SOURCES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Remembered saves, newest last: (file key, where it was saved). In the app data folder.
static SAVED_FILES: Mutex<Option<Vec<(String, PathBuf)>>> = Mutex::new(None);
const SAVED_FILES_MAX: usize = 1000;

/// A file link's identity: host and path of its S3 object, without the presigned query (re-signed
/// on every load).
fn file_key(url: &str) -> Option<String> {
  let url = Url::parse(url).ok()?;
  Some(format!("{}{}", url.host_str()?, url.path()))
}

/// Called by link.js before it saves a file link's blob.
#[tauri::command]
pub(crate) fn download_source(blob: String, url: String) {
  let Some(key) = file_key(&url) else {
    return;
  };
  let mut sources = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
  sources.push((blob, key));
  if sources.len() > 50 {
    sources.remove(0);
  }
}

/// desktopApp.fileSaved(url): this file link was saved here and the file is still there.
#[tauri::command]
pub(crate) fn file_saved(app: AppHandle, url: String) -> bool {
  saved_path(&app, &url).is_some()
}

/// desktopApp.showFile(url): its folder, with the file selected (false: not saved / moved away).
#[tauri::command]
pub(crate) fn show_saved_file(app: AppHandle, url: String) -> bool {
  let Some(path) = saved_path(&app, &url) else {
    return false;
  };
  match app.opener().reveal_item_in_dir(&path) {
    Ok(()) => true,
    Err(e) => {
      log::error!("revealing {} failed: {e}", path.display());
      false
    }
  }
}

fn saved_path(app: &AppHandle, url: &str) -> Option<PathBuf> {
  let key = file_key(url)?;
  with_saved_files(app, |files| files.iter().rev().find(|(k, _)| *k == key).map(|(_, p)| p.clone()))
    .filter(|p| p.is_file())
}

fn with_saved_files<T>(app: &AppHandle, f: impl FnOnce(&mut Vec<(String, PathBuf)>) -> T) -> T {
  let mut saved = SAVED_FILES.lock().unwrap_or_else(|e| e.into_inner());
  let files = saved.get_or_insert_with(|| {
    saved_files_path(app)
      .and_then(|p| std::fs::read_to_string(p).ok())
      .and_then(|text| serde_json::from_str(&text).ok())
      .unwrap_or_default()
  });
  f(files)
}

fn saved_files_path(app: &AppHandle) -> Option<PathBuf> {
  Some(app.path().app_data_dir().ok()?.join("saved-files.json"))
}

fn remember_saved(app: &AppHandle, key: String, path: PathBuf) {
  let text = with_saved_files(app, |files| {
    files.retain(|(k, _)| *k != key);
    files.push((key, path));
    if files.len() > SAVED_FILES_MAX {
      files.remove(0);
    }
    serde_json::to_string(files).ok()
  });
  let (Some(text), Some(file)) = (text, saved_files_path(app)) else {
    return;
  };
  if let Some(dir) = file.parent() {
    let _ = std::fs::create_dir_all(dir);
  }
  if let Err(e) = std::fs::write(&file, text) {
    log::error!("writing {} failed: {e}", file.display());
  }
  // Every app window: the same file card can be on screen in the main and the chat window.
  for window in app.webview_windows().values() {
    let _ = window.eval("window.dispatchEvent(new Event('ams:file-saved'))");
  }
}

/// The `on_download` hook of every window (`window::with_handlers`).
pub(crate) fn on_download(webview: Webview, event: DownloadEvent) -> bool {
  match event {
    DownloadEvent::Requested { url, destination } => {
      let default_dir = webview.app_handle().path().download_dir().unwrap_or_default();
      let name = original_name(destination, &default_dir);
      let dir = std::env::temp_dir().join("ams-downloads");
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
      match path.or(started) {
        Some(temp) if success => {
          let mut sources = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
          let key = sources.iter().position(|(blob, _)| *blob == url.as_str()).map(|at| sources.remove(at).1);
          drop(sources);
          ask_where_to_save(webview, temp, key);
        }
        Some(temp) => {
          let _ = std::fs::remove_file(&temp);
          notice(&webview, &format!("⚠ Tải không thành công {}", file_name(&temp)), "error");
        }
        None => notice(&webview, if success { "✓ Đã tải xong" } else { "⚠ Tải không thành công" }, "done"),
      }
    }
    _ => {}
  }
  true
}

/// The "Lưu" dialog for a finished download (non-blocking: Finished arrives on the main thread),
/// then the file moves from the temporary folder to the chosen path.
/// ponytail: two downloads finishing together open two dialogs; a queue if that turns out confusing.
fn ask_where_to_save(webview: Webview, temp: PathBuf, key: Option<String>) {
  let app = webview.app_handle().clone();
  let name = file_name(&temp);
  let mut dialog = app
    .dialog()
    .file()
    .set_title("Lưu file tải về")
    .set_directory(settings::download_dir(&app))
    .set_file_name(&name)
    .set_parent(&webview.window()); // a sheet on its window (macOS), modal on Windows
  if let Some(ext) = temp.extension().map(|e| e.to_string_lossy().into_owned()) {
    dialog = dialog.add_filter(ext.to_uppercase(), &[&ext]);
  }
  dialog.save_file(move |target| {
    let Some(target) = target.and_then(|p| p.into_path().ok()) else {
      let _ = std::fs::remove_file(&temp);
      notice(&webview, &format!("Đã huỷ lưu {name}"), "done");
      return;
    };
    // The dialog already asked before replacing an existing file. rename fails across disks
    // (temporary folder on C:, chosen folder on D:): copy then.
    let moved = std::fs::rename(&temp, &target).or_else(|_| {
      std::fs::copy(&temp, &target)?;
      std::fs::remove_file(&temp)
    });
    match moved {
      Ok(()) => {
        if let Some(dir) = target.parent() {
          let _ = settings::set_download_dir(&app, dir.to_path_buf());
        }
        if let Some(key) = key {
          remember_saved(&app, key, target.clone());
        }
        notice(&webview, &format!("✓ Đã lưu {}", file_name(&target)), "done");
      }
      Err(e) => {
        log::error!("saving the download to {}: {e}", target.display());
        notice(&webview, &format!("⚠ Không lưu được {}: {e}", file_name(&target)), "error");
      }
    }
  });
}

fn file_name(path: &Path) -> String {
  path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A line in the page's download notice (links/link.js defines __amsNotice in every app window).
fn notice(webview: &Webview, text: &str, end: &str) {
  let _ = webview.eval(format!("window.__amsNotice?.({text:?}, {end:?})"));
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
