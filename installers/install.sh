#!/usr/bin/env bash
set -Eeuo pipefail

# One-shot installer for systemd-based Linux hosts. The installed agent is
# always addressed through a Nix profile; no distro package manager is used for
# agent dependencies.

FLAKE_URL="${STATIX_FLAKE_URL:-github:StatixSolutions/statix-agent}"
FLAKE_ATTR="${STATIX_FLAKE_ATTR:-statix-agent-runtime}"
PROFILE="${STATIX_AGENT_PROFILE:-/nix/var/nix/profiles/statix-agent}"
CONFIG_DIR="${STATIX_CONFIG_DIR:-/etc/statix}"
STATE_DIR="${STATIX_AGENT_STATE_DIR:-/var/lib/statix-agent}"
SERVICE_USER="${STATIX_SERVICE_USER:-statix-agent}"

log() { printf '[statix-bootstrap] %s\n' "$*"; }
fail() { printf '[statix-bootstrap] error: %s\n' "$*" >&2; exit 1; }

[[ "$EUID" -eq 0 ]] || fail "run as root"
command -v systemctl >/dev/null || fail "systemd is required"
[[ -r /etc/os-release ]] || fail "cannot detect Linux distribution"

install_nix() {
  # The installer is commonly run through `sudo ... bash`, so it does not
  # inherit the interactive shell setup that adds an existing daemon install
  # to PATH. Load the standard profile first; otherwise reinstalling an agent
  # attempts to install Nix a second time and the Nix installer aborts when its
  # backup files already exist.
  for profile in \
    /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh \
    /etc/profile.d/nix-daemon.sh \
    /root/.nix-profile/etc/profile.d/nix.sh; do
    if [[ -r "$profile" ]]; then
      # shellcheck disable=SC1090
      . "$profile"
    fi
  done

  # Keep detection working even when the profile script is missing or has not
  # added the daemon profile to PATH yet.
  for nix_bin in \
    /nix/var/nix/profiles/default/bin/nix \
    /run/current-system/sw/bin/nix \
    /root/.nix-profile/bin/nix; do
    if [[ -x "$nix_bin" ]]; then
      PATH="${nix_bin%/*}:$PATH"
      export PATH
      break
    fi
  done

  hash -r 2>/dev/null || true
  if command -v nix >/dev/null 2>&1; then return; fi
  command -v curl >/dev/null 2>&1 || fail "curl is required to bootstrap Nix"
  log "installing the Nix daemon"
  curl --proto '=https' --tlsv1.2 -fsSL "${STATIX_NIX_INSTALLER_URL:-https://nixos.org/nix/install}" \
    | sh -s -- --daemon
  # The daemon installer normally installs this profile. Load it for the
  # remainder of this process without assuming a distro-specific shell.
  for profile in /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh /etc/profile.d/nix-daemon.sh; do
    if [[ -r "$profile" ]]; then . "$profile"; break; fi
  done
  command -v nix >/dev/null 2>&1 || fail "Nix installation did not provide nix"
}

prepare_host() {
  getent group "$SERVICE_USER" >/dev/null 2>&1 || groupadd --system "$SERVICE_USER"
  id -u "$SERVICE_USER" >/dev/null 2>&1 || useradd --system --gid "$SERVICE_USER" \
    --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
  install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$CONFIG_DIR" "$STATE_DIR"
  if [[ -f /etc/statix/agent.json && ! -f "$CONFIG_DIR/agent.json" ]]; then
    install -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0400 /etc/statix/agent.json "$CONFIG_DIR/agent.json"
  fi
}

disable_legacy() {
  local unit
  for unit in statix-agent.service statix-agent-update.service statix-agent-update.timer; do
    systemctl disable --now "$unit" >/dev/null 2>&1 || true
  done
  rm -f \
    /usr/local/bin/statix-agent \
    /usr/local/lib/statix/update.sh \
    /usr/local/lib/statix/statix-agent-dependencies.sh \
    /usr/local/libexec/statix-agent-lxc \
    /usr/local/libexec/statix-agent-network \
    /etc/systemd/system/statix-agent.service \
    /etc/systemd/system/statix-agent-update.service \
    /etc/systemd/system/statix-agent-update.timer \
    /etc/sudoers.d/statix-agent
}

install_profiles() {
  local features=(--extra-experimental-features 'nix-command flakes')
  nix "${features[@]}" profile install --profile "$PROFILE" "$FLAKE_URL#$FLAKE_ATTR"
}

install_command() {
  local command_path=/usr/local/bin/statix-agent
  install -d -m 0755 "${command_path%/*}"
  ln -sfn "$PROFILE/bin/statix-agent" "$command_path"
  [[ -x "$command_path" ]] || fail "installed agent command is not executable at $command_path"
}

install_units() {
  install -d -m 0755 /etc/systemd/system
  cat >/etc/systemd/system/statix-agent.service <<EOF
[Unit]
Description=Statix Agent
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart=$PROFILE/bin/statix-agent run
Environment=STATIX_AGENT_CONFIG=$CONFIG_DIR/agent.json
Environment=STATIX_AGENT_STATE_DIR=$STATE_DIR
Environment=HOME=$STATE_DIR
Environment=PATH=$PROFILE/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin
Environment=STATIX_LXC_HELPER_PATH=$PROFILE/bin/statix-agent-lxc
Restart=always
RestartSec=5s
KillSignal=SIGINT
KillMode=process
TimeoutStopSec=20s
User=$SERVICE_USER
Group=$SERVICE_USER
StateDirectory=statix-agent
ReadWritePaths=$STATE_DIR

[Install]
WantedBy=multi-user.target
EOF
  cat >/etc/sudoers.d/statix-agent <<EOF
Defaults!$PROFILE/bin/statix-agent-lxc secure_path="$PROFILE/bin:/usr/bin:/bin"
$SERVICE_USER ALL=(root) NOPASSWD: $PROFILE/bin/statix-agent-lxc *
$SERVICE_USER ALL=(root) NOPASSWD: /usr/bin/systemctl start statix-agent-update.service
EOF
  chmod 0440 /etc/sudoers.d/statix-agent
  if command -v visudo >/dev/null 2>&1; then
    visudo -cf /etc/sudoers.d/statix-agent >/dev/null
  fi
  cat >/etc/systemd/system/statix-agent-update.service <<EOF
[Unit]
Description=Update Statix Agent Nix generation
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
ExecStart=$PROFILE/bin/statix-agent update-worker
Environment=STATIX_AGENT_STATE_DIR=$STATE_DIR
Environment=STATIX_AGENT_PROFILE=$PROFILE
Environment=PATH=$PROFILE/bin:/nix/var/nix/profiles/default/bin:/usr/bin:/bin
User=root
Group=root
EOF
  systemctl daemon-reload
  systemctl enable --now statix-agent.service
}

install_nix
prepare_host
disable_legacy
install_profiles
install_command
install_units
"$PROFILE/bin/statix-agent-capabilities" --json > "$STATE_DIR/capabilities.json" 2>/dev/null || true
systemctl restart statix-agent.service
systemctl is-active --quiet statix-agent.service || fail "agent failed to start"
log "one-shot Nix installation complete"
