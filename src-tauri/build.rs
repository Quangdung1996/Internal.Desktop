fn main() {
  // AMS_FILE_HOST: the S3 endpoint host whose links download inside the app (lib.rs LINK_SCRIPT).
  // Kept out of the repo: from the environment (CI: secret AMS_FILE_HOST) or the repo-root .env
  // (local, gitignored). Unset: those links open in the browser, as for any other site.
  println!("cargo:rerun-if-env-changed=AMS_FILE_HOST");
  println!("cargo:rerun-if-changed=../.env");
  let host = std::env::var("AMS_FILE_HOST").ok().filter(|h| !h.trim().is_empty()).or_else(|| {
    std::fs::read_to_string("../.env").ok()?.lines().find_map(|line| {
      let value = line.trim().strip_prefix("AMS_FILE_HOST=")?.trim().trim_matches(['"', '\'']);
      (!value.is_empty()).then(|| value.to_string())
    })
  });
  match host {
    Some(host) => println!("cargo:rustc-env=AMS_FILE_HOST={}", host.trim()),
    None => println!("cargo:warning=AMS_FILE_HOST is not set (.env or environment): file links will open in the browser"),
  }
  tauri_build::build()
}
