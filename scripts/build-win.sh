#!/usr/bin/env bash
# Builds AMS for Windows (x64) from macOS (or Linux) with cargo-xwin and copies the result to dist/.
#
#   npm run build:win          release: NSIS installer if makensis exists, else a portable .exe
#   npm run build:win -- --portable   portable .exe only (no installer)
#   bash scripts/build-win.sh --check   compile check of the Windows code paths only (no build), fast
#
# Installer  AMS_<v>_x64-setup.exe needs `makensis` (brew install makensis; Homebrew needs current
#            Xcode Command Line Tools to build it). Without it: AMS_<v>_x64-portable.exe, which runs
#            as is but shows no Windows notifications (toasts need the installed app's identity) and
#            cannot self-update. Test notifications and updates with the installer.
# .msi       only builds on Windows (WiX): GitHub Actions "build", or on a Windows PC
#            `npm run tauri build -- --bundles nsis,msi`.
# Updater    .exe.sig only with TAURI_SIGNING_PRIVATE_KEY; skipped otherwise.
# Unsigned   no Windows code-signing certificate: SmartScreen asks "More info → Run anyway" once.
# First run downloads the MSVC runtime/SDK libraries (~several hundred MB) into the cargo-xwin cache.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

die() { echo "✗ $*" >&2; exit 1; }
note() { echo "• $*"; }
warn() { echo "⚠ $*"; }

TARGET=x86_64-pc-windows-msvc
mode=auto
for arg in "$@"; do
  case "$arg" in
    --portable) mode=portable ;;
    --check) mode=check ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) die "unknown option: $arg (see --help)" ;;
  esac
done

command -v cargo >/dev/null || die "Rust is missing: https://rustup.rs"

# --- Toolchain: Windows target, cargo-xwin, LLVM (clang-cl + lld-link) ------------------------
rustup target add "$TARGET" >/dev/null
if ! command -v cargo-xwin >/dev/null; then
  note "Installing cargo-xwin (one time)…"
  cargo install --locked cargo-xwin
fi
if ! command -v lld-link >/dev/null; then
  llvm_bin=""
  if command -v brew >/dev/null && [ -d "$(brew --prefix llvm 2>/dev/null)/bin" ]; then
    llvm_bin="$(brew --prefix llvm)/bin"
  fi
  [ -n "$llvm_bin" ] || die "LLVM is missing (needed to link for Windows): brew install llvm"
  export PATH="$llvm_bin:$PATH"
fi

if [ "$mode" = check ]; then
  note "Checking the Windows build (cargo xwin check)…"
  (cd src-tauri && cargo xwin check --target "$TARGET")
  note "Windows code compiles."
  exit 0
fi

[ -d node_modules ] || npm install
version=$(node -p "require('./src-tauri/tauri.conf.json').version")

if [ "$mode" = auto ]; then
  if command -v makensis >/dev/null; then
    mode=installer
  else
    warn "makensis not found: building a portable .exe instead of the installer (brew install makensis)."
    mode=portable
  fi
fi

args=(--runner cargo-xwin --target "$TARGET")
if [ "$mode" = installer ]; then
  args+=(--bundles nsis)
else
  args+=(--no-bundle)
fi
if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  [ "$mode" = installer ] && warn "No TAURI_SIGNING_PRIVATE_KEY: no updater .sig (fine for testing, not for a release)."
  args+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi

note "Building AMS $version for Windows x64 ($mode)…"
npm run tauri build -- "${args[@]}"

release="src-tauri/target/$TARGET/release"
mkdir -p dist
if [ "$mode" = installer ]; then
  setup=$(ls "$release"/bundle/nsis/*-setup.exe 2>/dev/null | head -1 || true)
  [ -n "$setup" ] || die "no installer produced in $release/bundle/nsis"
  cp "$setup" dist/
  [ -f "$setup.sig" ] && cp "$setup.sig" dist/
  out="dist/$(basename "$setup")"
else
  [ -f "$release/app.exe" ] || die "build finished but $release/app.exe is missing"
  out="dist/AMS_${version}_x64-portable.exe"
  cp "$release/app.exe" "$out"
fi

echo
note "Done. Copy to the Windows PC: $out"
ls -lh dist | sed 's/^/    /'
