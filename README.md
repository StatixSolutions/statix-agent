# statix-agent

Project layout:

This repo contains:
- the Rust `statix` agent binary
- Ubuntu installer assets under `installers/ubuntu/24.04`
- Arch Linux installer assets under `installers/archlinux`
- the host-side systemd units and updater script

## Releases

The expected public release assets are:
- `statix-agent-linux-amd64`
- `statix-agent-linux-arm64`
- matching `.sha256` files
- distro-specific installer assets for supported distributions, for example:
- `statix-agent-install-ubuntu-24.04.sh`
- `statix-agent-update-ubuntu-24.04.sh`
- `statix-agent-install-archlinux.sh`
- `statix-agent-update-archlinux.sh`

Installer assets should be published under:

```bash
https://github.com/statixab/statix-agent/releases/latest/download
```

Public docs or bootstrap scripts in the `statix` repo should select the correct
installer asset for the target distribution instead of assuming a universal
Linux `install.sh` or `update.sh`.

## Local build

```bash
cargo build --release
```

For release packaging, use:

```bash
bash scripts/build-release.sh all
```

This stages structured output under `dist/release/` and writes the flat GitHub
release asset set under `dist/upload/`.

## Runner integration tests

`just test` runs the fast unit suite. Real LXC and MicroVM tests are available
through `just test-runners-host` or the Docker-backed `just test-runners`.

If `STATIX_MICROVM_TEST_IMAGE` is not set, the integration target builds and
caches a bootable Ubuntu 24.04 qcow2 fixture using Docker. Optionally set
`STATIX_MICROVM_TEST_IMAGE` to use a custom bootable qcow2 cloud image. Also set
`STATIX_CONTAINER_TEST_IMAGE` to choose the LXC distribution and release (the
default is `ubuntu:24.04`). The Docker recipe mounts `/dev/kvm` and runs with
the privileges required for nested LXC. Missing images, privileges, or runtime
dependencies fail the integration target rather than being skipped.
