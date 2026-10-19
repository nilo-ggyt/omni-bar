#!/bin/zsh
set -euo pipefail

omni_benchmark_root=${0:A:h:h}
omni_benchmark_sources="$omni_benchmark_root/benchmarks/macos-native-memory-floor"
omni_benchmark_output=${1:-/private/tmp/omni-native-memory-floor}

mkdir -p "$omni_benchmark_output"

xcrun clang -std=c11 -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_sources/resident_shortcut_host.c" \
  -framework Carbon -framework CoreFoundation \
  -o "$omni_benchmark_output/resident-shortcut-host"

omni_rustc_release_flags=(
  --edition=2024
  --target aarch64-apple-darwin
  -C opt-level=z
  -C lto=fat
  -C codegen-units=1
  -C panic=abort
  -C strip=symbols
  -C link-arg=-mmacosx-version-min=14.0
)

MACOSX_DEPLOYMENT_TARGET=14.0 rustc "${omni_rustc_release_flags[@]}" \
  "$omni_benchmark_sources/resident_shortcut_host.rs" \
  -o "$omni_benchmark_output/rust-resident-shortcut-host"

MACOSX_DEPLOYMENT_TARGET=14.0 rustc "${omni_rustc_release_flags[@]}" \
  --cfg omni_shortcut_probe \
  "$omni_benchmark_sources/resident_shortcut_host.rs" \
  -o "$omni_benchmark_output/rust-resident-shortcut-probe"

xcrun clang -fobjc-arc -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_sources/launcher_surface.m" \
  -framework AppKit \
  -o "$omni_benchmark_output/launcher-surface"

xcrun clang -fobjc-arc -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_sources/textkit_markdown_surface.m" \
  -framework AppKit \
  -o "$omni_benchmark_output/textkit-markdown-surface"

xcrun clang -fobjc-arc -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_sources/mmap_piece_table_markdown_surface.m" \
  -framework AppKit \
  -o "$omni_benchmark_output/mmap-markdown-surface"

xcrun clang -fobjc-arc -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_root/scripts/macos-launcher-input-probe.m" \
  -framework Carbon -framework CoreGraphics -framework Foundation \
  -o "$omni_benchmark_output/launcher-input-probe"

xcrun clang -std=c11 -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_root/scripts/macos-global-shortcut-probe.c" \
  -framework Carbon -framework CoreGraphics \
  -o "$omni_benchmark_output/global-shortcut-probe"

xcrun clang -std=c11 -Wall -Wextra -Werror -Os -arch arm64 \
  -mmacosx-version-min=14.0 -Wl,-dead_strip \
  "$omni_benchmark_root/scripts/macos-process-rusage-probe.c" \
  -o "$omni_benchmark_output/process-rusage-probe"

codesign --force --sign - "$omni_benchmark_output/resident-shortcut-host"
codesign --force --sign - "$omni_benchmark_output/rust-resident-shortcut-host"
codesign --force --sign - "$omni_benchmark_output/rust-resident-shortcut-probe"
codesign --force --sign - "$omni_benchmark_output/launcher-surface"
codesign --force --sign - "$omni_benchmark_output/textkit-markdown-surface"
codesign --force --sign - "$omni_benchmark_output/mmap-markdown-surface"
codesign --force --sign - "$omni_benchmark_output/launcher-input-probe"
codesign --force --sign - "$omni_benchmark_output/global-shortcut-probe"
codesign --force --sign - "$omni_benchmark_output/process-rusage-probe"

"$omni_benchmark_output/mmap-markdown-surface" --self-test \
  "$omni_benchmark_sources/fixtures/mmap-piece-table-unicode.md"
