#!/usr/bin/env bash
set -Eeuo pipefail

# Nix is the sole release builder. The old flat-asset builder created mutable,
# independently verified artifacts and is intentionally no longer supported.
mode="${1:-all}"
out="${OUTPUT_ROOT:-dist}/nix"
mkdir -p "$out"

case "$mode" in
  all|binary)
    nix build --print-build-logs .#packages.x86_64-linux.statix-agent \
      --out-link "$out/statix-agent-x86_64-linux"
    nix build --print-build-logs .#packages.aarch64-linux.statix-agent \
      --out-link "$out/statix-agent-aarch64-linux"
    ;;
  shared)
    nix build --print-build-logs .#packages.x86_64-linux.statix-agent-capabilities \
      --out-link "$out/capabilities"
    ;;
  *)
    printf 'usage: %s [all|binary|shared]\n' "$0" >&2
    exit 2
    ;;
esac

printf '[build-release] Nix outputs staged under %s\n' "$out"
