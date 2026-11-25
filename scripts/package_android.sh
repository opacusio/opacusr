#!/usr/bin/env bash
# Repackage the cargo-apk output into the final release APK.
#
# cargo-apk 0.10 can only declare the single NativeActivity — the
# phone-as-host MediaProjection shim needs a second activity (consent
# dialog routing, NativeActivity cannot override onActivityResult) and
# a foreground service, i.e. classes.dex + a fuller manifest. This
# script (CI, after `cargo apk build`):
#   1. compiles the Java shim  → javac + d8 → classes.dex
#   2. links our full AndroidManifest.xml with aapt2 (res included)
#   3. assembles lib/*.so (from the cargo-apk output) + classes.dex
#   4. zip (resources.arsc STORED), zipalign, apksigner (release keystore)
#
# Usage: package_android.sh <cargo-apk.apk> <out.apk> <versionName>
# Env:   ANDROID_HOME (build-tools 34 + platforms/android-34 required),
#        JDK (javac), CARGO_APK_RELEASE_KEYSTORE (optional, default
#        release.keystore in the repo root) + its password.
set -euo pipefail

APK_IN="${1:?usage: package_android.sh <cargo-apk.apk> <out.apk> <versionName>}"
APK_OUT="${2:?usage: package_android.sh <cargo-apk.apk> <out.apk> <versionName>}"
VER_NAME="${3:?usage: package_android.sh <cargo-apk.apk> <out.apk> <versionName>}"

: "${ANDROID_HOME:?ANDROID_HOME must point to the Android SDK}"
BT="$(ls -d "$ANDROID_HOME"/build-tools/34* 2>/dev/null | sort -V | tail -1 || true)"
if [ -z "$BT" ]; then
    echo "no build-tools 34.x under $ANDROID_HOME/build-tools" >&2
    exit 1
fi
JAR="$ANDROID_HOME/platforms/android-34/android.jar"
if [ ! -f "$JAR" ]; then
    echo "missing $JAR (platform android-34)" >&2
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/crates/opacusr-android"
KS="${CARGO_APK_RELEASE_KEYSTORE:-$ROOT/release.keystore}"
KS_PASS="${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:?CARGO_APK_RELEASE_KEYSTORE_PASSWORD required}"
# Monotonic forever: minutes since epoch dwarfs any cargo-apk scheme
# (0.12.1 shipped 16780289) and only ever increases.
VER_CODE=$(( $(date +%s) / 60 ))

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "[package] versionName=$VER_NAME versionCode=$VER_CODE"

# 1. Java shim → classes.dex (anonymous inner classes included).
#    --release 8 keeps the bytecode at class-file v52 — trivially within
#    every d8's desugaring support (no lambdas/invokedynamic in the shim).
mkdir -p "$WORK/classes"
javac --release 8 -classpath "$JAR" -d "$WORK/classes" "$SRC"/java/io/opacusr/app/*.java
mapfile -t CLASSES < <(find "$WORK/classes" -name '*.class')
"$BT/d8" --release --min-api 24 --output "$WORK" "${CLASSES[@]}"
test -f "$WORK/classes.dex"

# 2. Manifest + resources via aapt2.
"$BT/aapt2" compile --dir "$SRC/res" -o "$WORK/res.zip"
"$BT/aapt2" link -o "$WORK/base.apk" -I "$JAR" --manifest "$SRC/AndroidManifest.xml" \
    --min-sdk-version 24 --target-sdk-version 34 \
    --version-code "$VER_CODE" --version-name "$VER_NAME" \
    "$WORK/res.zip"

# 3. Payload: native libs from cargo-apk + dex + aapt2 output.
mkdir -p "$WORK/final"
unzip -q -o "$APK_IN" 'lib/*' -d "$WORK/final"
test -f "$WORK/final/lib/arm64-v8a/libopacusr_android.so"
cp "$WORK/classes.dex" "$WORK/final/classes.dex"
unzip -q -o "$WORK/base.apk" -d "$WORK/final"

# 4. Zip: resources.arsc must be STORED+aligned for targetSdk 30+.
rm -f "$WORK/unsigned.apk" "$WORK/aligned.apk"
(cd "$WORK/final" && zip -q -X -0 "$WORK/unsigned.apk" resources.arsc)
(cd "$WORK/final" && zip -q -X -9 -r "$WORK/unsigned.apk" AndroidManifest.xml classes.dex res lib)

"$BT/zipalign" -f 4 "$WORK/unsigned.apk" "$WORK/aligned.apk"
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$KS_PASS" --out "$APK_OUT" "$WORK/aligned.apk"
"$BT/apksigner" verify "$APK_OUT"

echo "[package] OK: $APK_OUT"
