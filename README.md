# Internal.Desktop

Tauri v2 desktop shell (Windows / macOS) for AMS (`Internal.Frontend`, chat included).

The window loads the deployed site (`build.frontendDist` = `https://approve.agari.com.vn`).
No frontend is bundled: the FE is SSR (next-auth, route handlers, `proxy.ts`) and cannot
`output: 'export'`, which is the only Next.js mode Tauri bundles. Same origin as the web
app, so auth cookies, SignalR chat and CORS work unchanged.

**Two update paths:** UI changes ship with every FE deploy (the app loads the live site, nothing
to install). Only shell changes (this repo) need a new desktop release, delivered by the updater.

## What the shell adds (`src-tauri/src/lib.rs`)

- Chat notifications: `tauri-plugin-notification` replaces `window.Notification` in the page (no FE change). Clicking a notification does not open the room.
- Chat gets one window of its own (like a messenger): the sidebar Chat link, "open in Chat" and the tray all land in it; a room link navigates it, size/position remembered.
- Other `window.open` / `target="_blank"`: `*.agari.com.vn` and `blob:` open in an app window (session kept); other sites open in the default browser. Without this both engines drop the request.
- Close = hide to tray (chat keeps running). Tray: Mở AMS / Mở Chat / Khởi động cùng máy / Thoát. macOS: Dock click reopens.
- Single instance, remembered window size/position, Ctrl +/- zoom.
- macOS: downloads saved to ~/Downloads and revealed in Finder; `window.print` works (Tauri print API); camera, mic, location declared in `Info.plist` + `Entitlements.plist`.
- Auto update: checks GitHub `releases/latest` at startup and every 6 h, asks before installing.

## Release (no local toolchain needed)

1. Bump `version` in `src-tauri/tauri.conf.json` (the updater only offers a higher version).
2. GitHub → Actions → **build** → Run workflow. Windows (NSIS) + macOS (universal) land in a draft release `app-v<version>`.
3. Test the installers from the draft, then **Publish** it. Installed apps pick it up only after publishing.

One-time repo setup:
- Settings → General → visibility **Public** (updater downloads `latest.json` anonymously; Actions minutes are free).
- Settings → Actions → General → Workflow permissions → **Read and write**.
- Settings → Secrets and variables → Actions → `TAURI_SIGNING_PRIVATE_KEY` = content of the updater private key (generated with `npx tauri signer generate`, no password; public half is `plugins.updater.pubkey`). Losing that key means installed apps can never update again: keep a backup outside this machine.

Builds are not code-signed: Windows SmartScreen → "More info" → "Run anyway"; macOS (ad-hoc signed) →
System Settings → Privacy & Security → "Open Anyway".

## Local development (https://v2.tauri.app/start/prerequisites/)

- Windows: VS Build Tools "Desktop development with C++", Rust (`winget install Rustlang.Rustup`, then `rustup default stable-msvc`).
- macOS: `xcode-select --install`, Rust via rustup.
- Node.js LTS.

```sh
npm install
npm run tauri dev   # loads devUrl http://localhost:3000; start Internal.Frontend (`npm run dev`) first
```

A local `tauri build` needs `TAURI_SIGNING_PRIVATE_KEY` set (updater artifacts are signed).

Icons: the "AMS" wordmark cropped from `Internal.Frontend/public/logo AMS.svg` on a white rounded
square, rendered with `npx tauri icon <square svg>`.
