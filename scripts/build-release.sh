#!/usr/bin/env bash
set -Eeuo pipefail

MODE="${1:-all}"
OUTPUT_ROOT="${OUTPUT_ROOT:-dist}"
RELEASE_ROOT="${OUTPUT_ROOT}/release"
UPLOAD_ROOT="${OUTPUT_ROOT}/upload"
LINUX_ROOT="${RELEASE_ROOT}/linux"
UBUNTU_INSTALLER_ROOT="${RELEASE_ROOT}/installers/ubuntu/24.04"
ARCH_INSTALLER_ROOT="${RELEASE_ROOT}/installers/archlinux"
DEBIAN_INSTALLER_ROOT="${RELEASE_ROOT}/installers/debian"
METADATA_ROOT="${RELEASE_ROOT}/metadata"
MIGRATIONS_ROOT="${RELEASE_ROOT}/migrations"
BINARY_NAME="statix-agent"

log() {
  printf '[build-release] %s\n' "$*"
}

fail() {
  printf '[build-release] error: %s\n' "$*" >&2
  exit 1
}

detect_asset_arch() {
  local candidate="${ASSET_ARCH:-}"

  if [[ -n "$candidate" ]]; then
    printf '%s\n' "$candidate"
    return
  fi

  candidate="${RUST_TARGET:-}"
  case "$candidate" in
    x86_64-unknown-linux-gnu)
      printf 'amd64\n'
      ;;
    aarch64-unknown-linux-gnu)
      printf 'arm64\n'
      ;;
    "")
      case "$(uname -m)" in
        x86_64 | amd64)
          printf 'amd64\n'
          ;;
        aarch64 | arm64)
          printf 'arm64\n'
          ;;
        *)
          fail "unsupported host architecture: $(uname -m)"
          ;;
      esac
      ;;
    *)
      fail "unsupported Rust target for asset arch detection: ${candidate}"
      ;;
  esac
}

detect_rust_target() {
  if [[ -n "${RUST_TARGET:-}" ]]; then
    printf '%s\n' "$RUST_TARGET"
    return
  fi

  cargo -vV | awk '/^host:/ { print $2; exit }'
}

binary_path_for_target() {
  local rust_target="$1"

  if [[ -n "${RUST_TARGET:-}" ]]; then
    printf 'target/%s/release/%s\n' "$rust_target" "$BINARY_NAME"
    return
  fi

  printf 'target/release/%s\n' "$BINARY_NAME"
}

prepare_dirs() {
  mkdir -p "$LINUX_ROOT" "$UBUNTU_INSTALLER_ROOT" "$ARCH_INSTALLER_ROOT" "$DEBIAN_INSTALLER_ROOT" "$METADATA_ROOT" "$MIGRATIONS_ROOT" "$UPLOAD_ROOT"
}

build_binary_assets() {
  local asset_arch rust_target binary_dir binary_path flat_binary flat_sha
  asset_arch="$(detect_asset_arch)"
  rust_target="$(detect_rust_target)"
  binary_dir="${LINUX_ROOT}/${asset_arch}"
  binary_path="$(binary_path_for_target "$rust_target")"
  flat_binary="${UPLOAD_ROOT}/statix-agent-linux-${asset_arch}"
  flat_sha="${flat_binary}.sha256"

  mkdir -p "$binary_dir"

  if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    log "building ${rust_target}"
    if [[ -n "${RUST_TARGET:-}" ]]; then
      cargo build --locked --release --target "$rust_target"
    else
      cargo build --locked --release
    fi
  fi

  [[ -x "$binary_path" ]] || fail "missing built binary: ${binary_path}"

  install -m 0755 "$binary_path" "${binary_dir}/statix-agent-linux-${asset_arch}"
  sha256sum "${binary_dir}/statix-agent-linux-${asset_arch}" > "${binary_dir}/statix-agent-linux-${asset_arch}.sha256"

  install -m 0755 "${binary_dir}/statix-agent-linux-${asset_arch}" "$flat_binary"
  install -m 0644 "${binary_dir}/statix-agent-linux-${asset_arch}.sha256" "$flat_sha"
}

build_migration_assets() {
  local archive manifest file migration_id first
  local migration_files=()

  while IFS= read -r file; do
    migration_files+=("$(basename "$file")")
  done < <(find migrations -maxdepth 1 -type f -name '*.sh' -print | sort)

  ((${#migration_files[@]} > 0)) || fail "no migration scripts found"
  for migration_id in "${migration_files[@]}"; do
    [[ "$migration_id" =~ ^[0-9]{4}-[a-z0-9-]+\.sh$ ]] || fail "invalid migration filename: $migration_id"
  done

  archive="${MIGRATIONS_ROOT}/statix-agent-migrations.tar.gz"
  manifest="${MIGRATIONS_ROOT}/statix-agent-migrations.json"
  tar -czf "$archive" -C migrations "${migration_files[@]}"
  sha256sum "$archive" > "${archive}.sha256"

  {
    printf '{\n  "schemaVersion": 1,\n  "migrations": ['
    first=1
    for migration_id in "${migration_files[@]}"; do
      if ((first)); then first=0; else printf ', '; fi
      printf '\n    {"id":"%s"}' "${migration_id%.sh}"
    done
    printf '\n  ]\n}\n'
  } >"$manifest"
  sha256sum "$manifest" > "${manifest}.sha256"
}

build_shared_assets() {
  local built_at git_tag git_sha

  install -m 0755 installers/ubuntu/24.04/install.sh "${UBUNTU_INSTALLER_ROOT}/statix-agent-install-ubuntu-24.04.sh"
  install -m 0755 installers/ubuntu/24.04/update.sh "${UBUNTU_INSTALLER_ROOT}/statix-agent-update-ubuntu-24.04.sh"
  install -m 0644 installers/ubuntu/24.04/statix-agent.service "${UBUNTU_INSTALLER_ROOT}/statix-agent.service"
  install -m 0644 installers/ubuntu/24.04/statix-agent-update.service "${UBUNTU_INSTALLER_ROOT}/statix-agent-update.service"
  install -m 0755 installers/ubuntu/24.04/statix-agent-lxc-helper "${UBUNTU_INSTALLER_ROOT}/statix-agent-lxc-helper"
  install -m 0755 installers/common/statix-agent-dependencies.sh "${UBUNTU_INSTALLER_ROOT}/statix-agent-dependencies.sh"

  install -m 0755 installers/archlinux/install.sh "${ARCH_INSTALLER_ROOT}/statix-agent-install-archlinux.sh"
  install -m 0755 installers/archlinux/update.sh "${ARCH_INSTALLER_ROOT}/statix-agent-update-archlinux.sh"
  install -m 0644 installers/archlinux/statix-agent.service "${ARCH_INSTALLER_ROOT}/statix-agent.service"
  install -m 0644 installers/archlinux/statix-agent-update.service "${ARCH_INSTALLER_ROOT}/statix-agent-update.service"
  install -m 0755 installers/archlinux/statix-agent-lxc-helper "${ARCH_INSTALLER_ROOT}/statix-agent-lxc-helper"
  install -m 0755 installers/common/statix-agent-dependencies.sh "${ARCH_INSTALLER_ROOT}/statix-agent-dependencies.sh"

  install -m 0755 installers/debian/install.sh "${DEBIAN_INSTALLER_ROOT}/statix-agent-install-debian.sh"
  install -m 0755 installers/debian/update.sh "${DEBIAN_INSTALLER_ROOT}/statix-agent-update-debian.sh"
  install -m 0644 installers/ubuntu/24.04/statix-agent.service "${DEBIAN_INSTALLER_ROOT}/statix-agent.service"
  install -m 0644 installers/ubuntu/24.04/statix-agent-update.service "${DEBIAN_INSTALLER_ROOT}/statix-agent-update.service"
  install -m 0755 installers/ubuntu/24.04/statix-agent-lxc-helper "${DEBIAN_INSTALLER_ROOT}/statix-agent-lxc-helper"
  install -m 0755 installers/common/statix-agent-dependencies.sh "${DEBIAN_INSTALLER_ROOT}/statix-agent-dependencies.sh"

  build_migration_assets

  built_at="${BUILT_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"
  git_tag="${GIT_TAG:-$(git describe --tags --exact-match 2>/dev/null || printf 'dev')}"
  git_sha="${GIT_SHA:-$(git rev-parse HEAD)}"

  cat > "${METADATA_ROOT}/version.json" <<EOF
{
  "version": "${git_tag}",
  "commit": "${git_sha}",
  "builtAt": "${built_at}"
}
EOF

  install -m 0755 "${UBUNTU_INSTALLER_ROOT}/statix-agent-install-ubuntu-24.04.sh" "${UPLOAD_ROOT}/statix-agent-install-ubuntu-24.04.sh"
  install -m 0755 "${UBUNTU_INSTALLER_ROOT}/statix-agent-update-ubuntu-24.04.sh" "${UPLOAD_ROOT}/statix-agent-update-ubuntu-24.04.sh"
  sha256sum "${UPLOAD_ROOT}/statix-agent-update-ubuntu-24.04.sh" > "${UPLOAD_ROOT}/statix-agent-update-ubuntu-24.04.sh.sha256"
  install -m 0644 "${UBUNTU_INSTALLER_ROOT}/statix-agent.service" "${UPLOAD_ROOT}/statix-agent.service"
  install -m 0644 "${UBUNTU_INSTALLER_ROOT}/statix-agent-update.service" "${UPLOAD_ROOT}/statix-agent-update.service"

  install -m 0755 "${ARCH_INSTALLER_ROOT}/statix-agent-install-archlinux.sh" "${UPLOAD_ROOT}/statix-agent-install-archlinux.sh"
  install -m 0755 "${ARCH_INSTALLER_ROOT}/statix-agent-update-archlinux.sh" "${UPLOAD_ROOT}/statix-agent-update-archlinux.sh"
  sha256sum "${UPLOAD_ROOT}/statix-agent-update-archlinux.sh" > "${UPLOAD_ROOT}/statix-agent-update-archlinux.sh.sha256"
  install -m 0755 "${DEBIAN_INSTALLER_ROOT}/statix-agent-install-debian.sh" "${UPLOAD_ROOT}/statix-agent-install-debian.sh"
  install -m 0755 "${DEBIAN_INSTALLER_ROOT}/statix-agent-update-debian.sh" "${UPLOAD_ROOT}/statix-agent-update-debian.sh"
  sha256sum "${UPLOAD_ROOT}/statix-agent-update-debian.sh" > "${UPLOAD_ROOT}/statix-agent-update-debian.sh.sha256"
  install -m 0644 "${METADATA_ROOT}/version.json" "${UPLOAD_ROOT}/version.json"
  install -m 0755 "${UBUNTU_INSTALLER_ROOT}/statix-agent-lxc-helper" "${UPLOAD_ROOT}/statix-agent-lxc-helper"
  sha256sum "${UPLOAD_ROOT}/statix-agent-lxc-helper" > "${UPLOAD_ROOT}/statix-agent-lxc-helper.sha256"
  install -m 0755 "${UBUNTU_INSTALLER_ROOT}/statix-agent-dependencies.sh" "${UPLOAD_ROOT}/statix-agent-dependencies.sh"
  sha256sum "${UPLOAD_ROOT}/statix-agent-dependencies.sh" > "${UPLOAD_ROOT}/statix-agent-dependencies.sh.sha256"
  install -m 0644 "$MIGRATIONS_ROOT/statix-agent-migrations.tar.gz" "$UPLOAD_ROOT/statix-agent-migrations.tar.gz"
  install -m 0644 "$MIGRATIONS_ROOT/statix-agent-migrations.tar.gz.sha256" "$UPLOAD_ROOT/statix-agent-migrations.tar.gz.sha256"
  install -m 0644 "$MIGRATIONS_ROOT/statix-agent-migrations.json" "$UPLOAD_ROOT/statix-agent-migrations.json"
  install -m 0644 "$MIGRATIONS_ROOT/statix-agent-migrations.json.sha256" "$UPLOAD_ROOT/statix-agent-migrations.json.sha256"
}

prepare_dirs

case "$MODE" in
  all)
    build_binary_assets
    build_shared_assets
    ;;
  binary)
    build_binary_assets
    ;;
  shared)
    build_shared_assets
    ;;
  *)
    fail "unknown mode: ${MODE}"
    ;;
esac
