#!/usr/bin/env bash
set -Eeuo pipefail

readonly DEFAULT_DOWNLOAD_BASE_URL="https://github.com/StatixSolutions/statix-agent/releases/latest/download"

DOWNLOAD_BASE_URL="${STATIX_DOWNLOAD_BASE_URL:-$DEFAULT_DOWNLOAD_BASE_URL}"

fail() {
  printf '[statix-debian-installer] error: %s\n' "$*" >&2
  exit 1
}

[[ "${EUID}" -eq 0 ]] || fail "run this installer as root"
[[ -r /etc/os-release ]] || fail "cannot detect OS release"
# shellcheck disable=SC1091
. /etc/os-release
[[ "${ID:-}" == debian ]] || fail "this installer targets Debian; detected ${PRETTY_NAME:-unknown Linux}"
[[ "${VERSION_ID:-}" == 12 || "${VERSION_ID:-}" == 13 ]] || \
  printf '[statix-debian-installer] warning: tested on Debian 12 and 13; detected %s\n' "${PRETTY_NAME:-unknown Debian}"

command -v apt-get >/dev/null 2>&1 || fail "apt-get is required"
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends curl ca-certificates

DOWNLOAD_BASE_URL="${DOWNLOAD_BASE_URL%/}"
temporary="$(mktemp)"
trap 'rm -f "$temporary"' EXIT

curl -fsSL --retry 3 --retry-delay 2 \
  "$DOWNLOAD_BASE_URL/statix-agent-install-ubuntu-24.04.sh" \
  -o "$temporary" || fail "failed to download the shared Debian-family installer"

export STATIX_DOWNLOAD_BASE_URL="$DOWNLOAD_BASE_URL"
export STATIX_UPDATE_SCRIPT_URL="${STATIX_UPDATE_SCRIPT_URL:-$DOWNLOAD_BASE_URL/statix-agent-update-debian.sh}"
export STATIX_SERVICE_URL="${STATIX_SERVICE_URL:-$DOWNLOAD_BASE_URL/statix-agent.service}"
export STATIX_UPDATE_SERVICE_URL="${STATIX_UPDATE_SERVICE_URL:-$DOWNLOAD_BASE_URL/statix-agent-update.service}"

exec bash "$temporary"
