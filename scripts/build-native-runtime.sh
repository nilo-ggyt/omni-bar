#!/bin/zsh
set -euo pipefail

# Build and package the statically linked AppKit application from locked source inputs.

project_root=${0:A:h:h}
target_triple=aarch64-apple-darwin

# Resolve the repository toolchain even when this script was invoked from another directory.
cd "$project_root"

if [[ "$(uname -s)" != "Darwin" ]]; then
  # AppKit linkage, code signing and DMG assembly require the actual macOS platform.
  print -u2 "native runtime packaging requires macOS"
  exit 2
fi

# Ask Cargo for the resolved output directory so an explicit CARGO_TARGET_DIR is honored.
target_root=$(cargo +1.99.0 metadata --manifest-path "$project_root/Cargo.toml" \
  --format-version 1 --no-deps --locked | \
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
release_binary="$target_root/$target_triple/release/omni-native-runtime"
artifact_root="$target_root/native-runtime"
bundle_root="$artifact_root/Omni Bar.app"
bundle_contents="$bundle_root/Contents"
bundle_binary="$bundle_contents/MacOS/omni-native-runtime"
dmg_path="${1:-$artifact_root/Omni-Bar-native-runtime.dmg}"
if [[ "${dmg_path:e}" != "dmg" ]]; then
  # Reject an accidental source/document output path before any existing file could be replaced.
  print -u2 "native packaging output must have the .dmg extension"
  exit 2
fi

# The committed toolchain and lockfile select all source versions; packaging cannot update them.
MACOSX_DEPLOYMENT_TARGET=14.0 cargo +1.99.0 build \
  --manifest-path "$project_root/Cargo.toml" \
  --package omni-native-runtime \
  --release \
  --locked \
  --target "$target_triple"

if [[ ! -x "$release_binary" ]]; then
  # Do not create a misleading application bundle if compilation produced no executable.
  print -u2 "native runtime binary was not produced: $release_binary"
  exit 1
fi

# Keep cleanup limited to the generated packaging directory.  The Rust target
# directory and user data are never removed by this script.
rm -rf "$bundle_root"
mkdir -p "$bundle_contents/MacOS" "$bundle_contents/Resources"
cp "$release_binary" "$bundle_binary"
cp "$project_root/packaging/native-runtime/Info.plist" \
  "$bundle_contents/Info.plist"
# Native assets are first-class source inputs, independent of the temporary comparison tree.
cp "$project_root/assets/icon.icns" "$bundle_contents/Resources/Omni Bar.icns"
# Preserve the project attribution and exact dependency/standard-library notices in the shipped app.
cp "$project_root/LICENSE" "$bundle_contents/Resources/LICENSE"
python3 "$project_root/scripts/collect-bundle-notices.py" \
  --output "$bundle_contents/Resources/ThirdPartyNotices"
chmod 755 "$bundle_binary"

# A native candidate containing any WebKit linkage or symbol is rejected before
# signing.  This catches accidental dependency drift in CI and local builds.
if otool -L "$bundle_binary" | rg 'WebKit|JavaScriptCore|WebCore' >/dev/null; then
  # Read the entire pipe so pipefail cannot hide a match behind an upstream SIGPIPE.
  print -u2 "native runtime unexpectedly links a WebKit framework"
  exit 1
fi
if strings "$bundle_binary" | rg 'WKWebView|WebViewWindow|tauri::' >/dev/null; then
  # A concrete forbidden marker rejects this artifact before signing or replacement of the DMG.
  print -u2 "native runtime contains a forbidden WebView/Tauri surface marker"
  exit 1
fi

# A single native executable needs no recursive signing shortcut or nested helper bundle.
codesign_identity=${CODESIGN_IDENTITY:--}
codesign --force --sign "$codesign_identity" \
  --timestamp=none "$bundle_root"
codesign --verify --strict "$bundle_root"

# Replace only the declared generated DMG; source, user data and Cargo outputs remain owned separately.
rm -f "$dmg_path"
mkdir -p "${dmg_path:h}"
# 使用当前原生镜像入口，保留卷名和只读压缩格式；成功结果应是未附加的镜像文件。
diskutil image create from \
  --volumeName "Omni Bar" \
  --format UDZO \
  "$bundle_root" \
  "$dmg_path" >/dev/null

print "$dmg_path"
