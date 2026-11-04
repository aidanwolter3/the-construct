#!/usr/bin/env bash
# Local, loopback-only Pixelfed instance for end-to-end tests (DEV ONLY; never points at a public instance).
# Compose project: dev/pixelfed/docker-compose.yml, served at http://localhost:8090 (127.0.0.1 only).
# Requires colima + docker CLI + compose plugin (see docs/testing.md "A local real Pixelfed").
#
# Usage: scripts/pixelfed_dev.sh <command>
#   up      start colima if needed, compose up -d, wait until healthy, first-run init
#           (passport keys, instance actor, personal-access OAuth client). Idempotent.
#   down    compose down (keeps volumes = keeps users/posts)
#   reset   compose down -v (DELETES all Pixelfed data) and removes dev/pixelfed/.seed
#   seed    up + create users constructadmin (admin) and alice, mint access tokens, admin follows alice,
#           alice posts a 3-image carousel, a single image, an alt-text post and a CW/sensitive post,
#           bob posts one image and alice boosts it (so the admin home timeline contains a reblog).
#           Users/posts are skipped if they already exist; fresh tokens are minted on every run.
#           Tokens: dev/pixelfed/.seed/{admin,alice,bob}_token (gitignored).
#   record  save the home timeline (plain, with boosts, with _pe=1; admin token) to crates/app-pixelfed/tests/data/recorded/,
#           which app-pixelfed's normalization tests read
#   status  colima + compose status and an API probe
#   logs    follow compose logs (extra args go to `docker compose logs`, e.g. `logs app`)
#
# Env: PIXELFED_URL (default http://localhost:8090), COLIMA_CPU/COLIMA_MEMORY/COLIMA_DISK (2/4/15).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/dev/pixelfed"
SEED_DIR="$DIR/.seed"
IMG_DIR="$ROOT/scripts/pixelfed_dev/images"
REC_DIR="$ROOT/crates/app-pixelfed/tests/data/recorded"
URL="${PIXELFED_URL:-http://localhost:8090}"
export PATH="$HOME/.local/bin:$PATH"

ADMIN_USER=constructadmin
ADMIN_PASS=construct-dev-pass
ALICE_USER=alice
ALICE_PASS=alice-dev-pass
BOB_USER=bob
BOB_PASS=bob-dev-pass

dc() { docker compose --project-directory "$DIR" -f "$DIR/docker-compose.yml" "$@"; }
artisan() { dc exec -T app php artisan "$@"; }
tinker() { artisan tinker --execute="$1" | tr -d '\r'; }
log() { printf '[pixelfed_dev] %s\n' "$*" >&2; }
json() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

ensure_colima() {
  command -v colima >/dev/null || { log "colima not found in ~/.local/bin (see docs/testing.md)"; exit 1; }
  if ! colima status >/dev/null 2>&1; then
    log "starting colima"
    colima start --cpu "${COLIMA_CPU:-2}" --memory "${COLIMA_MEMORY:-4}" --disk "${COLIMA_DISK:-15}" \
      --vm-type vz --mount-type virtiofs
  fi
}

wait_healthy() {
  local deadline=$((SECONDS + 300)) svc st ok
  while :; do
    ok=1
    for svc in db redis app worker; do
      st="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' \
        "$(dc ps -q "$svc")" 2>/dev/null || echo missing)"
      [ "$st" = healthy ] || ok=0
    done
    [ $ok = 1 ] && break
    [ $SECONDS -lt $deadline ] || { dc ps; log "timed out waiting for healthy containers"; exit 1; }
    sleep 3
  done
  log "all containers healthy"
}

first_run_init() {
  if ! dc exec -T app test -f storage/oauth-private.key; then
    log "generating passport keys + instance actor"
    artisan passport:keys --no-interaction
    artisan instance:actor --no-interaction || true
  fi
  # The upstream image's .dockerignore drops storage/app/public/, so the default avatar/header that account
  # JSON points at (/storage/avatars/default.jpg, /storage/headers/missing.png) would 404/403. Install
  # placeholders into the storage volume.
  local p=storage/app/public
  dc exec -T app test -f "$p/avatars/default.jpg" ||
    dc exec -T app sh -c "mkdir -p $p/avatars && cat >$p/avatars/default.jpg" <"$IMG_DIR/default_avatar.jpg"
  dc exec -T app test -f "$p/headers/missing.png" ||
    dc exec -T app sh -c "mkdir -p $p/headers && cat >$p/headers/missing.png" <"$IMG_DIR/missing_header.png"
  # Passport 13 treats a first-party confidential client as a client_credentials client and rejects a token
  # whose user id equals its client id (TokenGuard). Start client ids at 1000 so they never collide with the
  # few dev user ids.
  local n
  n="$(tinker 'echo DB::table("oauth_clients")->where("personal_access_client",1)->count();' | tail -1)"
  if [ "$n" = 0 ]; then
    log "creating personal access OAuth client"
    tinker 'if (DB::table("oauth_clients")->max("id") < 1000) DB::statement("ALTER TABLE oauth_clients AUTO_INCREMENT = 1000");' >/dev/null
    artisan passport:client --personal --name="Construct Dev PAT" --provider=users --no-interaction >/dev/null
  fi
}

cmd_up() {
  ensure_colima
  dc up -d
  wait_healthy
  first_run_init
  log "Pixelfed is up at $URL"
}

ensure_user() { # username name email password is_admin
  local exists
  exists="$(tinker "echo App\\Models\\User::whereUsername('$1')->exists() ? 'yes' : 'no';" | tail -1)"
  if [ "$exists" = yes ]; then
    log "user $1 exists"
  else
    artisan user:create --name="$2" --username="$1" --email="$3" --password="$4" --is_admin="$5" --confirm_email=1 >/dev/null
    log "created user $1"
  fi
}

mint_token() { # username -> token on stdout
  tinker "echo App\\Models\\User::whereUsername('$1')->firstOrFail()->createToken('construct-dev-seed', ['read','write','follow','push'])->accessToken;" | tail -1
}

api() { # token method path [curl args...]
  local tok="$1" m="$2" p="$3"; shift 3
  curl -fsS -X "$m" -H "Authorization: Bearer $tok" -H 'Accept: application/json' "$URL$p" "$@"
}

upload() { # token file description -> media id
  # Pixelfed quirk: POST /api/v1/media answers 200 `[]` while users.last_active_at is still NULL (a fresh
  # account's first API write). The call itself sets it, so retry.
  local out i
  for i in 1 2 3; do
    out="$(api "$1" POST /api/v1/media -F "file=@$2;type=image/jpeg" -F "description=$3")"
    [ "$out" = "[]" ] || { json 'd["id"]' <<<"$out"; return; }
    sleep 1
  done
  log "media upload kept returning []"; return 1
}

post() { # token status [extra curl args...] -> status id
  local tok="$1" text="$2"; shift 2
  api "$tok" POST /api/v1/statuses --data-urlencode "status=$text" -d visibility=public "$@" | json 'd["id"]'
}

cmd_seed() {
  cmd_up
  mkdir -p "$SEED_DIR"
  ensure_user "$ADMIN_USER" "Construct Admin" constructadmin@example.test "$ADMIN_PASS" true
  ensure_user "$ALICE_USER" "Alice Example" alice@example.test "$ALICE_PASS" 0
  ensure_user "$BOB_USER" "Bob Example" bob@example.test "$BOB_PASS" 0
  local at lt bt alice_id count
  at="$(mint_token "$ADMIN_USER")"; lt="$(mint_token "$ALICE_USER")"; bt="$(mint_token "$BOB_USER")"
  (umask 077; printf '%s\n' "$at" >"$SEED_DIR/admin_token"; printf '%s\n' "$lt" >"$SEED_DIR/alice_token"
   printf '%s\n' "$bt" >"$SEED_DIR/bob_token")

  alice_id="$(api "$lt" GET /api/v1/accounts/verify_credentials | json 'd["id"]')"
  api "$at" POST "/api/v1/accounts/$alice_id/follow" >/dev/null
  log "constructadmin follows alice ($alice_id)"

  count="$(api "$lt" GET "/api/v1/accounts/$alice_id/statuses?limit=1" | json 'len(d)')"
  if [ "$count" != 0 ]; then
    log "alice already has posts; skipping post creation"
  else
    local m1 m2 m3 m4 m5 m6 m7 bob_post
    # bob (not followed by constructadmin) posts once; alice boosts it so the admin's home has a reblog.
    m7="$(upload "$bt" "$IMG_DIR/construct_07.jpg" "Teal square with the number 7")"
    bob_post="$(post "$bt" "Bob's post, boosted by alice" -d "media_ids[]=$m7")"
    m1="$(upload "$lt" "$IMG_DIR/construct_01.jpg" "Red square with the number 1")"
    m2="$(upload "$lt" "$IMG_DIR/construct_02.jpg" "Blue square with the number 2")"
    m3="$(upload "$lt" "$IMG_DIR/construct_03.jpg" "Green square with the number 3")"
    post "$lt" "Carousel: one, two, three #construct" -d "media_ids[]=$m1" -d "media_ids[]=$m2" -d "media_ids[]=$m3" >/dev/null
    m4="$(upload "$lt" "$IMG_DIR/construct_04.jpg" "")"
    post "$lt" "A single portrait image (4:5)" -d "media_ids[]=$m4" >/dev/null
    m5="$(upload "$lt" "$IMG_DIR/construct_05.jpg" "Yellow landscape card showing a large white number 5 inside a white border")"
    post "$lt" "Landscape image with alt text" -d "media_ids[]=$m5" >/dev/null
    m6="$(upload "$lt" "$IMG_DIR/construct_06.jpg" "Orange square with the number 6")"
    post "$lt" "Sensitive post behind a content warning" -d "media_ids[]=$m6" -d sensitive=true \
      --data-urlencode "spoiler_text=CW: orange" >/dev/null
    # Fan-out only inserts into home feeds that already exist in Redis, and a cold-boot rebuild picks up followed
    # accounts' posts but not their boosts: build the admin's feed (non-empty now) before alice boosts.
    sleep 2
    api "$at" GET "/api/v1/timelines/home?limit=1" >/dev/null
    api "$lt" POST "/api/v1/statuses/$bob_post/reblog" >/dev/null
    # Pixelfed requires media on statuses; a text-only post is expected to fail (kept as a probe).
    if post "$lt" "Text-only post" >/dev/null 2>&1; then log "text-only post accepted"; else log "text-only post rejected (expected)"; fi
    log "alice posted the seed statuses"
  fi

  cat <<EOF

Pixelfed dev instance: $URL   (API also on http://127.0.0.1:8090; web/OAuth pages need Host "localhost")
  admin: $ADMIN_USER / $ADMIN_PASS   token file: dev/pixelfed/.seed/admin_token
  user:  $ALICE_USER / $ALICE_PASS        token file: dev/pixelfed/.seed/alice_token
  user:  $BOB_USER / $BOB_PASS            token file: dev/pixelfed/.seed/bob_token
Admin access token:
$at
EOF
}

cmd_record() {
  local at
  at="$(cat "$SEED_DIR/admin_token" 2>/dev/null)" || { log "run seed first"; exit 1; }
  mkdir -p "$REC_DIR"
  api "$at" GET "/api/v1/timelines/home?limit=20" | python3 -m json.tool >"$REC_DIR/timeline_home.json"
  # Boosts are excluded from the home timeline unless include_reblogs=true (or the user setting enable_reblogs).
  api "$at" GET "/api/v1/timelines/home?limit=20&include_reblogs=true" | python3 -m json.tool >"$REC_DIR/timeline_home_reblogs.json"
  # Same timeline with Pixelfed's extended entities (?_pe=1: pf_type, liked_by, media mime/orientation, ...).
  api "$at" GET "/api/v1/timelines/home?limit=20&_pe=1" | python3 -m json.tool >"$REC_DIR/timeline_home_pe.json"
  log "recorded into $REC_DIR"
}

cmd_status() {
  colima status 2>&1 || true
  if colima status >/dev/null 2>&1; then
    dc ps
    curl -fsS "$URL/api/v1/instance" | json '"API ok: " + d["title"] + " / " + d["version"]' || log "API not reachable"
  fi
}

case "${1:-}" in
  up) cmd_up ;;
  down) dc down ;;
  reset) dc down -v; rm -rf "$SEED_DIR"; log "volumes removed" ;;
  seed) cmd_seed ;;
  record) cmd_record ;;
  status) cmd_status ;;
  logs) shift; dc logs -f "$@" ;;
  *) while IFS= read -r l; do case $l in '#!'*) ;; '#'*) printf '%s\n' "${l#\#}" ;; *) break ;; esac; done <"$0"; exit 2 ;;
esac
