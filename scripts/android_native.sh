#!/usr/bin/env bash
# The Rust engine's Android app (decision `rust-render-engine`): cross-compiles libconstruct_render.so with
# cargo-ndk, packages it as a NativeActivity APK (no Gradle, no JVM UI) and installs it on the adb device as The
# Construct itself (org.theconstruct.android), over a debug build of the main app signed with this machine's debug
# key, so the app's data and sign-in stay:
#
#   scripts/android_native.sh
#
# The app opens the pod it opened last (`pod.json` in the app's private files), else the main app's server, with the
# engine (its cache there too) and the main app's own saved sessions (the keystore), else the pod's sign-in screen
# (a pod code, or a temporary pod; the last code used is filled in).
# Leaving the pod in Settings goes back to that screen. `cd app && ./gradlew :androidApp:installDebug` goes back to
# the Compose app with the same data.
#
# Needs: an Android SDK with an NDK and build-tools (ANDROID_HOME, else ~/android-sdk), cargo-ndk, and
# `rustup target add aarch64-linux-android`. Output: build/android-native/construct-render.apk.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $# -gt 0 ]]; then
  echo "usage: $0   (no options)" >&2
  exit 2
fi
PKG=org.theconstruct.android

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
mkdir -p "$OUT"

echo "==> cross-compiling (arm64-v8a)"
cargo ndk -t arm64-v8a -o "$OUT/jniLibs" build -p construct-render --features window,remote --release

echo "==> packaging"
rm -rf "$OUT/apk" && mkdir -p "$OUT/apk/lib"
cp -r "$OUT/jniLibs/arm64-v8a" "$OUT/apk/lib/"
# The launcher icon: the main app's (app/androidApp), compiled from its resources.
RES=app/androidApp/src/main/res
rm -rf "$OUT/res" && mkdir -p "$OUT/res"
"$TOOLS/aapt2" compile -o "$OUT/res" \
  "$RES"/drawable/ic_launcher_{background,foreground,monochrome}.xml \
  "$RES"/mipmap-anydpi-v26/ic_launcher{,_round}.xml "$RES"/mipmap-*dpi/ic_launcher{,_round}.png \
  crates/construct-render/android/res/values/styles.xml
"$TOOLS/aapt2" link -o "$OUT/unsigned.apk" --manifest crates/construct-render/android/AndroidManifest.xml \
  -I "$PLATFORM/android.jar" "$OUT"/res/*.flat
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
# -d: the main app's versionCode may be higher; a debuggable app may go down.
if ! INSTALLED=$("$ADB" install -r -d "$OUT/construct-render.apk" 2>&1); then
  echo "$INSTALLED" >&2
  if grep -q UPDATE_INCOMPATIBLE <<<"$INSTALLED"; then
    echo "The Construct on the phone is signed with another key (a release build, or a debug build from another" >&2
    echo "machine), so it can't be updated in place. Uninstalling it would sign you out; install a debug build" >&2
    echo "from this machine first (cd app && ./gradlew :androidApp:installDebug), or uninstall it, then run this again." >&2
  fi
  exit 1
fi
echo "$INSTALLED"
"$ADB" shell am force-stop "$PKG"

"$ADB" logcat -c
"$ADB" shell am start -W -n "$PKG/android.app.NativeActivity" >/dev/null
for _ in $(seq 1 60); do
  "$ADB" logcat -d -s construct-render | grep -q "Frame 0 drawn\|couldn't" && break
  sleep 1
done
"$ADB" logcat -d -s construct-render
