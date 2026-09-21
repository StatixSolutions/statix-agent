#!/usr/bin/env bash
set -Eeuo pipefail

readonly service_name="statix-agent"
readonly drop_in_dir="${STATIX_AGENT_SERVICE_DROP_IN_DIR:-/etc/systemd/system/$service_name.service.d}"
readonly drop_in_path="$drop_in_dir/20-nginx-write-access.conf"

install -d -o root -g root -m 0755 "$drop_in_dir"
temporary="$(mktemp "$drop_in_dir/.nginx-write-access.XXXXXX")"
trap 'rm -f "$temporary"' EXIT

printf '%s\n' \
  '[Service]' \
  'ReadWritePaths=/run -/etc/nginx' >"$temporary"
chmod 0644 "$temporary"
mv -f "$temporary" "$drop_in_path"

systemctl daemon-reload
test -r "$drop_in_path"
