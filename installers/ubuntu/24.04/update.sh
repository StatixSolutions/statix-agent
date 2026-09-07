#!/usr/bin/env bash
set -Eeuo pipefail

readonly SERVICE_NAME="statix-agent"
readonly DEFAULT_DOWNLOAD_BASE_URL="https://github.com/StatixSolutions/statix-agent/releases/latest/download"
readonly DEPENDENCIES_ASSET_NAME="statix-agent-dependencies.sh"
readonly MIGRATIONS_ARCHIVE_NAME="statix-agent-migrations.tar.gz"
readonly MIGRATIONS_MANIFEST_NAME="statix-agent-migrations.json"

DOWNLOAD_BASE_URL="${STATIX_DOWNLOAD_BASE_URL:-$DEFAULT_DOWNLOAD_BASE_URL}"
BINARY_PATH="${STATIX_BINARY_PATH:-/usr/local/bin/statix-agent}"
VERSION_FILE="${STATIX_VERSION_FILE:-/opt/statix/version.json}"
SERVICE_PATH="${STATIX_SERVICE_PATH:-/etc/systemd/system/$SERVICE_NAME.service}"
DEPENDENCIES_PATH="${STATIX_DEPENDENCIES_PATH:-/usr/local/lib/statix/statix-agent-dependencies.sh}"
LXC_HELPER_PATH="${STATIX_LXC_HELPER_PATH:-/usr/local/libexec/statix-agent-lxc}"
LXC_HELPER_URL="${STATIX_LXC_HELPER_URL:-$DOWNLOAD_BASE_URL/statix-agent-lxc-helper}"
STATE_ROOT="${STATIX_AGENT_STATE_DIR:-/var/lib/statix-agent}"
MIGRATIONS_STATE_DIR="$STATE_ROOT/migrations"
MIGRATIONS_STATE_PATH="$MIGRATIONS_STATE_DIR/state.json"

log() {
  printf '[statix-updater] %s\n' "$*"
}

fail() {
  printf '[statix-updater] error: %s\n' "$*" >&2
  exit 1
}

detect_arch() {
  case "$(uname -m)" in
    x86_64 | amd64)
      printf 'amd64'
      ;;
    aarch64 | arm64)
      printf 'arm64'
      ;;
    *)
      fail "unsupported CPU architecture: $(uname -m)"
      ;;
  esac
}

download_file() {
  local url="$1"
  local destination="$2"
  curl -fsSL --retry 3 --retry-delay 2 "$url" -o "$destination"
}

verify_sha256() {
  local binary_path="$1"
  local sha_url="$2"
  local sha_file
  sha_file="$(mktemp)"

  if ! download_file "$sha_url" "$sha_file"; then
    rm -f "$sha_file"
    fail "checksum not available at $sha_url"
  fi

  local expected
  expected="$(awk '{print $1}' "$sha_file")"
  rm -f "$sha_file"

  if [[ ! "$expected" =~ ^[0-9a-fA-F]{64}$ ]]; then
    fail "invalid checksum from $sha_url"
  fi

  local actual
  actual="$(sha256sum "$binary_path" | awk '{print $1}')"
  if [[ "${actual,,}" != "${expected,,}" ]]; then
    fail "checksum mismatch"
  fi
}

download_verified() {
  local url="$1"
  local destination="$2"
  download_file "$url" "$destination" || fail "failed to download $url"
  verify_sha256 "$destination" "$url.sha256"
}

acquire_update_lock() {
  install -d -m 0750 "$STATE_ROOT" "$MIGRATIONS_STATE_DIR"
  exec 9>"$STATE_ROOT/update.lock"
  flock -n 9 || fail "another Statix update is already running"
}

read_last_migration() {
  if [[ ! -f "$MIGRATIONS_STATE_PATH" ]]; then
    printf '0000\n'
    return
  fi

  local migration_id
  migration_id="$(sed -n 's/.*"lastMigration"[[:space:]]*:[[:space:]]*"\([0-9][0-9][0-9][0-9]-[^"/]*\)".*/\1/p' "$MIGRATIONS_STATE_PATH")"
  [[ -n "$migration_id" ]] || migration_id="0000"
  printf '%s\n' "$migration_id"
}

write_migration_state() {
  local migration_id="$1"
  local temporary
  temporary="$(mktemp "$MIGRATIONS_STATE_DIR/state.XXXXXX")"
  printf '{"schemaVersion":1,"lastMigration":"%s"}\n' "$migration_id" >"$temporary"
  chmod 0640 "$temporary"
  mv -f "$temporary" "$MIGRATIONS_STATE_PATH"
}

apply_migrations() {
  local archive manifest extraction_dir last_migration script migration_id
  archive="$(mktemp)"
  manifest="$(mktemp)"
  extraction_dir="$(mktemp -d)"
  trap 'rm -f "$archive" "$manifest"; rm -rf "$extraction_dir"' RETURN

  download_verified "$DOWNLOAD_BASE_URL/$MIGRATIONS_ARCHIVE_NAME" "$archive"
  download_verified "$DOWNLOAD_BASE_URL/$MIGRATIONS_MANIFEST_NAME" "$manifest"
  grep -Eq '"schemaVersion"[[:space:]]*:[[:space:]]*1' "$manifest" || fail "unsupported migration manifest"

  tar -tzf "$archive" | awk '
    /^\// || /(^|\/)\.\.($|\/)/ || /(^|\/)\.($|\/)/ { exit 1 }
    !/^[0-9][0-9][0-9][0-9]-[a-z0-9-]+\.sh$/ { exit 1 }
  ' || fail "invalid migration bundle"
  tar -xzf "$archive" -C "$extraction_dir"
  while IFS= read -r migration_id; do
    [[ "$migration_id" =~ ^[0-9]{4}-[a-z0-9-]+$ ]] || fail "invalid migration id in manifest"
    [[ -f "$extraction_dir/$migration_id.sh" ]] || fail "manifest migration $migration_id is missing from bundle"
  done < <(sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest")

  last_migration="$(read_last_migration)"
  for script in "$extraction_dir"/*.sh; do
    [[ -f "$script" ]] || continue
    migration_id="$(basename "$script" .sh)"
    [[ "$migration_id" > "$last_migration" ]] || continue
    grep -Fq "\"id\":\"$migration_id\"" "$manifest" || fail "migration $migration_id is missing from manifest"
    log "applying migration $migration_id"
    STATIX_AGENT_STATE_DIR="$STATE_ROOT" \
      STATIX_SERVICE_USER="${STATIX_SERVICE_USER:-statix-agent}" \
      STATIX_SERVICE_GROUP="${STATIX_SERVICE_GROUP:-statix-agent}" \
      bash "$script"
    write_migration_state "$migration_id"
    last_migration="$migration_id"
  done
}

repair_dependencies() {
  local dependencies_url temporary
  dependencies_url="${STATIX_DEPENDENCIES_URL:-$DOWNLOAD_BASE_URL/$DEPENDENCIES_ASSET_NAME}"
  temporary="$(mktemp)"
  log "downloading dependency helper from $dependencies_url"
  download_file "$dependencies_url" "$temporary" || fail "failed to download dependency helper"
  install -d -m 0755 "$(dirname "$DEPENDENCIES_PATH")"
  install -m 0755 "$temporary" "$DEPENDENCIES_PATH"
  rm -f "$temporary"
  "$DEPENDENCIES_PATH" --install
}

repair_lxc_helper() {
  local helper_url temporary
  helper_url="${STATIX_LXC_HELPER_URL:-$DOWNLOAD_BASE_URL/statix-agent-lxc-helper}"
  temporary="$(mktemp)"
  log "downloading LXC helper from $helper_url"
  download_file "$helper_url" "$temporary" || fail "failed to download LXC helper"
  install -d -m 0755 "$(dirname "$LXC_HELPER_PATH")"
  install -o root -g root -m 0755 "$temporary" "$LXC_HELPER_PATH"
  rm -f "$temporary"
}

bootstrap_curl() {
  if command -v curl >/dev/null 2>&1; then
    return
  fi

  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends curl
  elif command -v pacman >/dev/null 2>&1; then
    pacman -Sy --needed --noconfirm curl
  else
    fail "curl is missing and no supported package manager was found"
  fi
}

main() {
  [[ "${EUID}" -eq 0 ]] || fail "updater must run as root"
  command -v systemctl >/dev/null 2>&1 || fail "systemctl is required"
  command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"

  DOWNLOAD_BASE_URL="${DOWNLOAD_BASE_URL%/}"
  bootstrap_curl
  acquire_update_lock
  apply_migrations
  repair_dependencies
  repair_lxc_helper
  local arch binary_url temporary backup version_url version_tmp
  arch="$(detect_arch)"
  binary_url="${STATIX_AGENT_BINARY_URL:-$DOWNLOAD_BASE_URL/statix-agent-linux-$arch}"
  temporary="$(mktemp)"
  backup="$(mktemp)"

  log "downloading $binary_url"
  download_file "$binary_url" "$temporary"
  verify_sha256 "$temporary" "$binary_url.sha256"
  chmod 0755 "$temporary"

  if [[ -x "$BINARY_PATH" ]]; then
    cp "$BINARY_PATH" "$backup"
  else
    rm -f "$backup"
  fi

  systemctl stop "$SERVICE_NAME"
  install -m 0755 "$temporary" "$BINARY_PATH"

  version_url="${STATIX_VERSION_URL:-$DOWNLOAD_BASE_URL/version.json}"
  version_tmp="$(mktemp)"
  if download_file "$version_url" "$version_tmp"; then
    install -d -m 0755 "$(dirname "$VERSION_FILE")"
    install -m 0644 "$version_tmp" "$VERSION_FILE"
  fi
  rm -f "$version_tmp" "$temporary"

  systemctl daemon-reload
  systemctl start "$SERVICE_NAME"
  if ! systemctl is-active --quiet "$SERVICE_NAME"; then
    log "new agent did not start; restoring previous binary"
    if [[ -s "$backup" ]]; then
      install -m 0755 "$backup" "$BINARY_PATH"
      systemctl start "$SERVICE_NAME" || true
    fi
    rm -f "$backup"
    fail "updated agent failed to start"
  fi

  if [[ -f "$SERVICE_PATH" ]]; then
    systemctl enable "$SERVICE_NAME" >/dev/null
  fi

  rm -f "$backup"
  log "update complete"
}

main "$@"
