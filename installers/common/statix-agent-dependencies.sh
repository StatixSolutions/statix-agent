#!/usr/bin/env bash
set -Eeuo pipefail

log() {
  printf '[statix-dependencies] %s\n' "$*"
}

fail() {
  printf '[statix-dependencies] error: %s\n' "$*" >&2
  exit 1
}

require_root() {
  [[ "${EUID}" -eq 0 ]] || fail "dependency maintenance must run as root"
}

ubuntu_packages() {
  printf '%s\n' \
    ca-certificates curl cloud-image-utils iproute2 lxc lxc-templates \
    xz-utils pciutils qemu-system-arm qemu-system-x86 qemu-utils \
    openssh-client uidmap wget sudo
}

arch_packages() {
  printf '%s\n' \
    ca-certificates curl iproute2 lxc xz pciutils sudo \
    wget qemu-desktop openssh cloud-init
}

detect_distro() {
  [[ -r /etc/os-release ]] || fail "cannot detect OS release"
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}" in
    ubuntu) printf 'ubuntu' ;;
    arch) printf 'archlinux' ;;
    *) fail "unsupported Linux distribution: ${ID:-unknown}" ;;
  esac
}

install_packages() {
  local distro="$1"
  local packages=()
  case "$distro" in
    ubuntu)
      command -v apt-get >/dev/null 2>&1 || fail "apt-get is required"
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      mapfile -t packages < <(ubuntu_packages)
      apt-get install -y --no-install-recommends "${packages[@]}"
      ;;
    archlinux)
      command -v pacman >/dev/null 2>&1 || fail "pacman is required"
      mapfile -t packages < <(arch_packages)
      pacman -Sy --needed --noconfirm "${packages[@]}"
      ;;
  esac
}

required_commands() {
  printf '%s\n' \
    curl sha256sum systemctl ip tar sudo \
    lxc-create lxc-start lxc-wait lxc-attach lxc-stop lxc-destroy \
    qemu-img qemu-system-x86_64 qemu-system-aarch64 cloud-localds \
    ssh-keygen ssh scp
}

check_commands() {
  local missing=() command_name
  while IFS= read -r command_name; do
    command -v "$command_name" >/dev/null 2>&1 || missing+=("$command_name")
  done < <(required_commands)

  if (( ${#missing[@]} > 0 )); then
    fail "missing required commands: ${missing[*]}"
  fi
}

main() {
  require_root
  local mode="${1:---install}"
  local distro
  distro="$(detect_distro)"

  case "$mode" in
    --install)
      log "installing host dependencies for $distro"
      install_packages "$distro"
      check_commands
      ;;
    --check)
      check_commands
      ;;
    *)
      fail "usage: $0 [--install|--check]"
      ;;
  esac
}

main "$@"
