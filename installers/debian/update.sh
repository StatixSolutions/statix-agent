#!/usr/bin/env bash
set -Eeuo pipefail

readonly DEFAULT_DOWNLOAD_BASE_URL="https://github.com/StatixSolutions/statix-agent/releases/latest/download"

DOWNLOAD_BASE_URL="${STATIX_DOWNLOAD_BASE_URL:-$DEFAULT_DOWNLOAD_BASE_URL}"
readonly UPDATE_SCRIPT_ASSET_NAME="statix-agent-update-ubuntu-24.04.sh"
[[ "${EUID}" -eq 0 ]] || { printf '[statix-debian-updater] error: updater must run as root\n' >&2; exit 1; }

if ! command -v curl >/dev/null 2>&1; then
  command -v apt-get >/dev/null 2>&1 || { printf '[statix-debian-updater] error: apt-get is required to bootstrap curl\n' >&2; exit 1; }
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends curl
fi

DOWNLOAD_BASE_URL="${DOWNLOAD_BASE_URL%/}"
updater_url="${STATIX_UPDATE_SCRIPT_URL:-$DOWNLOAD_BASE_URL/$UPDATE_SCRIPT_ASSET_NAME}"
temporary="$(mktemp)"
checksum="$(mktemp)"
trap 'rm -f "$temporary" "$checksum"' EXIT

curl -fsSL --retry 3 --retry-delay 2 \
  "$updater_url" \
  -o "$temporary" || { printf '[statix-debian-updater] error: failed to download shared updater\n' >&2; exit 1; }

curl -fsSL --retry 3 --retry-delay 2 \
  "$updater_url.sha256" \
  -o "$checksum" || { printf '[statix-debian-updater] error: failed to download shared updater checksum\n' >&2; exit 1; }

expected="$(awk '{print $1}' "$checksum")"
actual="$(sha256sum "$temporary" | awk '{print $1}')"
[[ "$expected" =~ ^[0-9a-fA-F]{64}$ && "${actual,,}" == "${expected,,}" ]] || {
  printf '[statix-debian-updater] error: shared updater checksum mismatch\n' >&2
  exit 1
}

export STATIX_DOWNLOAD_BASE_URL="$DOWNLOAD_BASE_URL"
exec bash "$temporary"
