#!/usr/bin/env bash
set -Eeuo pipefail

# Nix is the sole release builder. The old flat-asset builder created mutable,
# independently verified artifacts and is intentionally no longer supported.
mode="${1:-all}"
out="${OUTPUT_ROOT:-dist}/upload"
mkdir -p "$out"

case "$mode" in
  all|binary)
    system="${NIX_SYSTEM:-x86_64-linux}"
    arch="${ASSET_ARCH:-x86_64}"
    link="$out/.statix-agent-${arch}-linux"
    nix build --print-build-logs ".#packages.${system}.statix-agent-runtime" \
      --out-link "$link"
    # symlinkJoin exposes the runtime through links into /nix/store. Dereference
    # them while archiving so the published artifact does not contain links to
    # the builder's private store paths.
    tar -h -C "$(readlink -f "$link")" -czf "$out/statix-agent-${arch}-linux.tar.gz" .
    rm -f "$link"
    ;;
  shared)
    link="$out/.capabilities"
    nix build --print-build-logs .#packages.x86_64-linux.statix-agent-capabilities \
      --out-link "$link"
    tar -h -C "$(readlink -f "$link")" -czf "$out/statix-agent-capabilities.tar.gz" .
    rm -f "$link"
    ;;
  *)
    printf 'usage: %s [all|binary|shared]\n' "$0" >&2
    exit 2
    ;;
esac

printf '[build-release] Nix outputs staged under %s\n' "$out"
