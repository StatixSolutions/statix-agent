#!/usr/bin/env bash
set -Eeuo pipefail

has() { command -v "$1" >/dev/null 2>&1; }

capability() {
  local name="$1" status="$2" detail="$3"
  printf '%s\t%s\t%s\n' "$name" "$status" "$detail"
}

probe() {
  capability core available "systemd, nix and the agent update worker are installed"
  if has lxc-create && has lxc-start && has lxc-attach; then
    capability lxc available "LXC command set detected"
  else
    capability lxc missing "lxc-create, lxc-start and lxc-attach are required"
  fi
  if has docker && docker info >/dev/null 2>&1; then
    capability docker available "Docker daemon is reachable"
  elif has docker; then
    capability docker incompatible "Docker is installed but its daemon is unavailable"
  else
    capability docker missing "Docker is not installed"
  fi
  if [[ -e /dev/kvm ]] && has qemu-system-x86_64 && has qemu-img; then
    capability microvm available "KVM and QEMU are detected"
  elif [[ ! -e /dev/kvm ]]; then
    capability microvm missing "/dev/kvm is unavailable"
  else
    capability microvm incompatible "QEMU tools are incomplete"
  fi
  if has nginx && has iptables && has ip; then
    capability network available "nginx, iptables and iproute2 are detected"
  else
    capability network missing "nginx, iptables and iproute2 are required"
  fi
  if has wg; then
    capability wireguard available "wireguard-tools is detected"
  else
    capability wireguard disabled "wireguard-tools is not installed"
  fi
}

if [[ "${1:-table}" == "--json" ]]; then
  printf '{"capabilities":['
  first=1
  while IFS=$'\t' read -r name status detail; do
    (( first )) || printf ','
    first=0
    detail_json=${detail//\\/\\\\}
    detail_json=${detail_json//"/\\"}
    printf '{"name":"%s","status":"%s","detail":"%s"}' "$name" "$status" "$detail_json"
  done < <(probe)
  printf ']}\n'
else
  probe
fi
