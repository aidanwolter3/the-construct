#!/usr/bin/env bash
# Android launch smoke test: installs the debug app on an emulator/device, launches it for one or more
# workspaces and checks that it stays alive and renders. `assembleDebug` and the JVM host tests can't catch
# Android-only failures (e.g. ICU regex syntax, missing .so, JNA loading), this can.
#
# Usage: scripts/android_smoke.sh [options]
#
#   --boot              boot AVD $SMOKE_AVD (default Medium_Phone_API_36) headless if no device is attached,
#                       and shut it down at the end
#   --server            start the server (cargo build -p server, then target/debug/server) if nothing is
#                       listening on 127.0.0.1:8080, and stop it at the end
#   --pixelfed          also launch the dev apps (same as --workspaces pixelfed_home,pixelfed_demo,home)
#   --workspaces a,b    workspaces to launch, in order (default: pixelfed_home, the app's default).
#                       Known: pixelfed_home, pixelfed_demo, home
#   --out DIR           output dir for screenshots, UI dumps and logcat
#                       (default: kmp-app/androidApp/build/smoke)
#   --base-url URL      server the app uses, passed as the debug `--es baseUrl` extra (the app itself defaults to
#                       the staging pod; default here: http://10.0.2.2:8080, the local server seen from the emulator;
#                       a physical phone needs `adb reverse tcp:8080 tcp:8080` and http://127.0.0.1:8080)
#   --skip-install      don't run :androidApp:installDebug (use the installed app)
#   --skip-rust         pass -PskipRustBuild to Gradle (package the existing jniLibs)
#   --wait SECONDS      settle time after each launch before checking (default 5)
#   --timeout SECONDS   max extra time to wait for the expected text to appear (default 20)
#
# Without --boot, a device must already be attached (ANDROID_SERIAL selects one if several are).
# Without --server, the server must already be reachable on 127.0.0.1:8080 (the emulator uses 10.0.2.2:8080).
#
# Checks per workspace (any failure -> non-zero exit):
#   - logcat (main, system, crash buffers) has no "FATAL EXCEPTION" after the launch
#   - the app process is alive (adb shell pidof org.theconstruct.android)
#   - `uiautomator dump` contains the menu button (content-desc "Menu") and the expected text:
#       pixelfed_home: "Sign in with Pixelfed" (a fresh install is signed out) or a "Like" button (signed in)
#       pixelfed_demo: "Sunrise over Boulder"
#       home:          "Count: <n>"
# Saves <ws>.png (screencap), <ws>.xml (UI dump) and <ws>.logcat.txt to the output dir.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="org.theconstruct.android"
ACTIVITY="$PKG/.MainActivity"
AVD="${SMOKE_AVD:-Medium_Phone_API_36}"
SERVER_URL="http://127.0.0.1:8080"
APP_BASE_URL="http://10.0.2.2:8080"

BOOT=0
SERVER=0
INSTALL=1
SKIP_RUST=0
WORKSPACES="pixelfed_home"
OUT_DIR="$REPO_ROOT/kmp-app/androidApp/build/smoke"
WAIT=5
TIMEOUT=20

usage() { grep '^#' "${BASH_SOURCE[0]}" | grep -v '^#!' | cut -c3-; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --boot) BOOT=1 ;;
    --server) SERVER=1 ;;
    --pixelfed) WORKSPACES="pixelfed_home,pixelfed_demo,home" ;;
    --workspaces) WORKSPACES="${2:?--workspaces needs a value}"; shift ;;
    --out) OUT_DIR="${2:?--out needs a value}"; shift ;;
    --base-url) APP_BASE_URL="${2:?--base-url needs a value}"; shift ;;
    --skip-install) INSTALL=0 ;;
    --skip-rust) SKIP_RUST=1 ;;
    --wait) WAIT="${2:?--wait needs a value}"; shift ;;
    --timeout) TIMEOUT="${2:?--timeout needs a value}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
export PATH="$HOME/.cargo/bin:$SDK/platform-tools:$SDK/emulator:$PATH"
if [[ -z "${JAVA_HOME:-}" && -d "/Applications/Android Studio.app/Contents/jbr/Contents/Home" ]]; then
  export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
fi
command -v adb >/dev/null || { echo "adb not found (looked in $SDK/platform-tools)" >&2; exit 2; }
mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"

log() { echo "[smoke] $*"; }

# --- cleanup: only what this script started ----------------------------------------------------------
STARTED_SERVER_PID=""
STARTED_EMU_PID=""
STARTED_EMU_SERIAL=""
cleanup() {
  local rc=$?
  set +e
  if [[ -n "$STARTED_SERVER_PID" ]]; then
    log "stopping server (pid $STARTED_SERVER_PID)"
    kill "$STARTED_SERVER_PID" 2>/dev/null
    wait "$STARTED_SERVER_PID" 2>/dev/null
  fi
  if [[ -n "$STARTED_EMU_PID" ]]; then
    log "shutting down emulator $STARTED_EMU_SERIAL (pid $STARTED_EMU_PID)"
    adb -s "$STARTED_EMU_SERIAL" emu kill >/dev/null 2>&1
    for _ in $(seq 1 30); do kill -0 "$STARTED_EMU_PID" 2>/dev/null || break; sleep 1; done
    if kill -0 "$STARTED_EMU_PID" 2>/dev/null; then kill "$STARTED_EMU_PID" 2>/dev/null; fi
  fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# --- device ------------------------------------------------------------------------------------------
device_count() { adb devices | awk 'NR > 1 && $2 == "device"' | wc -l | tr -d ' '; }

if [[ -z "${ANDROID_SERIAL:-}" && "$(device_count)" == "0" ]]; then
  if [[ "$BOOT" == 1 ]]; then
    # A fixed console port gives a known serial, so cleanup kills exactly this emulator.
    PORT=5584
    STARTED_EMU_SERIAL="emulator-$PORT"
    log "booting AVD $AVD headless as $STARTED_EMU_SERIAL (log: $OUT_DIR/emulator.log)"
    emulator -avd "$AVD" -port "$PORT" -no-window -no-audio -no-boot-anim -no-snapshot-save \
      >"$OUT_DIR/emulator.log" 2>&1 &
    STARTED_EMU_PID=$!
    export ANDROID_SERIAL="$STARTED_EMU_SERIAL"
    deadline=$((SECONDS + 240))
    until [[ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" == "1" ]]; do
      kill -0 "$STARTED_EMU_PID" 2>/dev/null || { echo "emulator exited; see $OUT_DIR/emulator.log" >&2; exit 1; }
      (( SECONDS < deadline )) || { echo "emulator did not finish booting in 240s" >&2; exit 1; }
      sleep 2
    done
    log "emulator booted"
  else
    echo "no device attached; boot one or pass --boot" >&2
    exit 2
  fi
fi
if [[ -z "${ANDROID_SERIAL:-}" && "$(device_count)" != "1" ]]; then
  echo "several devices attached; set ANDROID_SERIAL" >&2
  exit 2
fi
log "device: ${ANDROID_SERIAL:-$(adb devices | awk 'NR > 1 && $2 == "device" {print $1}')}"

# --- server ------------------------------------------------------------------------------------------
server_up() { curl -fsS --max-time 2 "$SERVER_URL/healthz" >/dev/null 2>&1; }

if server_up; then
  log "server already running at $SERVER_URL"
elif [[ "$SERVER" == 1 ]]; then
  log "building server"
  (cd "$REPO_ROOT" && cargo build -q -p server)
  log "starting server (log: $OUT_DIR/server.log)"
  "$REPO_ROOT/target/debug/server" >"$OUT_DIR/server.log" 2>&1 &
  STARTED_SERVER_PID=$!
  deadline=$((SECONDS + 30))
  until server_up; do
    kill -0 "$STARTED_SERVER_PID" 2>/dev/null || { echo "server exited; see $OUT_DIR/server.log" >&2; STARTED_SERVER_PID=""; exit 1; }
    (( SECONDS < deadline )) || { echo "server not reachable after 30s" >&2; exit 1; }
    sleep 0.5
  done
  log "server up (pid $STARTED_SERVER_PID)"
else
  echo "server not reachable at $SERVER_URL; start it (cargo run -p server) or pass --server" >&2
  exit 2
fi

# --- install -----------------------------------------------------------------------------------------
if [[ "$INSTALL" == 1 ]]; then
  gradle_args=(:androidApp:installDebug -q)
  [[ "$SKIP_RUST" == 1 ]] && gradle_args+=(-PskipRustBuild)
  log "installing: ./gradlew ${gradle_args[*]}"
  (cd "$REPO_ROOT/kmp-app" && ./gradlew "${gradle_args[@]}")
fi

# --- launch + checks ---------------------------------------------------------------------------------
# Each pattern is an ERE matched against the dump's `text="..."` and `content-desc="..."` attributes.
expected_patterns() {
  printf '%s\n' '^Menu$'  # the shared UI's hamburger button, on every app
  case "$1" in
    pixelfed_home) printf '%s\n' '(Sign in with Pixelfed|Like)' ;;
    pixelfed_demo) printf '%s\n' 'Sunrise over Boulder' ;;
    home) printf '%s\n' 'Count: [0-9]+' ;;
    *) ;;  # unknown workspace: only the menu/crash/alive checks apply
  esac
}

# No grep -q: exiting early would SIGPIPE sed and fail the pipeline under pipefail.
attr_matches() {  # $1 = ERE, $2 = UI dump: true if some text or content-desc value matches
  grep -Eo '(text|content-desc)="[^"]*"' "$2" | sed -E 's/^[a-z-]+="(.*)"$/\1/' | grep -E -- "$1" >/dev/null
}

ui_dump() {  # $1 = local output file
  adb shell uiautomator dump /sdcard/smoke_ui.xml >/dev/null 2>&1 || return 1
  adb exec-out cat /sdcard/smoke_ui.xml >"$1" 2>/dev/null
}

FAILED=0
SUMMARY=()
IFS=',' read -r -a WS_LIST <<<"$WORKSPACES"
for ws in "${WS_LIST[@]}"; do
  ws="$(echo "$ws" | tr -d ' ')"
  [[ -n "$ws" ]] || continue
  log "--- workspace $ws"
  problems=()
  adb logcat -c -b main -b system -b crash || adb logcat -c
  adb shell am start -S -W -n "$ACTIVITY" --es baseUrl "$APP_BASE_URL" --es workspace "$ws" >/dev/null
  sleep "$WAIT"

  # Wait (bounded) for the expected text; stop early if the app dies.
  patterns=()
  while IFS= read -r p; do [[ -n "$p" ]] && patterns+=("$p"); done < <(expected_patterns "$ws")
  missing=("${patterns[@]+"${patterns[@]}"}")
  deadline=$((SECONDS + TIMEOUT))
  while :; do
    adb shell pidof "$PKG" >/dev/null 2>&1 || break
    if ui_dump "$OUT_DIR/$ws.xml"; then
      missing=()
      for p in "${patterns[@]+"${patterns[@]}"}"; do
        attr_matches "$p" "$OUT_DIR/$ws.xml" || missing+=("$p")
      done
      [[ ${#missing[@]} -eq 0 ]] && break
    fi
    (( SECONDS < deadline )) || break
    sleep 1
  done

  adb exec-out screencap -p >"$OUT_DIR/$ws.png" || problems+=("screencap failed")
  adb logcat -d -b main -b system -b crash >"$OUT_DIR/$ws.logcat.txt" 2>/dev/null \
    || adb logcat -d >"$OUT_DIR/$ws.logcat.txt"

  if grep -q "FATAL EXCEPTION" "$OUT_DIR/$ws.logcat.txt"; then
    problems+=("FATAL EXCEPTION in logcat")
    grep -A 12 "FATAL EXCEPTION" "$OUT_DIR/$ws.logcat.txt" | sed 's/^/    /' >&2 || true
  fi
  if ! adb shell pidof "$PKG" >/dev/null 2>&1; then
    problems+=("app process not running")
  fi
  for p in "${missing[@]+"${missing[@]}"}"; do problems+=("text not found: /$p/"); done

  if [[ ${#problems[@]} -eq 0 ]]; then
    SUMMARY+=("PASS  $ws")
  else
    FAILED=1
    SUMMARY+=("FAIL  $ws: $(IFS=';'; echo "${problems[*]}")")
  fi
done

echo
echo "Android smoke summary (output: $OUT_DIR)"
for line in "${SUMMARY[@]}"; do echo "  $line"; done
for ws in "${WS_LIST[@]}"; do
  ws="$(echo "$ws" | tr -d ' ')"; [[ -n "$ws" ]] || continue
  echo "  $ws: $OUT_DIR/$ws.png  $OUT_DIR/$ws.xml  $OUT_DIR/$ws.logcat.txt"
done
exit "$FAILED"
