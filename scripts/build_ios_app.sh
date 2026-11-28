#!/usr/bin/env bash
# Build the OpacusR iOS app shell: link libopacusr_ios.a into a real
# Mach-O via the Xcode (swiftc) toolchain and package an UNSIGNED .ipa.
#
# This is the step `cargo check --target aarch64-apple-ios` cannot do:
# it proves the whole static library actually LINKS (every symbol from
# eframe/wgpu/winit/ring/... resolves against the iOS SDK frameworks).
#
# The IPA is NOT signed (no Apple Developer certificate in CI) — install
# by self-signing with a free Personal Team in Xcode, or via AltStore /
# Sideloadly; see ios/README.md. TestFlight/App Store is the follow-up
# stage and needs a paid Apple Developer account.
#
# Environment:
#   PROFILE  cargo profile for libopacusr_ios.a (debug | release);
#            default: debug (fast CI verification)
#   VERSION  bundle marketing version; default: parsed from Cargo.toml
#   OUT_DIR  working directory for the bundle; default: target/ios-shell
#
# Output: opacusr-<VERSION>-ios-unsigned.ipa in the repo root (plus the
# unpacked OpacusR.app and Payload/ under OUT_DIR).
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="${PROFILE:-debug}"
OUT_DIR="${OUT_DIR:-target/ios-shell}"
VERSION="${VERSION:-}"

# 1. Version: env override, else the workspace version (release prep
#    bumps Cargo.toml and the tag together, so this matches the tag).
if [ -z "$VERSION" ]; then
  VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
  test -n "$VERSION" || { echo "cannot parse version from Cargo.toml"; exit 1; }
fi

case "$PROFILE" in
  release) EXTRA=(--release) ;;
  debug) EXTRA=() ;;
  *) echo "PROFILE must be debug or release (got: $PROFILE)"; exit 1 ;;
esac
# macOS ships bash 3.2 where an EMPTY array under `set -u` explodes on
# "${EXTRA[@]}" — expand via the classic guard instead.
EXTRA_EXPANDED=${EXTRA[@]+"${EXTRA[@]}"}

mkdir -p "$OUT_DIR"

# 2. Compile the Rust static library AND capture the native link flags
#    in one pass: rustc's `native-static-libs` printout is the canonical
#    list of frameworks/libraries needed when linking a binary against
#    libopacusr_ios.a (Metal, UIKit, Security, CoreGraphics, ...). Hand
#    maintaining that list would rot with every wgpu/winit bump.
echo "==> cargo rustc -p opacusr-ios ($PROFILE) + native-static-libs"
cargo rustc --color=never -p opacusr-ios --target aarch64-apple-ios $EXTRA_EXPANDED \
  -- --print native-static-libs >/dev/null 2>"$OUT_DIR/native-libs.log"
NATIVE=$(sed -n 's/.*native-static-libs: //p' "$OUT_DIR/native-libs.log" |
  # Belt and braces for ANSI color codes: even with --color=never the
  # CI env (CARGO_TERM_COLOR=always) can re-enable them, and a stray
  # ESC sequence becomes a phantom "<U+001B>" linker input. Strip all
  # SGR sequences, then take the last line.
  sed 's/\x1b\[[0-9;]*m//g' | tail -1 |
  # iOS has no separate libm (it lives inside libSystem) — rustc's
  # generic native-static-libs still emits `-lm`, which makes ld fail
  # with "library 'm' not found". Duplicate -lSystem/-lc/-lobjc are
  # harmless (ld only warns).
  sed 's/-lm//g')
test -n "$NATIVE" || { echo "could not capture native-static-libs"; cat "$OUT_DIR/native-libs.log"; exit 1; }
echo "    native libs: $NATIVE"

LIB="target/aarch64-apple-ios/$PROFILE/libopacusr_ios.a"
test -f "$LIB" || { echo "missing $LIB"; exit 1; }

# 3. Bundle skeleton.
APP_DIR="$OUT_DIR/OpacusR.app"
rm -rf "$APP_DIR" "$OUT_DIR/Payload"
mkdir -p "$APP_DIR"
sed "s/@VERSION@/$VERSION/g" ios/App/Info.plist.in > "$APP_DIR/Info.plist"
plutil -lint "$APP_DIR/Info.plist"

# 4. Compile Swift entry + link. -ObjC is cheap insurance: it forces the
#    linker to pull ObjC metadata objects out of the static archive even
#    when nothing references them by symbol (objc2 registers its classes
#    at runtime, but the flag costs nothing and removes one failure
#    mode). Native flags go LAST so the linker resolves our archive's
#    dependencies against the frameworks left-to-right.
echo "==> swiftc (arm64-apple-ios13.0) + link"
# -parse-as-library: swiftc otherwise treats the single source file as
# top-level script code and rejects @main ("'main' attribute cannot be
# used in a module that contains top-level code").
xcrun -sdk iphoneos swiftc \
  -parse-as-library \
  -target arm64-apple-ios13.0 -O \
  -import-objc-header ios/App/bridging.h \
  ios/App/AppDelegate.swift \
  -L "target/aarch64-apple-ios/$PROFILE" \
  -lopacusr_ios \
  -Xlinker -ObjC \
  $NATIVE \
  -o "$APP_DIR/OpacusR"

# 5. Sanity: the Mach-O must be arm64, dylib-linked against the iOS SDK
#    and contain our entry symbol.
file "$APP_DIR/OpacusR"
otool -L "$APP_DIR/OpacusR" | head -8
# nm via a FILE, not a pipe: a debug-profile binary carries hundreds of
# thousands of symbols and `grep -q` closes the pipe early — nm dies
# with SIGPIPE ("LLVM ERROR: IO failure") before it ever reaches the
# sorted `_opacusr_*` region. Also -gU: global + defined only.
nm -gU "$APP_DIR/OpacusR" > "$OUT_DIR/symbols.txt" 2>/dev/null || true
grep -q "_opacusr_ios_run" "$OUT_DIR/symbols.txt" || {
  echo "entry symbol _opacusr_ios_run missing from the linked binary"
  exit 1
}
echo "    entry symbol _opacusr_ios_run: present"

# 6. Package the unsigned IPA (Payload/<App>.app layout).
mkdir -p "$OUT_DIR/Payload"
cp -R "$APP_DIR" "$OUT_DIR/Payload/"
( cd "$OUT_DIR" && zip -rq "opacusr-${VERSION}-ios-unsigned.ipa" Payload )
cp "$OUT_DIR/opacusr-${VERSION}-ios-unsigned.ipa" ./
ls -la "opacusr-${VERSION}-ios-unsigned.ipa"
echo "==> done: opacusr-${VERSION}-ios-unsigned.ipa (UNSIGNED)"
