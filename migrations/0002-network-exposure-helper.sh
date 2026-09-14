#!/usr/bin/env bash
set -Eeuo pipefail

readonly service_name="statix-agent"
readonly service_user="${STATIX_SERVICE_USER:-statix-agent}"
readonly state_root="${STATIX_AGENT_STATE_DIR:-/var/lib/statix-agent}"
readonly download_base_url="${STATIX_DOWNLOAD_BASE_URL:-https://github.com/StatixSolutions/statix-agent/releases/latest/download}"
readonly lxc_helper_path="${STATIX_LXC_HELPER_PATH:-/usr/local/libexec/statix-agent-lxc}"
readonly network_helper_path="${STATIX_NETWORK_HELPER_PATH:-/usr/local/libexec/statix-agent-network}"
readonly network_helper_url="${STATIX_NETWORK_HELPER_URL:-${download_base_url%/}/statix-agent-network-helper}"
readonly sudoers_path="${STATIX_AGENT_SUDOERS_PATH:-/etc/sudoers.d/$service_name}"

helper_temporary="$(mktemp)"
checksum_temporary="$(mktemp)"
sudoers_temporary="$(mktemp)"
trap 'rm -f "$helper_temporary" "$checksum_temporary" "$sudoers_temporary"' EXIT

curl -fsSL --retry 3 --retry-delay 2 "$network_helper_url" -o "$helper_temporary"
curl -fsSL --retry 3 --retry-delay 2 "$network_helper_url.sha256" -o "$checksum_temporary"
expected="$(awk '{print $1; exit}' "$checksum_temporary")"
actual="$(sha256sum "$helper_temporary" | awk '{print $1}')"
[[ "$expected" =~ ^[0-9a-fA-F]{64}$ && "${actual,,}" == "${expected,,}" ]]

install -d -m 0755 "$(dirname "$network_helper_path")"
install -o root -g root -m 0755 "$helper_temporary" "$network_helper_path"

cat >"$sudoers_temporary" <<EOF
Defaults!$lxc_helper_path env_keep += "STATIX_AGENT_STATE_DIR STATE_DIRECTORY STATIX_LXC_NETWORK_BRIDGE STATIX_LXC_NETWORK_GATEWAY"
Defaults!$network_helper_path env_keep += "STATIX_AGENT_STATE_DIR STATE_DIRECTORY"
$service_user ALL=(root) NOPASSWD: /usr/bin/systemctl start $service_name-update.service
$service_user ALL=(root) NOPASSWD: $lxc_helper_path *
$service_user ALL=(root) NOPASSWD: $network_helper_path apply $state_root/network/nginx-exposures.conf
EOF
if command -v visudo >/dev/null 2>&1; then
  visudo -cf "$sudoers_temporary" >/dev/null
fi
install -d -m 0755 "$(dirname "$sudoers_path")"
install -m 0440 "$sudoers_temporary" "$sudoers_path"

test -x "$network_helper_path"
test -r "$sudoers_path"
