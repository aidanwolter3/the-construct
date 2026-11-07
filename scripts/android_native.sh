#!/usr/bin/env bash
# The Rust engine's Android app (decision `rust-render-engine`): cross-compiles libconstruct_render.so with
# cargo-ndk, packages it as a NativeActivity APK (no Gradle, no JVM UI), and, with a device or emulator on adb,
# installs it, checks the lifecycle in logcat, takes a screenshot and compares the device's own frame of the
# task-list fixture with the desktop golden.
#
#   scripts/android_native.sh              # build, then test on the adb device if there is one
#   scripts/android_native.sh --build-only # build the APK only
#
# Needs: an Android SDK with an NDK and build-tools (ANDROID_HOME, else ~/android-sdk), cargo-ndk, and
# `rustup target add aarch64-linux-android`. Output: build/android-native/construct-render.apk.
set -euo pipefail
cd "$(dirname "$0")/.."

SDK=${ANDROID_HOME:-$HOME/android-sdk}
# The newest folder under $1 that holds $2: a real install, not a leftover such as `28.2.13676358.bak`.
# Sorted by numeric version parts (`sort -V` isn't on every macOS).
newest() {
  local d
  for d in "$1"/*/; do
    d=${d%/}
    [[ -e $d/$2 ]] && echo "$(basename "$d" | sed 's/^android-//') $d"
  done | sort -t. -k1,1n -k2,2n -k3,3n | tail -1 | cut -d' ' -f2-
}
TOOLS=$(newest "$SDK/build-tools" aapt2)
PLATFORM=$(newest "$SDK/platforms" android.jar)
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$(newest "$SDK/ndk" source.properties)}
missing() { echo "no $1 found in $SDK (install it in Android Studio's SDK Manager)" >&2; exit 1; }
[[ -n $TOOLS ]] || missing build-tools
[[ -n $PLATFORM ]] || missing "SDK platform"
[[ -n $ANDROID_NDK_HOME ]] || missing NDK
echo "NDK: $ANDROID_NDK_HOME"
OUT=build/android-native
PKG=org.theconstruct.render
mkdir -p "$OUT"

echo "==> cross-compiling (arm64-v8a)"
cargo ndk -t arm64-v8a -o "$OUT/jniLibs" build -p construct-render --features window --release

echo "==> packaging"
rm -rf "$OUT/apk" && mkdir -p "$OUT/apk/lib"
cp -r "$OUT/jniLibs/arm64-v8a" "$OUT/apk/lib/"
"$TOOLS/aapt2" link -o "$OUT/unsigned.apk" --manifest crates/construct-render/android/AndroidManifest.xml \
  -I "$PLATFORM/android.jar"
(cd "$OUT/apk" && zip -qr ../unsigned.apk lib)
"$TOOLS/zipalign" -f -p 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"
KEY=$HOME/.android/debug.keystore
if [[ ! -f $KEY ]]; then
  mkdir -p "$(dirname "$KEY")"
  keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias androiddebugkey \
    -dname "CN=Android Debug,O=Android,C=US" -keyalg RSA -validity 10000 >/dev/null
fi
"$TOOLS/apksigner" sign --ks "$KEY" --ks-pass pass:android --out "$OUT/construct-render.apk" "$OUT/aligned.apk"
echo "built $OUT/construct-render.apk"

[[ ${1:-} == --build-only ]] && exit 0
if ! command -v adb >/dev/null || [[ -z $(adb devices | sed 1d | grep -w device || true) ]]; then
  echo "no adb device: built only"
  exit 0
fi

echo "==> installing and launching"
adb install -r "$OUT/construct-render.apk"
FILES=/sdcard/Android/data/$PKG/files
adb shell mkdir -p "$FILES"
adb push crates/construct-render/tests/data/task_list.json "$FILES/layout.json" >/dev/null
adb shell touch "$FILES/frame-request"
adb logcat -c
adb shell am start -W -n "$PKG/android.app.NativeActivity" >/dev/null

echo "==> waiting for the first frame"
for _ in $(seq 1 60); do
  adb logcat -d -s construct-render | grep -q "Frame 0 drawn" && break
  sleep 1
done
adb logcat -d -s construct-render | tee "$OUT/logcat.txt"
grep -q "surface initialized successfully" "$OUT/logcat.txt"
grep -q "Frame 0 drawn" "$OUT/logcat.txt"

echo "==> screenshot and frame comparison"
adb exec-out screencap -p > "$OUT/android_capture.png"
adb pull "$FILES/frame.png" "$OUT/device_frame.png" >/dev/null
cargo run -q -p construct-render --no-default-features --example frame_diff -- \
  "$OUT/device_frame.png" crates/construct-render/tests/data/goldens/task_list.png 0.01
echo "screenshot: $OUT/android_capture.png"
