# statix-agent

Project layout:

This repo contains:
- the Rust `statix` agent binary
- Ubuntu installer assets under `installers/ubuntu/24.04`
- Arch Linux installer assets under `installers/archlinux`
- Debian installer assets under `installers/debian`
- the host-side systemd units and updater script

## Releases

The expected public release assets are:
- `statix-agent-linux-amd64`
- `statix-agent-linux-arm64`
- matching `.sha256` files
- `statix-agent-dependencies.sh` and its `.sha256` file
- `statix-agent-migrations.tar.gz` and its `.sha256` file
- `statix-agent-migrations.json` and its `.sha256` file
- distro-specific installer assets for supported distributions, for example:
- `statix-agent-install-ubuntu-24.04.sh`
- `statix-agent-update-ubuntu-24.04.sh`
- `statix-agent-install-archlinux.sh`
- `statix-agent-update-archlinux.sh`
- `statix-agent-install-debian.sh`
- `statix-agent-update-debian.sh`

Installer assets should be published under:

```bash
https://github.com/StatixSolutions/statix-agent/releases/latest/download
```

Public docs or bootstrap scripts in the `statix` repo should select the correct
installer asset for the target distribution instead of assuming a universal
Linux `install.sh` or `update.sh`.

Host migrations are cumulative release assets. New migration scripts go under
`migrations/` with a unique four-digit prefix and are never renamed or removed
after release. The updater records the last successful migration in
`/var/lib/statix-agent/migrations/state.json` and retries pending migrations on
the next update if one fails.

The first release containing the migration runner must be installed on existing
hosts with the platform installer once, because older updater scripts cannot
execute migrations they do not contain. Subsequent updates apply pending
migrations automatically.

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

## Pull request checks

The `Test` GitHub Actions workflow runs on pull requests targeting `main` and
can also be started manually. Independent `Unit tests` and `Runner integration
tests` jobs run on Ubuntu 24.04, with 15- and 45-minute timeouts respectively.
New commits cancel superseded runs for the same PR.

Run the same checks locally with `just test` (`cargo test --locked --all-targets`)
and `just test-runners`. CI uses stable Rust and the committed Cargo lockfile.
The integration job verifies Docker and usable `/dev/kvm` before running both
ignored tests serially in the privileged test container. Missing prerequisites
fail the check; the tests are not silently skipped. Ubuntu cloud images and the
GHCR test image must be anonymously accessible. GitHub-hosted nested
virtualization is not officially supported, so the workflow requires a successful
hosted integration run to validate runner compatibility.

These workflows report PR checks; requiring them before merging is configured
separately in repository branch rules.

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

## Logging

The agent writes human-readable structured logs to stderr, which systemd
captures in the journal. Logging defaults to `info` and can be filtered at
runtime with `RUST_LOG`, for example:

```sh
RUST_LOG=debug statix-agent run
journalctl -u statix-agent --no-pager -n 100
```

To manually run the installed updater, use:

```sh
statix-agent update
```

The command starts `statix-agent-update.service`, waits for the oneshot to
finish, and follows its journal output. It is available on Linux installations
that include the updater service (such as the Ubuntu and Arch installers).

Job stdout and stderr continue to be forwarded to the server as job logs.
Agent diagnostics are bounded and common secret arguments are redacted.
