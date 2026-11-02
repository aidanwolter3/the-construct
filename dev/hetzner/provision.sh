#!/usr/bin/env bash
# Provision a fresh Hetzner server (Ubuntu 24.04, arm64 or x86_64) as a Project Canvas
# dev/e2e box: construct server, local Pixelfed (Docker), Rust + JDK toolchains,
# headless simulator deps, Claude Code, and optionally the Android SDK/emulator,
# HTTPS via Caddy, and a GitHub Actions self-hosted runner.
#
# Run as root on the new server:
#   scp dev/hetzner/provision.sh root@<IP>:/root/
#   ssh root@<IP> 'bash /root/provision.sh [options]'
#
# Options:
#   --user NAME            login user to create (default: dev)
#   --android              install Android SDK, NDK, cargo-ndk (+ emulator if /dev/kvm exists)
#   --domain HOST          serve the construct server over HTTPS at HOST via Caddy
#                          (point the DNS A record at this server first)
#   --pixelfed-domain HOST also proxy the local Pixelfed (port 8090) at HOST over HTTPS
#   --repo URL             git clone URL for the project (e.g. git@github.com:you/the_construct.git)
#   --runner-url URL       GitHub repo URL for a self-hosted Actions runner
#   --runner-token TOKEN   registration token (repo Settings > Actions > Runners > New)
#   --swap GB              swap file size (default: 4)
#
# Re-running is safe: each step checks whether it has already been done.
#
# Security defaults: SSH keys only (password auth off), root SSH login off,
# firewall allows only 22/80/443. Ports 8080 (server) and 8090 (Pixelfed) stay
# private: use Caddy (--domain) or an SSH tunnel:  ssh -L 8080:localhost:8080 dev@<IP>

set -euo pipefail

DEV_USER=dev
WANT_ANDROID=0
DOMAIN=""
PIXELFED_DOMAIN=""
REPO_URL=""
RUNNER_URL=""
RUNNER_TOKEN=""
SWAP_GB=4

# Pinned tool versions (override via environment).
ANDROID_CMDLINE_TOOLS_ZIP="${ANDROID_CMDLINE_TOOLS_ZIP:-commandlinetools-linux-13114758_latest.zip}"
ANDROID_PLATFORM="${ANDROID_PLATFORM:-android-36}"
ANDROID_BUILD_TOOLS="${ANDROID_BUILD_TOOLS:-36.0.0}"
ANDROID_NDK="${ANDROID_NDK:-28.2.13676358}"
GH_RUNNER_VERSION="${GH_RUNNER_VERSION:-2.329.0}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user) DEV_USER="$2"; shift 2 ;;
    --android) WANT_ANDROID=1; shift ;;
    --domain) DOMAIN="$2"; shift 2 ;;
    --pixelfed-domain) PIXELFED_DOMAIN="$2"; shift 2 ;;
    --repo) REPO_URL="$2"; shift 2 ;;
    --runner-url) RUNNER_URL="$2"; shift 2 ;;
    --runner-token) RUNNER_TOKEN="$2"; shift 2 ;;
    --swap) SWAP_GB="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

log() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
as_dev() { sudo -u "$DEV_USER" -H bash -lc "$1"; }

[[ $EUID -eq 0 ]] || { echo "run as root" >&2; exit 1; }
if [[ $WANT_ANDROID -eq 1 && "$(uname -m)" != "x86_64" ]]; then
  echo "--android needs an x86_64 server (the Android SDK tools on Linux are x86_64-only)." >&2; exit 2
fi
if [[ -n "$RUNNER_URL" && -z "$RUNNER_TOKEN" ]] || [[ -z "$RUNNER_URL" && -n "$RUNNER_TOKEN" ]]; then
  echo "--runner-url and --runner-token must be given together" >&2; exit 2
fi

export DEBIAN_FRONTEND=noninteractive

# ---------------------------------------------------------------- base system
log "Base packages and security updates"
apt-get update -y
apt-get upgrade -y
apt-get install -y \
  build-essential pkg-config libssl-dev git curl wget jq sqlite3 unzip zip xz-utils \
  ca-certificates gnupg tmux htop ufw fail2ban unattended-upgrades \
  openjdk-21-jdk-headless \
  fontconfig fonts-dejavu-core fonts-noto-core libfreetype6 libgl1 libegl1 \
  libxrender1 libxtst6 libxi6 libx11-6 libxext6
dpkg-reconfigure -f noninteractive unattended-upgrades

log "Swap (${SWAP_GB}G)"
if ! swapon --show | grep -q /swapfile; then
  fallocate -l "${SWAP_GB}G" /swapfile
  chmod 600 /swapfile
  mkswap /swapfile
  swapon /swapfile
  grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

# ------------------------------------------------------------------ dev user
log "User '$DEV_USER' (sudo, SSH key copied from root)"
if ! id "$DEV_USER" >/dev/null 2>&1; then
  adduser --disabled-password --gecos "" "$DEV_USER"
fi
usermod -aG sudo "$DEV_USER"
echo "$DEV_USER ALL=(ALL) NOPASSWD:ALL" > "/etc/sudoers.d/90-$DEV_USER"
chmod 440 "/etc/sudoers.d/90-$DEV_USER"
install -d -m 700 -o "$DEV_USER" -g "$DEV_USER" "/home/$DEV_USER/.ssh"
if [[ -f /root/.ssh/authorized_keys ]]; then
  install -m 600 -o "$DEV_USER" -g "$DEV_USER" /root/.ssh/authorized_keys "/home/$DEV_USER/.ssh/authorized_keys"
fi
[[ -s "/home/$DEV_USER/.ssh/authorized_keys" ]] || {
  echo "No SSH key for $DEV_USER; refusing to lock down SSH. Add your key to /root/.ssh/authorized_keys and re-run." >&2
  exit 1
}

log "SSH hardening (keys only, no root login)"
cat > /etc/ssh/sshd_config.d/90-canvas.conf <<'EOF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
EOF
systemctl reload ssh || systemctl reload sshd

log "Firewall (22, 80, 443)"
ufw default deny incoming
ufw default allow outgoing
ufw allow OpenSSH
ufw allow 80/tcp
ufw allow 443/tcp
ufw --force enable
systemctl enable --now fail2ban

# -------------------------------------------------------------------- docker
log "Docker Engine + compose plugin"
if ! command -v docker >/dev/null; then
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    > /etc/apt/sources.list.d/docker.list
  apt-get update -y
  apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
fi
usermod -aG docker "$DEV_USER"
# Docker publishes ports past ufw; containers in dev/pixelfed should bind 127.0.0.1 only.

# ---------------------------------------------------------------- toolchains
log "Rust (rustup, stable + clippy/rustfmt)"
as_dev 'command -v rustup >/dev/null || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt'

log "Claude Code"
as_dev 'command -v claude >/dev/null || curl -fsSL https://claude.ai/install.sh | bash'

# Headless JVM rendering (simulator) needs no display; make that the default.
cat > /etc/profile.d/canvas.sh <<EOF
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-$(dpkg --print-architecture)
EOF
cat >> /etc/profile.d/canvas.sh <<'EOF'
export JAVA_TOOL_OPTIONS="${JAVA_TOOL_OPTIONS:-} -Djava.awt.headless=true"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
EOF

# ------------------------------------------------------------------- android
if [[ $WANT_ANDROID -eq 1 ]]; then
  log "Android SDK ($ANDROID_PLATFORM, build-tools $ANDROID_BUILD_TOOLS, NDK $ANDROID_NDK)"
  SDK=/home/$DEV_USER/android-sdk
  as_dev "
    set -e
    mkdir -p $SDK/cmdline-tools
    if [[ ! -x $SDK/cmdline-tools/latest/bin/sdkmanager ]]; then
      cd /tmp && curl -fsSLo cmdline-tools.zip https://dl.google.com/android/repository/$ANDROID_CMDLINE_TOOLS_ZIP
      rm -rf /tmp/cmdline-tools && unzip -q cmdline-tools.zip -d /tmp
      rm -rf $SDK/cmdline-tools/latest && mv /tmp/cmdline-tools $SDK/cmdline-tools/latest
      rm -f cmdline-tools.zip
    fi
    yes | $SDK/cmdline-tools/latest/bin/sdkmanager --licenses >/dev/null
    $SDK/cmdline-tools/latest/bin/sdkmanager --install 'platform-tools' 'platforms;$ANDROID_PLATFORM' 'build-tools;$ANDROID_BUILD_TOOLS' 'ndk;$ANDROID_NDK'
    rustup target add aarch64-linux-android x86_64-linux-android
    command -v cargo-ndk >/dev/null || cargo install cargo-ndk
  "
  cat >> /etc/profile.d/canvas.sh <<EOF
export ANDROID_HOME=$SDK
export ANDROID_SDK_ROOT=$SDK
export ANDROID_NDK_HOME=$SDK/ndk/$ANDROID_NDK
export PATH="\$PATH:$SDK/platform-tools:$SDK/emulator:$SDK/cmdline-tools/latest/bin"
EOF

  if [[ -e /dev/kvm ]]; then
    log "KVM available: installing emulator + x86_64 system image + AVD"
    apt-get install -y qemu-kvm libpulse0 libnss3 libxcomposite1 libxcursor1 libxdamage1 libxrandr2 libasound2t64 || true
    usermod -aG kvm "$DEV_USER"
    as_dev "
      set -e
      $SDK/cmdline-tools/latest/bin/sdkmanager --install 'emulator' 'system-images;$ANDROID_PLATFORM;google_apis;x86_64'
      $SDK/cmdline-tools/latest/bin/avdmanager list avd | grep -q 'Name: canvas_api36' || \
        echo no | $SDK/cmdline-tools/latest/bin/avdmanager create avd -n canvas_api36 -k 'system-images;$ANDROID_PLATFORM;google_apis;x86_64' -d pixel_7
    "
    echo "Emulator: emulator -avd canvas_api36 -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect &"
  else
    echo "NOTE: /dev/kvm not present (typical on Hetzner Cloud VPS). Skipping the emulator;"
    echo "      builds, cargo-ndk and installs to a USB/remote device still work."
  fi
fi

# --------------------------------------------------------------------- caddy
if [[ -n "$DOMAIN" ]]; then
  log "Caddy HTTPS reverse proxy for $DOMAIN -> 127.0.0.1:8080"
  if ! command -v caddy >/dev/null; then
    apt-get install -y debian-keyring debian-archive-keyring apt-transport-https
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' > /etc/apt/sources.list.d/caddy-stable.list
    apt-get update -y && apt-get install -y caddy
  fi
  {
    echo "$DOMAIN {"
    echo "  encode zstd gzip"
    echo "  reverse_proxy 127.0.0.1:8080 {"
    echo "    flush_interval -1   # stream SSE layout updates immediately"
    echo "  }"
    echo "}"
    if [[ -n "$PIXELFED_DOMAIN" ]]; then
      echo "$PIXELFED_DOMAIN {"
      echo "  reverse_proxy 127.0.0.1:8090"
      echo "}"
    fi
  } > /etc/caddy/Caddyfile
  systemctl reload caddy || systemctl restart caddy
fi

# ---------------------------------------------------------------- repository
as_dev 'test -f ~/.ssh/id_ed25519 || ssh-keygen -t ed25519 -N "" -C "canvas-dev@$(hostname)" -f ~/.ssh/id_ed25519 >/dev/null'
as_dev 'ssh-keyscan -t ed25519 github.com >> ~/.ssh/known_hosts 2>/dev/null; sort -u -o ~/.ssh/known_hosts ~/.ssh/known_hosts'
if [[ -n "$REPO_URL" ]]; then
  log "Cloning $REPO_URL"
  as_dev "
    mkdir -p ~/src
    if [[ ! -d ~/src/the_construct/.git ]]; then
      if ! git clone '$REPO_URL' ~/src/the_construct; then
        echo 'Clone failed. For a private repo, add this server key as a deploy key, then re-run:'
        cat ~/.ssh/id_ed25519.pub
      fi
    fi
  "
fi

# ----------------------------------------------------- github actions runner
if [[ -n "$RUNNER_URL" ]]; then
  log "GitHub Actions self-hosted runner for $RUNNER_URL"
  as_dev "
    set -e
    mkdir -p ~/actions-runner && cd ~/actions-runner
    if [[ ! -f .runner ]]; then
      curl -fsSLo runner.tgz https://github.com/actions/runner/releases/download/v$GH_RUNNER_VERSION/actions-runner-linux-x64-$GH_RUNNER_VERSION.tar.gz
      tar xzf runner.tgz && rm runner.tgz
      ./config.sh --unattended --url '$RUNNER_URL' --token '$RUNNER_TOKEN' --name \"\$(hostname)\" --labels canvas-e2e$( [[ -e /dev/kvm ]] && echo ',kvm' )
    fi
  "
  (cd "/home/$DEV_USER/actions-runner" && ./svc.sh install "$DEV_USER" && ./svc.sh start) || true
fi

# ------------------------------------------------------------------- summary
IP=$(curl -fsS4 https://ifconfig.me || hostname -I | awk '{print $1}')
log "Done"
cat <<EOF
Server:        $IP  ($(nproc) vCPU, $(free -g | awk '/Mem:/{print $2}') GB RAM, KVM: $([[ -e /dev/kvm ]] && echo yes || echo no))
Login:         ssh $DEV_USER@$IP          (root login is now disabled)
Docker:        $(docker --version)
Java:          $(java -version 2>&1 | head -1)
Rust:          $(as_dev 'rustc --version' 2>/dev/null || echo 'installed for $DEV_USER')
Claude Code:   run 'claude' after logging in as $DEV_USER (first run signs you in)
This server's GitHub deploy key (add to the repo if private):
  $(cat /home/$DEV_USER/.ssh/id_ed25519.pub)
Private ports: ssh -L 8080:localhost:8080 -L 8090:localhost:8090 $DEV_USER@$IP
$( [[ -n "$DOMAIN" ]] && echo "HTTPS:         https://$DOMAIN -> construct server" )
EOF
