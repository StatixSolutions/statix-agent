#!/usr/bin/env bash
set -Eeuo pipefail

readonly DEFAULT_DOWNLOAD_BASE_URL="https://github.com/StatixSolutions/statix-agent/releases/latest/download"

DOWNLOAD_BASE_URL="${STATIX_DOWNLOAD_BASE_URL:-$DEFAULT_DOWNLOAD_BASE_URL}"
[[ "${EUID}" -eq 0 ]] || { printf '[statix-debian-updater] error: updater must run as root\n' >&2; exit 1; }

if ! command -v curl >/dev/null 2>&1; then
  command -v apt-get >/dev/null 2>&1 || { printf '[statix-debian-updater] error: apt-get is required to bootstrap curl\n' >&2; exit 1; }
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends curl
fi

DOWNLOAD_BASE_URL="${DOWNLOAD_BASE_URL%/}"
temporary="$(mktemp)"
trap 'rm -f "$temporary"' EXIT

curl -fsSL --retry 3 --retry-delay 2 \
  "$DOWNLOAD_BASE_URL/statix-agent-update-ubuntu-24.04.sh" \
  -o "$temporary" || { printf '[statix-debian-updater] error: failed to download shared updater\n' >&2; exit 1; }

export STATIX_DOWNLOAD_BASE_URL="$DOWNLOAD_BASE_URL"
exec bash "$temporary"
