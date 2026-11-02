# Internal.Desktop

Tauri v2 desktop shell (Windows / macOS) for AMS (`Internal.Frontend`, chat included).

The window loads the deployed site (`build.frontendDist` = `https://approve.agari.com.vn`).
No frontend is bundled: the FE is SSR (next-auth, route handlers, `proxy.ts`) and cannot
`output: 'export'`, which is the only Next.js mode Tauri bundles. Same origin as the web
app, so auth cookies, SignalR chat and CORS work unchanged; deploying the FE updates the app.

## Build installers (no local toolchain needed)

GitHub → Actions → **build** → Run workflow. Windows + macOS (universal) installers land in a
draft release `app-v<version>`; bump `version` in `src-tauri/tauri.conf.json` for a new release.
Repo setting required: Settings → Actions → General → Workflow permissions → Read and write.

Builds are unsigned: Windows SmartScreen → "More info" → "Run anyway"; macOS (ad-hoc signed) →
System Settings → Privacy & Security → "Open Anyway".

## Local development (https://v2.tauri.app/start/prerequisites/)

- Windows: VS Build Tools "Desktop development with C++", Rust (`winget install Rustlang.Rustup`, then `rustup default stable-msvc`). WebView2 ships with Win10 1803+/Win11. The `.msi` needs the VBSCRIPT optional feature; `npm run tauri build -- --bundles nsis` skips it.
- macOS: `xcode-select --install`, Rust via rustup. A macOS bundle can only be built on a Mac (or a macOS CI runner).
- Node.js LTS.

## Commands

```sh
npm install
npm run tauri dev     # loads devUrl http://localhost:3000; start Internal.Frontend (`npm run dev`) first
npm run tauri build   # Windows: .msi + .exe (NSIS); macOS: .app + .dmg -> src-tauri/target/release/bundle/
```

Icons are generated from `Internal.Frontend/public/agari.svg`: `npx tauri icon <square png/svg>`.
