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
https://github.com/StatixSolutions/statix-agent/releases/latest/download
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

`just test` runs the fast unit suite. LXC/Docker-in-LXC and MicroVM tests are available
through `just test-runners-host` or the Docker-backed `just test-runners`.

If `STATIX_MICROVM_TEST_IMAGE` is not set, the integration target builds and
caches a bootable Ubuntu 24.04 qcow2 fixture using Docker. Optionally set
`STATIX_MICROVM_TEST_IMAGE` to use a custom bootable qcow2 cloud image. Runner
Container `image` values are LXC distro/release references (for example
`ubuntu:24.04`); the guest is provisioned with Docker Engine and Docker Compose.
MicroVMs use `STATIX_MICROVM_BASE_IMAGE` for their bootable qcow2 base
(default `ubuntu-24.04`) and run Docker Compose in the guest as `statix`.
Both runtimes place the project files in `/home/statix/docker`.
The Docker recipe mounts `/dev/kvm` and runs with the privileges required for
nested virtualization. Missing images, privileges, or runtime
dependencies fail the integration target rather than being skipped.
