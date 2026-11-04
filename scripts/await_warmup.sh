#!/usr/bin/env bash
# Wait for the cloud cache warm-up (scripts/cloud_setup.sh) before a build or test run in a cloud session.
#
#   scripts/await_warmup.sh            # wait up to 20 minutes; exit 0 when done, 1 when it failed
#   scripts/await_warmup.sh --status   # print the state and exit
#
# The status file says "running <pid>", "done" or "failed <steps>". When it says running but that process is
# gone (the container restarted, or a shell that ran it was killed), the warm-up is started again. No status
# file at all means the environment has no setup script: run `CONSTRUCT_SETUP_WARM=foreground bash
# scripts/cloud_setup.sh` once.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATUS_FILE="$HOME/.construct-cloud-warmup.status"
LOG_FILE="$HOME/.construct-cloud-warmup.log"
TIMEOUT_SECS="${CONSTRUCT_WARMUP_TIMEOUT:-1200}"

state() {
  local line pid
  line="$(cat "$STATUS_FILE" 2>/dev/null || true)"
  case "$line" in
    done) echo done ;;
    failed*) echo "$line" ;;
    running*)
      pid="${line#running}"
      pid="${pid// /}"
      if [[ -n $pid ]] && kill -0 "$pid" 2>/dev/null; then
        echo running
      elif pgrep -f "bash -c warm_caches" >/dev/null 2>&1; then
        echo running
      else
        echo stale
      fi
      ;;
    "") echo missing ;;
    *) echo "unknown: $line" ;;
  esac
}

if [[ ${1:-} == --status ]]; then
  state
  exit 0
fi

start=$SECONDS
restarted=0
while true; do
  s="$(state)"
  case "$s" in
    done) echo "warm-up done"; exit 0 ;;
    failed*) echo "warm-up $s (log: $LOG_FILE)"; exit 1 ;;
    missing) echo "no warm-up status: run CONSTRUCT_SETUP_WARM=foreground bash scripts/cloud_setup.sh"; exit 1 ;;
    stale)
      if ((restarted)); then
        echo "warm-up died again (log: $LOG_FILE)"
        exit 1
      fi
      echo "warm-up process is gone; restarting it in the background"
      restarted=1
      bash "$REPO_ROOT/scripts/cloud_setup.sh" >/dev/null
      ;;
    running) ;;
    *) echo "$s"; exit 1 ;;
  esac
  if ((SECONDS - start > TIMEOUT_SECS)); then
    echo "still running after ${TIMEOUT_SECS}s; last log lines:"
    tail -n 5 "$LOG_FILE" 2>/dev/null
    exit 1
  fi
  sleep 10
done
