#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="Git Graph RS"
BUNDLE_DIR="$ROOT_DIR/target/bundle/${APP_NAME}.app"
CONTENTS_DIR="$BUNDLE_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

PROFILE="${1:-release}"
if [[ "$PROFILE" == "debug" ]]; then
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p git-graph-app
  BIN_PATH="$ROOT_DIR/target/debug/git-graph-app"
else
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --release -p git-graph-app
  BIN_PATH="$ROOT_DIR/target/release/git-graph-app"
fi

rm -rf "$BUNDLE_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "$BIN_PATH" "$MACOS_DIR/git-graph-app"
cp "$ROOT_DIR/assets/AppIcon.icns" "$RESOURCES_DIR/AppIcon.icns"

cat > "$CONTENTS_DIR/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>zh_CN</string>
  <key>CFBundleDisplayName</key>
  <string>Git Graph RS</string>
  <key>CFBundleExecutable</key>
  <string>git-graph-app</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.git-graph-rs.app</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Git Graph RS</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
PLIST

echo "已生成 macOS 应用包: $BUNDLE_DIR"
