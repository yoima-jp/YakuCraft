#!/usr/bin/env bash
set -euo pipefail

cd /workspace
export CARGO_TARGET_DIR=/opt/yakucraft-cargo-target
export YAKUCRAFT_SCREENSHOTS_DIR=/workspace/e2e/screenshots/linux-docker-verify
export YAKUCRAFT_XDG_OPEN_LOG=/tmp/yakucraft-xdg-open.log
export YAKUCRAFT_E2E_XDG_OPEN=1
signing_key_file=
trap 'if [[ -n "$signing_key_file" ]]; then rm -f "$signing_key_file"; fi' EXIT

mkdir -p "$(dirname "$YAKUCRAFT_SCREENSHOTS_DIR")"
rm -f "$YAKUCRAFT_XDG_OPEN_LOG"
mkdir -p /tmp/yakucraft-linux-verify-bin
cat > /tmp/yakucraft-linux-verify-bin/xdg-open <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$YAKUCRAFT_XDG_OPEN_LOG"
SH
chmod +x /tmp/yakucraft-linux-verify-bin/xdg-open
export PATH="/tmp/yakucraft-linux-verify-bin:$PATH"

npm ci
npm run check:version
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run typecheck
npm run lint
npm test

signing_key_file=$(mktemp)
npm run tauri -- signer generate --write-keys "$signing_key_file" --ci --force
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$signing_key_file")"
npm run tauri build
unset TAURI_SIGNING_PRIVATE_KEY
rm -f "$signing_key_file"
signing_key_file=

appimage=$(find "$CARGO_TARGET_DIR/release/bundle" -type f -name '*.AppImage' -print -quit)
deb=$(find "$CARGO_TARGET_DIR/release/bundle" -type f -name '*.deb' -print -quit)
test -n "$appimage"
test -n "$deb"
printf 'AppImage: %s\n' "$appimage"
printf 'deb: %s\n' "$deb"
dpkg-deb -I "$deb"
depends=$(dpkg-deb -f "$deb" Depends)
printf 'deb Depends: %s\n' "$depends"
[[ "$depends" == *libwebkit2gtk-4.1-0* ]]

bash scripts/linux-verify/verify-secret-service.sh

export YAKUCRAFT_E2E_LINUX_EXTRAS=1
env -u APPIMAGE xvfb-run -a npm run test:e2e

export YAKUCRAFT_E2E_SPEC=/workspace/e2e/specs/linux-runtime.spec.mjs
export YAKUCRAFT_E2E_EXPECT_APPIMAGE=1
export APPIMAGE=/tmp/yakucraft-linux-verify.AppImage
xvfb-run -a npm run test:e2e

unset APPIMAGE YAKUCRAFT_E2E_SPEC YAKUCRAFT_E2E_EXPECT_APPIMAGE
xvfb-run -a npm run test:e2e:screens

test -s "$YAKUCRAFT_XDG_OPEN_LOG"
printf 'xdg-open calls:\n'
cat "$YAKUCRAFT_XDG_OPEN_LOG"
printf 'Screenshots: %s\n' "$YAKUCRAFT_SCREENSHOTS_DIR"
printf 'Linux verification completed.\n'
