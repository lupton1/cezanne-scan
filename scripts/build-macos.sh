#!/bin/bash
# Builds the macOS release on a Mac: one universal app for Intel and Apple Silicon, signed
# ad hoc, and a dmg. The same steps as the macOS job in .github/workflows/build.yml.
#
#   scripts/build-macos.sh
#
# Needs the Xcode command line tools (xcode-select --install) and Rust (https://rustup.rs).
# The dmg is written to dist/.
set -euo pipefail

cd "$(dirname "$0")/.."
PACKAGER_VERSION=0.11.8
export MACOSX_DEPLOYMENT_TARGET=10.15

if [ "$(uname)" != Darwin ]; then
  echo "This builds the Mac version and has to run on a Mac."
  exit 1
fi
if ! xcode-select -p >/dev/null 2>&1; then
  echo "The Xcode command line tools are missing. Run: xcode-select --install"
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is missing. Install it with:"
  echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi

echo "== Rust targets for Intel and Apple Silicon"
rustup target add aarch64-apple-darwin x86_64-apple-darwin

if ! cargo packager --version 2>/dev/null | grep -q "$PACKAGER_VERSION"; then
  echo "== Installing cargo-packager $PACKAGER_VERSION (once, takes a few minutes)"
  cargo install cargo-packager --locked --version "$PACKAGER_VERSION"
fi

echo "== Tests"
cargo test --workspace

echo "== Building for Apple Silicon and for Intel"
cargo build --release --workspace --target aarch64-apple-darwin
cargo build --release --workspace --target x86_64-apple-darwin

echo "== Joining them into universal programs"
U=target/universal-apple-darwin/release
mkdir -p "$U"
for b in cezanne-scan cezanne-cli cezanne-bridge cezanne-server; do
  lipo -create -output "$U/$b" "target/aarch64-apple-darwin/release/$b" "target/x86_64-apple-darwin/release/$b"
  lipo -info "$U/$b"
done

echo "== Packaging the app"
cargo packager -c Packager.toml --release --target universal-apple-darwin --formats app --out-dir "$U"
APP="$U/Cezanne Scan.app"
# no Apple Developer ID: an ad hoc signature lets Apple Silicon Macs run it
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

echo "== Quick check of the server against the simulator"
"$APP/Contents/MacOS/cezanne-server" --listen 127.0.0.1:7899 --connect sim --output-dir "$(mktemp -d)" >/dev/null 2>&1 &
PID=$!
sleep 2
if curl -s http://127.0.0.1:7899/api/info | grep -q '"version"'; then
  echo "server answered"
else
  echo "WARNING: the server did not answer"
fi
kill $PID 2>/dev/null || true

echo "== Making the dmg"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Packager.toml | head -1)
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
mkdir -p dist
DMG="dist/Cezanne Scan_${VERSION}_universal.dmg"
hdiutil create -volname "Cezanne Scan" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

echo
echo "Done: $DMG"
