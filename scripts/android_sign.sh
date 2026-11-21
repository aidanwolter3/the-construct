#!/usr/bin/env bash
# Signs an aligned, unsigned APK (scripts/android_native.sh's build/android-native/aligned.apk, or the release
# workflow's unsigned artifact). Runs only the SDK's apksigner and the JDK's keytool, never cargo or the repository's
# build scripts, so the release key is never on disk or in the environment while dependencies' code runs (#442).
#
#   scripts/android_sign.sh <aligned.apk> <signed.apk>             # this machine's debug key
#   scripts/android_sign.sh --release <aligned.apk> <signed.apk>   # the release key below, else the debug key and a
#                                                                  # warning
#
# Release signing: CONSTRUCT_ANDROID_KEYSTORE (a .jks/.p12 file), CONSTRUCT_ANDROID_KEYSTORE_PASSWORD,
# CONSTRUCT_ANDROID_KEY_ALIAS, CONSTRUCT_ANDROID_KEY_PASSWORD (default the keystore's password).
# Needs an Android SDK's build-tools (ANDROID_HOME, else ~/android-sdk) and a JDK (keytool, for the debug key).
set -euo pipefail

usage() { echo "usage: $0 [--release] <aligned.apk> <signed.apk>" >&2; exit 2; }
RELEASE=false
if [[ ${1:-} == --release ]]; then RELEASE=true; shift; fi
[[ $# -eq 2 ]] || usage
IN=$1
OUT=$2
[[ -f $IN ]] || { echo "no APK at $IN" >&2; exit 1; }

SDK=${ANDROID_HOME:-$HOME/android-sdk}
# The newest build-tools folder that holds apksigner (sorted by numeric version parts; `sort -V` isn't on every macOS).
TOOLS=$(for d in "$SDK"/build-tools/*/; do
  d=${d%/}
  if [[ -e $d/apksigner ]]; then echo "$(basename "$d") $d"; fi
done | sort -t. -k1,1n -k2,2n -k3,3n | tail -1 | cut -d' ' -f2-)
[[ -n $TOOLS ]] || { echo "no build-tools with apksigner found in $SDK" >&2; exit 1; }

if $RELEASE && [[ -n ${CONSTRUCT_ANDROID_KEYSTORE:-} ]]; then
  : "${CONSTRUCT_ANDROID_KEYSTORE_PASSWORD:?the password of the release keystore}"
  : "${CONSTRUCT_ANDROID_KEY_ALIAS:?the alias of the release key}"
  # Passwords through the environment, so they never show in a process list.
  KS_PASS=$CONSTRUCT_ANDROID_KEYSTORE_PASSWORD KEY_PASS=${CONSTRUCT_ANDROID_KEY_PASSWORD:-$CONSTRUCT_ANDROID_KEYSTORE_PASSWORD} \
    "$TOOLS/apksigner" sign --ks "$CONSTRUCT_ANDROID_KEYSTORE" --ks-key-alias "$CONSTRUCT_ANDROID_KEY_ALIAS" \
    --ks-pass env:KS_PASS --key-pass env:KEY_PASS --out "$OUT" "$IN"
  echo "signed $OUT (release, with $CONSTRUCT_ANDROID_KEY_ALIAS)"
  "$TOOLS/apksigner" verify --print-certs "$OUT" | grep -i 'SHA-256 digest' || true
  exit 0
fi

KEY=$HOME/.android/debug.keystore
if [[ ! -f $KEY ]]; then
  mkdir -p "$(dirname "$KEY")"
  keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias androiddebugkey \
    -dname "CN=Android Debug,O=Android,C=US" -keyalg RSA -validity 10000 >/dev/null
fi
"$TOOLS/apksigner" sign --ks "$KEY" --ks-pass pass:android --out "$OUT" "$IN"
if $RELEASE; then
  echo "WARNING: no CONSTRUCT_ANDROID_KEYSTORE: the release build is signed with the debug key" >&2
  echo "signed $OUT (release, debug key)"
else
  echo "signed $OUT (debug key)"
fi
