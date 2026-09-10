#!/usr/bin/env bash
set -Eeuo pipefail

trap 'rc=$?; printf "[updater-test] failure at line %s (exit %s): %s\n" "$LINENO" "$rc" "$BASH_COMMAND" >&2; exit "$rc"' ERR

if [[ "${EUID}" -ne 0 ]]; then
  printf 'updater tests must run as root (use sudo)\n' >&2
  exit 1
fi

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

asset_root="$tmp_root/assets"
fake_bin="$tmp_root/bin"
state_root="$tmp_root/state"
install_root="$tmp_root/install"
mkdir -p "$asset_root" "$fake_bin" "$state_root" "$install_root"

cat >"$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
destination=
url=
while (($#)); do
  case "$1" in
    -o)
      destination="$2"
      shift 2
      ;;
    -*)
      shift
      ;;
    *)
      url="$1"
      shift
      ;;
  esac
done
[[ -n "$destination" && -n "$url" ]] || exit 2
asset="${url##*/}"
cp "$TEST_ASSET_ROOT/$asset" "$destination"
EOF
chmod 0755 "$fake_bin/curl"

cat >"$fake_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
case "${1:-}" in
  stop|daemon-reload|enable)
    printf '%s\n' "$*" >>"$TEST_SYSTEMCTL_LOG"
    ;;
  start)
    printf '%s\n' "$*" >>"$TEST_SYSTEMCTL_LOG"
    if [[ -f "$TEST_FAIL_START" ]]; then
      printf '[fake-systemctl] injected start failure\n' >&2
      exit 1
    fi
    ;;
  is-active)
    if [[ -f "$TEST_FAIL_ACTIVE" ]]; then
      printf '[fake-systemctl] injected active-state failure\n' >&2
      exit 1
    fi
    ;;
  *)
    printf 'unexpected systemctl call: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF
chmod 0755 "$fake_bin/systemctl"

cat >"$asset_root/statix-agent-dependencies.sh" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
[[ "${1:-}" == --install ]] || exit 2
printf 'installed\n' >"$TEST_DEPENDENCY_INSTALLED"
EOF
chmod 0755 "$asset_root/statix-agent-dependencies.sh"

printf 'new-agent\n' >"$asset_root/statix-agent-linux-amd64"
printf 'new-helper\n' >"$asset_root/statix-agent-lxc-helper"
printf 'new-updater\n' >"$asset_root/statix-agent-update-ubuntu-24.04.sh"
printf '{"version":"test-new"}\n' >"$asset_root/version.json"
sha256sum "$asset_root/statix-agent-linux-amd64" >"$asset_root/statix-agent-linux-amd64.sha256"
sha256sum "$asset_root/statix-agent-lxc-helper" >"$asset_root/statix-agent-lxc-helper.sha256"
sha256sum "$asset_root/statix-agent-update-ubuntu-24.04.sh" >"$asset_root/statix-agent-update-ubuntu-24.04.sh.sha256"
sha256sum "$asset_root/statix-agent-dependencies.sh" >"$asset_root/statix-agent-dependencies.sh.sha256"

make_migration_assets() {
  local mode="${1:-success}"
  local migration_dir="$tmp_root/migrations"
  rm -rf "$migration_dir"
  mkdir -p "$migration_dir"

  if [[ "$mode" == success ]]; then
    cat >"$migration_dir/0001-test-state.sh" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
marker="$STATIX_AGENT_STATE_DIR/migration-runs"
count=0
[[ -f "$marker" ]] && count="$(cat "$marker")"
printf '%s\n' "$((count + 1))" >"$marker"
EOF
  else
    cat >"$migration_dir/0001-test-state.sh" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
exit 42
EOF
  fi
  chmod 0755 "$migration_dir/0001-test-state.sh"
  tar -czf "$asset_root/statix-agent-migrations.tar.gz" -C "$migration_dir" 0001-test-state.sh
  sha256sum "$asset_root/statix-agent-migrations.tar.gz" >"$asset_root/statix-agent-migrations.tar.gz.sha256"
  printf '{\n  "schemaVersion": 1,\n  "migrations": [\n    {"id":"0001-test-state"}\n  ]\n}\n' >"$asset_root/statix-agent-migrations.json"
  sha256sum "$asset_root/statix-agent-migrations.json" >"$asset_root/statix-agent-migrations.json.sha256"
}

run_update() {
  PATH="$fake_bin:$PATH" \
    TEST_ASSET_ROOT="$asset_root" \
    TEST_SYSTEMCTL_LOG="$tmp_root/systemctl.log" \
    TEST_FAIL_START="$tmp_root/fail-start" \
    TEST_FAIL_ACTIVE="$tmp_root/fail-active" \
    TEST_DEPENDENCY_INSTALLED="$tmp_root/dependency-installed" \
    STATIX_DOWNLOAD_BASE_URL="https://test.invalid/assets" \
    STATIX_AGENT_STATE_DIR="$state_root" \
    STATIX_BINARY_PATH="$install_root/statix-agent" \
    STATIX_VERSION_FILE="$install_root/version.json" \
    STATIX_SERVICE_PATH="$install_root/statix-agent.service" \
    STATIX_DEPENDENCIES_PATH="$install_root/dependencies.sh" \
    STATIX_LXC_HELPER_PATH="$install_root/statix-agent-lxc" \
    STATIX_UPDATE_SCRIPT_PATH="$install_root/update.sh" \
    bash "$repo_root/installers/ubuntu/24.04/update.sh"
}

assert_file_contains() {
  local path="$1"
  local expected="$2"
  grep -Fqx "$expected" "$path" || {
    printf 'expected %s to contain exactly %s\n' "$path" "$expected" >&2
    exit 1
  }
}

check() {
  local name="$1"
  shift
  printf '[updater-test] %s\n' "$name"
  if ! "$@"; then
    printf '[updater-test] FAILED: %s\n' "$name" >&2
    printf '[updater-test] command:' >&2
    printf ' %q' "$@" >&2
    printf '\n[updater-test] install tree:\n' >&2
    find "$install_root" -maxdepth 2 -printf '%M %u:%g %p\n' >&2 2>/dev/null || true
    printf '[updater-test] state tree:\n' >&2
    find "$state_root" -maxdepth 3 -printf '%M %u:%g %p\n' >&2 2>/dev/null || true
    printf '[updater-test] systemctl calls:\n' >&2
    cat "$tmp_root/systemctl.log" >&2 2>/dev/null || true
    return 1
  fi
}

expect_failure() {
  local name="$1"
  shift
  printf '[updater-test] %s\n' "$name"
  if "$@"; then
    printf '[updater-test] FAILED: command unexpectedly succeeded: %s\n' "$name" >&2
    return 1
  fi
  printf '[updater-test] expected failure observed: %s\n' "$name"
}

assert_regular_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    printf 'expected regular file: %s\n' "$path" >&2
    ls -la "$(dirname "$path")" >&2
    return 1
  fi
}

make_migration_assets success
printf 'old-agent\n' >"$install_root/statix-agent"
printf '{"schemaVersion":1,"lastMigration":"0000"}\n' >"$state_root/migrations-state-before"

check 'successful update' run_update
check 'new binary installed' assert_file_contains "$install_root/statix-agent" 'new-agent'
check 'migration ran once' assert_file_contains "$state_root/migration-runs" '1'
check 'migration state recorded' grep -Fq '0001-test-state' "$state_root/migrations/state.json"
check 'dependency helper ran' test -s "$tmp_root/dependency-installed"
check 'LXC helper installed' assert_regular_file "$install_root/statix-agent-lxc"
check 'updater installed' assert_file_contains "$install_root/update.sh" 'new-updater'

printf 'old-agent\n' >"$install_root/statix-agent"
printf '%064d\n' 0 >"$asset_root/statix-agent-update-ubuntu-24.04.sh.sha256"
expect_failure 'invalid updater checksum is rejected' run_update
check 'binary unchanged after updater failure' assert_file_contains "$install_root/statix-agent" 'old-agent'
sha256sum "$asset_root/statix-agent-update-ubuntu-24.04.sh" >"$asset_root/statix-agent-update-ubuntu-24.04.sh.sha256"

check 'idempotent update' run_update
check 'migration was not repeated' assert_file_contains "$state_root/migration-runs" '1'

printf 'old-agent\n' >"$install_root/statix-agent"
rm -f "$state_root/migrations/state.json" "$tmp_root/fail-start"
make_migration_assets failure
expect_failure 'failed migration is rejected' run_update
check 'old binary retained after migration failure' assert_file_contains "$install_root/statix-agent" 'old-agent'
check 'migration state not advanced' test ! -e "$state_root/migrations/state.json"

make_migration_assets success
rm -f "$state_root/migrations/state.json"
touch "$tmp_root/fail-active"
expect_failure 'failed active-state check is rejected' run_update
check 'old binary restored after service failure' assert_file_contains "$install_root/statix-agent" 'old-agent'

rm -f "$tmp_root/fail-active"
touch "$tmp_root/fail-start"
expect_failure 'failed service start is rejected' run_update
check 'old binary restored after start failure' assert_file_contains "$install_root/statix-agent" 'old-agent'

printf 'updater tests passed\n'
