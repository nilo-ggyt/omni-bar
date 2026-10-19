#!/bin/zsh
set -euo pipefail

# Reproducible local checks for the standalone native candidate.  This script
# deliberately reports observations; it does not relabel a failed memory gate
# as success and it never inspects user note or clipboard contents.

project_root=${0:A:h:h}
target_triple=aarch64-apple-darwin
sample_seconds=${1:-10}

# Repository-relative execution selects the committed toolchain instead of the caller's default.
cd "$project_root"

if [[ "$(uname -s)" != "Darwin" ]]; then
  # Unsupported platforms cannot supply native linkage/runtime evidence.
  print -u2 "native runtime verification requires macOS"
  exit 2
fi
if [[ "$sample_seconds" != <-> || ${#sample_seconds} -gt 4 ]] || (( sample_seconds > 3600 )); then
  # Only a bounded integer observation window may reach shell arithmetic and the resident CLI.
  print -u2 "sample seconds must be an integer from 0 through 3600"
  exit 2
fi

# Match packaging's resolved output location, including an explicit CARGO_TARGET_DIR override.
target_root=$(cargo +1.99.0 metadata --manifest-path "$project_root/Cargo.toml" \
  --format-version 1 --no-deps --locked | \
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
binary="$target_root/$target_triple/release/omni-native-runtime"
if [[ ! -x "$binary" ]]; then
  # Verification requires a concrete release artifact rather than an implicit rebuild.
  print -u2 "release binary not found; run scripts/build-native-runtime.sh first"
  exit 1
fi

# `file` honors the caller's MAGIC environment variable. A Homebrew magic
# database can be newer than macOS' system `file`, which makes an otherwise
# valid native binary fail verification before any runtime check executes.
binary_arch=$(/usr/bin/env -u MAGIC /usr/bin/file -b "$binary")
print "binary: $binary_arch"
if [[ "$binary_arch" != *"arm64"* ]]; then
  # Reject an artifact for the wrong supported product architecture.
  print -u2 "native runtime is not an arm64 executable"
  exit 1
fi

linked_frameworks=$(otool -L "$binary")
if print -r -- "$linked_frameworks" | rg -q 'WebKit|JavaScriptCore|WebCore'; then
  # The captured linkage text is complete, so this check cannot be short-circuited by producer SIGPIPE.
  print -u2 "forbidden WebKit linkage found"
  exit 1
fi
print -r -- "$linked_frameworks" | sed -n '1,24p'

if strings "$binary" | rg 'WKWebView|WebViewWindow|tauri::' >/dev/null; then
  # Consume the complete producer output so pipefail cannot hide a forbidden marker.
  print -u2 "forbidden WebView/Tauri surface marker found"
  exit 1
fi

print "smoke test:"
"$binary" --smoke-test

if (( sample_seconds > 0 )); then
  # This launches the real resident state; an external sampler must collect actual resource counters.
  print "production resident observation window (${sample_seconds}s; external sampling required):"
  "$binary" --benchmark "$sample_seconds"
fi
