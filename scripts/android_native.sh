#!/usr/bin/env bash
# The Rust engine's Android app (decision `rust-render-engine`): cross-compiles libconstruct_render.so with
# cargo-ndk, packages it as a NativeActivity APK (no Gradle, no JVM UI; a few bridge classes pass Android's callbacks
# to Rust) and installs it on the adb device as The
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
# Needs: an Android SDK with an NDK and build-tools (ANDROID_HOME, else ~/android-sdk), a JDK (javac), cargo-ndk,
# `rustup target add aarch64-linux-android`, and Maven Central the first time (the UnifiedPush libraries' classes).
# Output: build/android-native/construct-render.apk.
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
  "$RES"/drawable/ic_stat_notification.xml \
  crates/construct-render/android/res/values/styles.xml \
  crates/construct-render/android/res/values-night/styles.xml
"$TOOLS/aapt2" link -o "$OUT/unsigned.apk" --manifest crates/construct-render/android/AndroidManifest.xml \
  -I "$PLATFORM/android.jar" "$OUT"/res/*.flat
# The bridge (crates/construct-render/android/java): a few Java classes that pass Android's callbacks to Rust,
# compiled with javac and converted to dex with d8.
JAVAC=""
for j in "${JAVA_HOME:+$JAVA_HOME/bin/javac}" "$(command -v javac || true)" \
  "/Applications/Android Studio.app/Contents/jbr/Contents/Home/bin/javac" /opt/android-studio/jbr/bin/javac; do
  [[ -n $j && -x $j ]] && { JAVAC=$j; break; }
done
[[ -n $JAVAC ]] || { echo "no javac (a JDK: JAVA_HOME, PATH, or Android Studio's)" >&2; exit 1; }
# Prebuilt libraries' classes go in too: the UnifiedPush connector (push while the app is closed: `PushReceiver`
# extends its receiver, `PushKeys` keeps its keys in Rust), the embedded FCM distributor (push through Play services
# on phones without a distributor app; it registers with the pod's VAPID key, so no Firebase project), and the Kotlin
# libraries they need. Not Tink, protobuf or gson, the connector's own key store's: `PushKeys` replaces it, so they
# never load. Downloaded once from Maven Central into build/android-native/libs, and checked against these hashes.
LIBS=$OUT/libs
mkdir -p "$LIBS"
fetch() { # path on Maven Central, file, sha256
  if [[ ! -f $LIBS/$2 ]] || ! echo "$3  $LIBS/$2" | shasum -a 256 -c --status - 2>/dev/null; then
    curl -fsSL --retry 4 -o "$LIBS/$2" "https://repo1.maven.org/maven2/$1/$2"
    echo "$3  $LIBS/$2" | shasum -a 256 -c --status - || { echo "$2: unexpected contents" >&2; rm -f "$LIBS/$2"; exit 1; }
  fi
}
fetch org/unifiedpush/android/connector/3.3.5 connector-3.3.5.aar \
  fa017cdfabbdc9af021e1f8c8eff2c8098e701d7a2c599937dce0b4ecf1e929b
fetch org/unifiedpush/android/embedded-fcm-distributor/3.1.0 embedded-fcm-distributor-3.1.0.aar \
  a2e730e33c54a5d59141ac6657b6969375c91e27f64ae9299fab2275e4707291
fetch org/jetbrains/kotlin/kotlin-stdlib/2.3.0 kotlin-stdlib-2.3.0.jar \
  887587c91713250ad52fe14ad9166d042c33835049890e9437f355ffc5a195b1
fetch org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.10.2 kotlinx-coroutines-core-jvm-1.10.2.jar \
  5ca175b38df331fd64155b35cd8cae1251fa9ee369709b36d42e0a288ccce3fd
unzip -oq "$LIBS/connector-3.3.5.aar" classes.jar -d "$LIBS/connector-3.3.5"
unzip -oq "$LIBS/embedded-fcm-distributor-3.1.0.aar" classes.jar -d "$LIBS/embedded-fcm-distributor-3.1.0"
JARS=("$LIBS/connector-3.3.5/classes.jar" "$LIBS/embedded-fcm-distributor-3.1.0/classes.jar"
  "$LIBS/kotlin-stdlib-2.3.0.jar" "$LIBS/kotlinx-coroutines-core-jvm-1.10.2.jar")
rm -rf "$OUT/classes" "$OUT/apk/dex" && mkdir -p "$OUT/classes" "$OUT/apk/dex"
"$JAVAC" -nowarn --release 11 -classpath "$PLATFORM/android.jar:${JARS[0]}" -d "$OUT/classes" \
  $(find crates/construct-render/android/java -name '*.java')
# Every class Rust names in a JNI signature (`L…;` in src/*.rs) must exist in the platform or the APK's classes:
# a wrong one only fails at run time, on the phone ("Method not found").
KNOWN=$( { unzip -Z1 "$PLATFORM/android.jar"; for j in "${JARS[@]}"; do unzip -Z1 "$j"; done
  (cd "$OUT/classes" && find . -name '*.class' | sed 's|^\./||'); } | sed -n 's/\.class$//p' | sort -u)
MISSING=$(grep -ohE '"[^"]*\(L[A-Za-z0-9_/$]+;[^"]*"|"L[A-Za-z0-9_/$]+;"' crates/construct-render/src/*.rs \
  | grep -oE 'L[a-z][A-Za-z0-9_]*(/[A-Za-z0-9_$]+)+;' | sed 's/^L//; s/;$//' | sort -u | comm -23 - <(echo "$KNOWN"))
[[ -z $MISSING ]] || { echo "JNI signatures name classes that don't exist: $MISSING" >&2; exit 1; }
# d8's output goes to a log: the Kotlin 2.3 libraries' class metadata is newer than d8 before build-tools 36.1
# reads, and it warns with a stack trace for each such class (thousands of lines). Harmless: only Kotlin reflection
# reads that metadata, and the app has none. Anything else d8 says is shown.
D8LOG=$OUT/d8.log
if ! "$TOOLS/d8" --release --min-api 28 --lib "$PLATFORM/android.jar" --output "$OUT/apk/dex" \
  $(find "$OUT/classes" -name '*.class') "${JARS[@]}" >"$D8LOG" 2>&1; then
  cat "$D8LOG" >&2
  exit 1
fi
OTHER=$(grep -vE '^Info: Class .* has malformed kotlin\.Metadata|^Warning: (Unexpected error during rewriting of Kotlin metadata|An error occurred when parsing kotlin metadata)|^com\.android\.tools\.r8\..*: Should never be called|^[[:space:]]+at |^$' "$D8LOG" || true)
[[ -z $OTHER ]] || echo "$OTHER" >&2
KOTLIN_METADATA=$(grep -c 'has malformed kotlin.Metadata' "$D8LOG" || true)
[[ $KOTLIN_METADATA == 0 ]] || echo "d8: skipped newer Kotlin metadata in $KOTLIN_METADATA library classes (harmless; $D8LOG)"
rm -f "$OUT"/apk/classes*.dex && cp "$OUT"/apk/dex/classes*.dex "$OUT/apk/"
(cd "$OUT/apk" && zip -qr ../unsigned.apk lib classes*.dex)
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
"$ADB" shell am start -W -n "$PKG/.ConstructActivity" >/dev/null
for _ in $(seq 1 60); do
  "$ADB" logcat -d -s construct-render | grep -q "Frame 0 drawn\|couldn't" && break
  sleep 1
done
"$ADB" logcat -d -s construct-render
