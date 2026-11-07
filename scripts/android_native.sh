#!/usr/bin/env bash
# The Rust engine's Android app (decision `rust-render-engine`): cross-compiles libconstruct_render.so with
# cargo-ndk, packages it as a NativeActivity APK (no Gradle, no JVM UI), and, with a device or emulator on adb,
# installs it, checks the lifecycle in logcat, takes a screenshot and compares the device's own frame of the
# task-list fixture with the desktop golden.
#
#   scripts/android_native.sh              # build, then test on the adb device if there is one
#   scripts/android_native.sh --build-only # build the APK only
#   scripts/android_native.sh --pod https://the-construct-pod.fly.dev
#                                          # install and open that pod live (read-only): asks for the pod code
#                                          # (or reads CONSTRUCT_POD_CODE), checks it with the pod, and puts the
#                                          # session token the pod answers (on today's server, the pod code
#                                          # itself) in the app's private storage, unencrypted (the main app
#                                          # keeps it in the Android keystore)
#   scripts/android_native.sh --no-pod     # install and go back to the demo
#
# Needs: an Android SDK with an NDK and build-tools (ANDROID_HOME, else ~/android-sdk), cargo-ndk, and
# `rustup target add aarch64-linux-android`. Output: build/android-native/construct-render.apk.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE=test POD_URL=
case "${1:-}" in
  --build-only) MODE=build ;;
  --pod) MODE=pod POD_URL=${2:?--pod needs the pod URL, e.g. https://the-construct-pod.fly.dev} ;;
  --no-pod) MODE=demo ;;
  "") ;;
  *) echo "usage: $0 [--build-only | --pod <url> | --no-pod]" >&2; exit 2 ;;
esac
POD_URL=${POD_URL%/}

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
cargo ndk -t arm64-v8a -o "$OUT/jniLibs" build -p construct-render --features window,remote --release

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

[[ $MODE == build ]] && exit 0
# adb from PATH, else the SDK's own (a shell alias doesn't reach this script).
ADB=$(command -v adb || true)
[[ -n $ADB ]] || ADB=$SDK/platform-tools/adb
if [[ ! -x $ADB ]]; then
  echo "no adb (not on PATH, nor $SDK/platform-tools/adb): built only"
  exit 0
fi
if [[ $("$ADB" get-state 2>/dev/null || true) != device ]]; then
  echo "no adb device ready ($ADB devices lists none, or more than one: set ANDROID_SERIAL): built only"
  exit 0
fi
echo "device: $("$ADB" get-serialno) via $ADB"

echo "==> installing"
"$ADB" install -r "$OUT/construct-render.apk"

if [[ $MODE == pod || $MODE == demo ]]; then
  "$ADB" shell am force-stop "$PKG"
  if [[ $MODE == pod ]]; then
    CODE=${CONSTRUCT_POD_CODE:-}
    if [[ -z $CODE ]]; then
      read -rsp "Pod code for $POD_URL: " CODE
      echo
    fi
    # The pod checks the code and answers the session token to use.
    OPENED=$(curl -fsS -X POST "$POD_URL/v1/pods/open" -H 'content-type: application/json' \
      -d "{\"token\": \"$CODE\"}") || { echo "couldn't open the pod (wrong code?)" >&2; exit 1; }
    SESSION=$(printf '%s' "$OPENED" | sed -n 's/.*"session_token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    [[ -n $SESSION ]] || { echo "the pod answered no session_token" >&2; exit 1; }
    printf '{"url": "%s", "token": "%s"}' "$POD_URL" "$SESSION" |
      "$ADB" shell "run-as $PKG sh -c 'mkdir -p files && cat > files/pod.json'"
    echo "session saved in the app's private storage (--no-pod removes it)"
  else
    "$ADB" shell "run-as $PKG rm -f files/pod.json"
    "$ADB" shell rm -f "/sdcard/Android/data/$PKG/files/layout.json"
  fi
  "$ADB" logcat -c
  "$ADB" shell am start -W -n "$PKG/android.app.NativeActivity" >/dev/null
  for _ in $(seq 1 60); do
    "$ADB" logcat -d -s construct-render | grep -q "Frame 0 drawn\|couldn't open" && break
    sleep 1
  done
  "$ADB" logcat -d -s construct-render
  exit 0
fi

echo "==> launching the test layout"
FILES=/sdcard/Android/data/$PKG/files
"$ADB" shell mkdir -p "$FILES"
"$ADB" push crates/construct-render/tests/data/task_list.json "$FILES/layout.json" >/dev/null
"$ADB" shell touch "$FILES/frame-request"
"$ADB" logcat -c
"$ADB" shell am start -W -n "$PKG/android.app.NativeActivity" >/dev/null

echo "==> waiting for the first frame"
for _ in $(seq 1 60); do
  "$ADB" logcat -d -s construct-render | grep -q "Frame 0 drawn" && break
  sleep 1
done
"$ADB" logcat -d -s construct-render | tee "$OUT/logcat.txt"
grep -q "surface initialized successfully" "$OUT/logcat.txt"
grep -q "Frame 0 drawn" "$OUT/logcat.txt"

echo "==> screenshot and frame comparison"
"$ADB" exec-out screencap -p > "$OUT/android_capture.png"
"$ADB" pull "$FILES/frame.png" "$OUT/device_frame.png" >/dev/null
cargo run -q -p construct-render --no-default-features --example frame_diff -- \
  "$OUT/device_frame.png" crates/construct-render/tests/data/goldens/task_list.png 0.01
echo "screenshot: $OUT/android_capture.png"
