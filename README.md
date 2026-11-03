# Internal.Desktop

Tauri v2 desktop shell (Windows / macOS) for AMS (`Internal.Frontend`, chat included).

The window loads the deployed site (`build.frontendDist` = `https://approve.agari.com.vn`).
No frontend is bundled: the FE is SSR (next-auth, route handlers, `proxy.ts`) and cannot
`output: 'export'`, which is the only Next.js mode Tauri bundles. Same origin as the web
app, so auth cookies, SignalR chat and CORS work unchanged.

**Two update paths:** UI changes ship with every FE deploy (the app loads the live site, nothing
to install). Only shell changes (this repo) need a new desktop release, delivered by the updater.

## What the shell adds (`src-tauri/src/lib.rs`)

- Chat notifications: `tauri-plugin-notification` replaces `window.Notification` in the page (no FE change). Clicking a notification runs the page's own `onclick` (opens the room in the chat window); on Windows only while the toast is on screen, not from the Action Center.
- Chat gets one window of its own (like a messenger): the sidebar Chat link, "open in Chat" and the tray all land in it; a room link navigates it, size/position remembered.
- Other `window.open` / `target="_blank"`: `*.agari.com.vn` and `blob:` open in an app window (session kept); other sites open in the default browser. Without this both engines drop the request.
- Close = hide to tray (chat keeps running). Tray: Mở AMS / Mở Chat / Cài đặt… / Thoát. macOS: Dock click reopens.
- Deep links: `ams://<path>?<query>` opens that page of the site (e.g. `ams://chat/<roomId>` in the chat window, others in the main window); only path/query are used, the origin stays the site's. Registered by the installer; nothing generates these links yet (emails/FE must).
- Single instance, remembered window size/position, Ctrl +/- zoom.
- Global shortcut ⌥⌘A (macOS) / Ctrl+Alt+A (Windows): AMS in front goes to the tray, otherwise it comes forward. Skipped (logged) if another app holds it.
- Start: the main window opens on a bundled page (`src-tauri/offline.html`: logo + loading bar) that goes on to the site once it answers. Offline at launch it stays there with a "Mất kết nối internet" warning and retries every 5 s; a main/chat page that never loads within 10 s also lands there.
- Offline while in use: the page is kept, dimmed and blurred with a "Mất kết nối internet" warning (clicks held) until the connection is back; nothing navigates.
- macOS: downloads saved to ~/Downloads and revealed in Finder; `window.print` works (Tauri print API); camera, mic, location declared in `Info.plist` + `Entitlements.plist`.
- Auto update: checks `releases/latest` of `ams-desktop-releases` (then of this repo, for the move) at startup and every 6 h, asks before installing.

## Release (no local toolchain needed)

1. Bump `version` in `src-tauri/tauri.conf.json` (the updater only offers a higher version).
2. GitHub → Actions → **build** → Run workflow. Windows (.exe NSIS per-user + .msi per-machine) + macOS (universal) land in a draft release `app-v<version>` of **https://github.com/Quangdung1996/ams-desktop-releases** (public: README + releases only, no code).
3. Test the installers from the draft, then **Publish** it. Installed apps pick it up only after publishing.

One-time setup:
- Public repo `Quangdung1996/ams-desktop-releases`: only a README (staff install steps); Issues, Wiki, Projects off. The updater downloads `latest.json` and the installers from its releases anonymously, so this repo (the code) can be private.
- This repo → Settings → Secrets and variables → Actions → `RELEASES_TOKEN` = fine-grained token (github.com/settings/personal-access-tokens): repository access **only** `ams-desktop-releases`, permission **Contents: Read and write**. It expires: renew it and replace the secret before then, or builds fail at upload.
- Settings → Secrets and variables → Actions → `TAURI_SIGNING_PRIVATE_KEY` = content of the updater private key (generated with `npx tauri signer generate`, no password; public half is `plugins.updater.pubkey`). Losing that key means installed apps can never update again: keep a backup outside this machine.
- macOS signing + notarization secrets: `APPLE_CERTIFICATE` (base64 of the Developer ID Application .p12),
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` (`Developer ID Application: AGARI COMPANY LIMITED (ZGS2BK2B25)`),
  `APPLE_ID`, `APPLE_PASSWORD` (app-specific password, account.apple.com), `APPLE_TEAM_ID` (`ZGS2BK2B25`).
  The cert expires (check `certutil -dump` → NotAfter): renew it and replace the first two secrets before then.

macOS builds are Developer ID signed and notarized (open like any app). Windows builds are not code-signed:
SmartScreen → "More info" → "Run anyway".

## Install on macOS (staff)

1. Download `AMS_<version>_universal.dmg` from https://github.com/Quangdung1996/ams-desktop-releases/releases/latest (one file for Apple Silicon and Intel).
2. Open the .dmg, drag **AMS** into **Applications**, eject the disk. Do not run it from the .dmg: auto update cannot replace a read-only app.
3. Open AMS from Applications, allow **Notifications** when asked (chat alerts). If missed: System Settings → Notifications → AMS → Allow.
   Camera / microphone / location are asked the first time a page uses them; change later in Privacy & Security.
4. Optional: menu bar icon → **Cài đặt…** → **Khởi động cùng máy** to start with macOS.

Closing the window keeps AMS running in the menu bar; quit from the tray → **Thoát** (or ⌘Q).
Updates: the app asks when a new version is published; accept and it restarts itself.

## Local development (https://v2.tauri.app/start/prerequisites/)

- Windows: VS Build Tools "Desktop development with C++", Rust (`winget install Rustlang.Rustup`, then `rustup default stable-msvc`).
- macOS: `xcode-select --install`, Rust via rustup.
- Node.js LTS.

```sh
npm install
npm run dev          # the app against the live site (https://approve.agari.com.vn)
```

Under `dev` macOS attributes notifications to Terminal and shows no location prompt (no .app
bundle): test notifications, GPS and Settings with `npm run build:mac:debug` instead.

### Building installers (output in `dist/`)

| Command | Result |
|---|---|
| `npm run build:mac` | `AMS_<v>_universal.dmg` (Apple silicon + Intel). Add `-- --native` for this Mac's chip only |
| `npm run build:mac:debug` | `AMS-debug.app`, quick, for testing on this Mac |
| `npm run build:win` | From a Mac: `AMS_<v>_x64-setup.exe` if `makensis` is installed, else `AMS_<v>_x64-portable.exe` |

Hand out the `.dmg` and the `-setup.exe`; the `.sig` / `.app.tar.gz` files are for the updater.
`.msi` only builds on Windows (GitHub Actions). Run a script with `--help` for details.

Signing and notarizing (`build:mac`) and updater files (both) come from the environment, never from
the repo (it is public):

| Variable | Used for |
|---|---|
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: AGARI COMPANY LIMITED (ZGS2BK2B25)`; found in the keychain if unset |
| `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID` | notarization; or a `xcrun notarytool store-credentials AMS_NOTARY` keychain profile |
| `TAURI_SIGNING_PRIVATE_KEY` | updater `.sig` files; without it a build skips them (fine for testing, not for a release) |

Prerequisites for `build:win` on a Mac: `brew install llvm`, and `brew install makensis` for the
installer (needs current Xcode Command Line Tools). cargo-xwin and the Windows Rust target install
themselves on first run.

Icons: the "AMS" wordmark cropped from `Internal.Frontend/public/logo AMS.svg` on a white rounded
square, rendered with `npx tauri icon <square svg>`.
