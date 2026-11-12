#!/bin/bash
# Builds AudioWallpaper.app next to this script and launches it.
set -e
cd "$(dirname "$0")"

if ! xcrun -f swiftc >/dev/null 2>&1; then
    echo "Apple's Command Line Tools are needed to build this."
    echo "An installer window should open now. When it finishes, run this script again."
    xcode-select --install || true
    exit 1
fi

APP="AudioWallpaper.app"

pkill -x AudioWallpaper 2>/dev/null || true
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"

echo "Compiling..."
xcrun swiftc -O -target "$(uname -m)-apple-macos13.0" main.swift -o "$APP/Contents/MacOS/AudioWallpaper"

# Sign with a fixed identity rule so macOS can keep the recording permission across rebuilds.
codesign --force --sign - -r='designated => identifier "local.audiowallpaper"' "$APP" 2>/dev/null \
    || codesign --force --sign - "$APP"

echo "Built $APP. Launching..."
open "$APP"
