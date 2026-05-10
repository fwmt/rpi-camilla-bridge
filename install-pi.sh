#!/usr/bin/env bash
# rpi-camilla-bridge installer for Raspberry Pi (or any aarch64 Linux box
# running CamillaDSP via snd-aloop). Downloads the latest pre-built
# `pi-receiver` binary, drops the systemd unit and a starter bridge
# config, creates a dedicated unprivileged user, and starts the service.
#
# Usage (typical):
#   curl -sSL https://raw.githubusercontent.com/fwmt/rpi-camilla-bridge/main/install-pi.sh | sudo bash
#
# Pinning a specific version:
#   curl -sSL https://raw.githubusercontent.com/fwmt/rpi-camilla-bridge/main/install-pi.sh \
#     | sudo VERSION=v0.1.0 bash
#
# Skipping the interactive idle-config prompt (e.g. unattended re-runs):
#   sudo IDLE_CONFIG=/home/pi/camilladsp/configs/music.yml \
#     bash install-pi.sh

set -euo pipefail

REPO="${REPO:-fwmt/rpi-camilla-bridge}"
VERSION="${VERSION:-latest}"
SERVICE_USER="${SERVICE_USER:-pi-camilla}"
INSTALL_DIR="${INSTALL_DIR:-/etc/rpi-camilla-bridge}"
BIN_PATH="${BIN_PATH:-/usr/local/bin/pi-receiver}"
SERVICE_PATH="${SERVICE_PATH:-/etc/systemd/system/pi-receiver.service}"

# Script-scoped scratch directory; the EXIT trap below cleans it up no
# matter where we leave from. Declared here (not inside the function
# that uses it) so `set -u` is happy when the trap fires after the
# function returns.
WORK_DIR=""
cleanup() {
  if [ -n "${WORK_DIR:-}" ] && [ -d "$WORK_DIR" ]; then
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

log()  { printf '\033[1;34m▸\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

require_root() {
  if [ "$(id -u)" -ne 0 ]; then
    die "This installer needs root (it creates a system user, writes to /etc, /usr/local). Re-run with sudo."
  fi
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

detect_arch() {
  local arch; arch=$(uname -m)
  case "$arch" in
    aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
    *) die "Unsupported architecture: $arch (only aarch64 builds are published; build from source for $arch)" ;;
  esac
}

resolve_version() {
  if [ "$VERSION" = "latest" ]; then
    log "Resolving latest release of $REPO"
    VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
      | grep -oE '"tag_name":[^,]+' | head -1 | cut -d'"' -f4)
    if [ -z "${VERSION:-}" ]; then
      die "Could not determine latest release. Set VERSION=v0.x.y explicitly."
    fi
  fi
  log "Installing $REPO $VERSION"
}

download_binary() {
  local target; target=$(detect_arch)
  local asset="pi-receiver-${VERSION}-${target}.tar.gz"
  local url="https://github.com/$REPO/releases/download/$VERSION/$asset"
  WORK_DIR=$(mktemp -d)

  log "Downloading $asset"
  curl -fsSL "$url" -o "$WORK_DIR/$asset" \
    || die "Download failed: $url"
  curl -fsSL "$url.sha256" -o "$WORK_DIR/$asset.sha256" \
    || warn "No checksum file alongside release; skipping verification."

  if [ -f "$WORK_DIR/$asset.sha256" ]; then
    log "Verifying checksum"
    (cd "$WORK_DIR" && sha256sum -c "$asset.sha256") \
      || die "Checksum mismatch for $asset"
  fi

  log "Extracting binary"
  tar -C "$WORK_DIR" -xzf "$WORK_DIR/$asset"
  local extracted_dir; extracted_dir=$(find "$WORK_DIR" -mindepth 1 -maxdepth 1 -type d | head -1)
  install -m 0755 "$extracted_dir/pi-receiver" "$BIN_PATH"
  log "Installed binary at $BIN_PATH"

  # Stash the deploy/ files we'll use below.
  STAGED_BRIDGE_EXAMPLE="$extracted_dir/bridge.yml.example"
  STAGED_SERVICE_FILE="$extracted_dir/pi-receiver.service"
}

ensure_user() {
  if id "$SERVICE_USER" >/dev/null 2>&1; then
    log "Service user '$SERVICE_USER' already exists"
  else
    log "Creating service user '$SERVICE_USER' (system, no shell, no home, group audio)"
    useradd --system --no-create-home --shell /usr/sbin/nologin -G audio "$SERVICE_USER"
  fi
}

install_configs() {
  install -d -m 0755 "$INSTALL_DIR"

  # Bridge config
  if [ -f "$INSTALL_DIR/bridge.yml" ]; then
    log "$INSTALL_DIR/bridge.yml already present; leaving as-is"
  elif [ -f "$STAGED_BRIDGE_EXAMPLE" ]; then
    install -m 0644 "$STAGED_BRIDGE_EXAMPLE" "$INSTALL_DIR/bridge.yml"
    log "Wrote starter $INSTALL_DIR/bridge.yml — you MUST adapt the playback section to your DAC"
  else
    warn "No bridge.yml.example in the release archive; you'll need to write $INSTALL_DIR/bridge.yml by hand."
  fi

  # Idle config — interactive unless IDLE_CONFIG env is set.
  if [ -L "$INSTALL_DIR/idle.yml" ] || [ -f "$INSTALL_DIR/idle.yml" ]; then
    log "$INSTALL_DIR/idle.yml already present; leaving as-is"
  elif [ -n "${IDLE_CONFIG:-}" ]; then
    if [ -f "$IDLE_CONFIG" ]; then
      ln -sf "$IDLE_CONFIG" "$INSTALL_DIR/idle.yml"
      log "Symlinked $INSTALL_DIR/idle.yml → $IDLE_CONFIG"
    else
      die "IDLE_CONFIG=$IDLE_CONFIG does not exist."
    fi
  elif [ -t 0 ]; then
    # Stdin is a TTY — we can prompt.
    echo
    echo "Where is your existing CamillaDSP config (the one that should run when no PC is streaming)?"
    echo "We'll symlink it as $INSTALL_DIR/idle.yml so the bridge restores it on disconnect."
    echo "Common locations:"
    echo "  /home/<user>/camilladsp/configs/<name>.yml"
    echo "  /etc/camilladsp/<name>.yml"
    read -r -p "Path (leave empty to skip and configure later): " idle_path
    if [ -n "$idle_path" ]; then
      if [ -f "$idle_path" ]; then
        ln -sf "$idle_path" "$INSTALL_DIR/idle.yml"
        log "Symlinked $INSTALL_DIR/idle.yml → $idle_path"
      else
        warn "$idle_path does not exist — skipping. Configure $INSTALL_DIR/idle.yml manually before starting the service."
      fi
    else
      warn "No idle config configured. Bridge will fail to swap back on disconnect."
    fi
  else
    warn "Non-interactive run with no IDLE_CONFIG. Configure $INSTALL_DIR/idle.yml manually before starting the service."
  fi
}

install_service() {
  if [ ! -f "$STAGED_SERVICE_FILE" ]; then
    die "Release archive is missing pi-receiver.service. Try a newer release."
  fi
  install -m 0644 "$STAGED_SERVICE_FILE" "$SERVICE_PATH"
  log "Wrote $SERVICE_PATH"
  systemctl daemon-reload
}

start_service() {
  if [ -e "$INSTALL_DIR/idle.yml" ] && [ -f "$INSTALL_DIR/bridge.yml" ]; then
    log "Enabling and starting pi-receiver"
    systemctl enable --now pi-receiver.service
    sleep 1
    if systemctl is-active --quiet pi-receiver; then
      log "pi-receiver is running. Tail logs with: journalctl -u pi-receiver -f"
    else
      warn "pi-receiver failed to start. Check: systemctl status pi-receiver"
    fi
  else
    warn "Skipping start — finish $INSTALL_DIR/bridge.yml and $INSTALL_DIR/idle.yml first, then:"
    echo "    sudo systemctl enable --now pi-receiver"
  fi
}

print_next_steps() {
  cat <<EOF

────────────────────────────────────────────────────────────
Done.

  Binary:   $BIN_PATH
  User:     $SERVICE_USER (system, no shell, in 'audio' group)
  Service:  $SERVICE_PATH
  Configs:  $INSTALL_DIR/{bridge.yml, idle.yml}

Next steps on this Pi:
  1. Edit $INSTALL_DIR/bridge.yml — adapt the playback device,
     channel count, sample rate and any filters/limiters to your DAC.
  2. Verify: systemctl status pi-receiver
  3. Tail:   journalctl -u pi-receiver -f

From a PC on the same LAN:
  pc-sender --host $(hostname).local --port 9000

  - On Linux this auto-creates "rpi-camilla-bridge" in your
    Sound output settings; route any app to it and audio flows here.
  - On Windows, install VB-CABLE (free) and use --device "CABLE Output"
    until WASAPI loopback support lands.
────────────────────────────────────────────────────────────
EOF
}

main() {
  require_root
  for c in curl tar sha256sum systemctl install useradd id; do require_cmd "$c"; done

  resolve_version
  download_binary
  ensure_user
  install_configs
  install_service
  start_service
  print_next_steps
}

main "$@"
