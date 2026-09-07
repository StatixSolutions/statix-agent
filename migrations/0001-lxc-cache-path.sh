#!/usr/bin/env bash
set -Eeuo pipefail

readonly state_root="${STATIX_AGENT_STATE_DIR:-/var/lib/statix-agent}"
readonly service_user="${STATIX_SERVICE_USER:-statix-agent}"
readonly service_group="${STATIX_SERVICE_GROUP:-statix-agent}"
readonly lxc_home="$state_root/lxc"

install -d -m 0711 "$state_root" "$lxc_home"
install -d -m 0750 "$lxc_home/.cache/lxc" "$lxc_home/.config" "$lxc_home/.local/share"

if getent passwd "$service_user" >/dev/null 2>&1 && getent group "$service_group" >/dev/null 2>&1; then
  chown "$service_user:$service_group" "$state_root" "$lxc_home" "$lxc_home/.cache" "$lxc_home/.cache/lxc" "$lxc_home/.config" "$lxc_home/.local" "$lxc_home/.local/share"
fi

test -d "$lxc_home/.cache/lxc"
