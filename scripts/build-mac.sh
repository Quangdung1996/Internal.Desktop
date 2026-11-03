#!/usr/bin/env bash
# Builds AMS for macOS (.app + .dmg) on a Mac and copies the results to dist/.
#
#   npm run build:mac            universal (Apple silicon + Intel), release, signed + notarized if possible
#   npm run build:mac -- --native   this Mac's chip only (faster)
#   npm run build:mac:debug      quick debug .app for this Mac (no .dmg, no notarization)
#
# Signing    APPLE_SIGNING_IDENTITY, else the first "Developer ID Application" identity in the
#            keychain, else ad-hoc (Gatekeeper then blocks the first launch: "Open Anyway").
# Notarizing APPLE_ID + APPLE_PASSWORD (app-specific) + APPLE_TEAM_ID: Tauri notarizes (CI, DevOps).
#            Otherwise the notarytool keychain profile NOTARY_PROFILE (default AMS_NOTARY, made with
#            `xcrun notarytool store-credentials`): this script notarizes and staples the .dmg.
#            Neither: skipped, with a warning.
# Updater    .app.tar.gz + .sig only with TAURI_SIGNING_PRIVATE_KEY (a release); without it they are
#            skipped so a local build still works.
# Never put any of these values in the repo (it is public): environment or keychain only.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

die() { echo "✗ $*" >&2; exit 1; }
note() { echo "• $*"; }
warn() { echo "⚠ $*"; }

[ "$(uname -s)" = Darwin ] || die "macOS builds need a Mac (use GitHub Actions otherwise)."
command -v cargo >/dev/null || die "Rust is missing: https://rustup.rs"
command -v xcrun >/dev/null || die "Xcode Command Line Tools are missing: xcode-select --install"

debug=false
target=universal-apple-darwin
for arg in "$@"; do
  case "$arg" in
    --debug) debug=true; target="" ;;
    --native) target="" ;;
    -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
    *) die "unknown option: $arg (see --help)" ;;
  esac
done

[ -d node_modules ] || npm install
version=$(node -p "require('./src-tauri/tauri.conf.json').version")

# --- Signing identity -------------------------------------------------------------------------
if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  found=$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)
  if [ -n "$found" ]; then
    export APPLE_SIGNING_IDENTITY="$found"
  fi
fi
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  note "Signing as: $APPLE_SIGNING_IDENTITY"
else
  warn "No Developer ID certificate: ad-hoc signing (users must 'Open Anyway' on first launch)."
fi

# --- Notarization -----------------------------------------------------------------------------
notary=none
profile="${NOTARY_PROFILE:-AMS_NOTARY}"
if $debug; then
  note "Debug build: no notarization."
elif [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  warn "Not notarizing: an ad-hoc app cannot be notarized."
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  notary=tauri
  note "Notarizing via APPLE_ID ($APPLE_ID)."
elif [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ] && [ -n "${APPLE_API_KEY_PATH:-}" ]; then
  notary=tauri
  note "Notarizing via App Store Connect API key $APPLE_API_KEY."
elif xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1; then
  notary=profile
  note "Notarizing via keychain profile '$profile'."
else
  warn "Not notarizing: set APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID or run 'xcrun notarytool store-credentials $profile'."
fi

# --- Build ------------------------------------------------------------------------------------
args=()
if $debug; then
  args+=(--debug --bundles app)
else
  args+=(--bundles app,dmg)
fi
if [ -n "$target" ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  args+=(--target "$target")
fi
if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  warn "No TAURI_SIGNING_PRIVATE_KEY: building without updater files (fine for testing, not for a release)."
  args+=(--config '{"bundle":{"createUpdaterArtifacts":false}}')
fi

note "Building AMS $version for macOS (${target:-this Mac}, $($debug && echo debug || echo release))…"
npm run tauri build -- "${args[@]}"

profile_dir=$($debug && echo debug || echo release)
bundle="src-tauri/target/${target:+$target/}$profile_dir/bundle"
app="$bundle/macos/AMS.app"
[ -d "$app" ] || die "build finished but $app is missing"

# --- Notarize with the keychain profile (Tauri covers the env-var cases itself) ---------------
dmg=$(ls "$bundle"/dmg/*.dmg 2>/dev/null | head -1 || true)
if [ "$notary" = profile ] && [ -n "$dmg" ]; then
  note "Submitting $dmg to Apple (a few minutes)…"
  out=$(xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait 2>&1) || true
  echo "$out" | tail -4
  echo "$out" | grep -q "status: Accepted" || die "notarization was not accepted (see above; 'xcrun notarytool log <id> --keychain-profile $profile')"
  xcrun stapler staple "$dmg" >/dev/null
  xcrun stapler staple "$app" >/dev/null
  note "Notarized and stapled."
fi

# --- Collect into dist/ -----------------------------------------------------------------------
mkdir -p dist
if $debug; then
  rm -rf dist/AMS-debug.app
  cp -R "$app" dist/AMS-debug.app
  note "Done: dist/AMS-debug.app"
  exit 0
fi
[ -n "$dmg" ] || die "no .dmg produced in $bundle/dmg"
cp "$dmg" dist/
for f in "$bundle"/macos/*.app.tar.gz "$bundle"/macos/*.app.tar.gz.sig; do
  [ -f "$f" ] && cp "$f" dist/
done

echo
note "Gatekeeper check of the app:"
spctl -a -vv "$app" 2>&1 | sed 's/^/    /' || true
echo
note "Done. Send users: dist/$(basename "$dmg")"
ls -lh dist | sed 's/^/    /'
