# statix-agent

Project layout:

This repo contains the Rust `statix-agent`, its Nix flake, an on-demand update
worker, and capability-aware host integration for systemd Linux systems.

## Releases and updates

Releases are immutable Nix flake revisions published to a signed binary cache.
Nodes install the exact revision into a Nix profile when commanded by the
orchestration plane; they never poll, run a timer, consume a mutable `latest`
URL, or execute a release-provided dependency script.

The installed target is `statix-agent-runtime`, a Nix closure containing the
agent and its host tools: LXC, QEMU/KVM utilities, networking, nginx, SSH,
cloud-image tooling, WireGuard, Docker tooling, and supporting utilities.

Install a systemd Linux host with:

```bash
curl -fsSL https://raw.githubusercontent.com/StatixSolutions/statix-agent/main/installers/install.sh \
  | sudo STATIX_FLAKE_URL=github:StatixSolutions/statix-agent bash
```

The installer installs the Nix daemon, creates the agent profile, and installs
an on-demand update service. An orchestration `update_agent` operation supplies
an exact release revision. The agent realizes a new generation before
activation, restarts only the agent control plane, checks health and
capabilities, and rolls back the profile on failure.

Inspect or operate updates with:

```bash
sudo systemctl start statix-agent-update.service
```

The old per-distribution installers and updater remain only as migration
artifacts during rollout; they are not part of the new release path.

NixOS hosts should manage `services.statix-agent.package` through their normal
system flake and `nixos-rebuild`; the on-demand update service is for hosts
installed through the standalone bootstrap.

Host migrations are cumulative release assets. New migration scripts go under
`migrations/` with a unique four-digit prefix and are never renamed or removed
after release. The updater records the last successful migration in
`/var/lib/statix-agent/migrations/state.json` and retries pending migrations on
the next update if one fails.

The first release containing the migration runner must be installed on existing
hosts with the platform installer once, because older updater scripts cannot
execute migrations they do not contain. Subsequent updates apply pending
migrations automatically.

Agent upgrades are control-plane updates. The service units use
`KillMode=process` so long-lived deployed LXC/Docker runtimes and project
MicroVM processes are not stopped when the agent is restarted. Runtime state is
kept under the agent state directory so a new agent instance can reconnect to
existing workloads. Release validation must prove that deployed application
traffic remains healthy throughout an update.

## Local build

```bash
cargo build --release
```

For local Nix evaluation and packaging, use:

```bash
nix flake check
nix build .#statix-agent
nix build .#statix-agent-capabilities
```

## Pull request checks

The `Test` GitHub Actions workflow runs on pull requests targeting `main` and
can also be started manually. Independent `Unit tests` and `Runner integration
tests` jobs run on Ubuntu 24.04, with 15- and 45-minute timeouts respectively.
New commits cancel superseded runs for the same PR.

Run the same checks locally with `just test` (`cargo test --locked --all-targets`),
`sudo just test-updater`, and `just test-runners`. CI uses stable Rust and the
committed Cargo lockfile.
The integration job verifies Docker and usable `/dev/kvm`, then runs both ignored
runner tests serially as `statix-agent` under the shipped Ubuntu systemd service
policy in a privileged test container. Missing prerequisites
fail the check; the tests are not silently skipped. Ubuntu cloud images and the
GHCR test image must be anonymously accessible. GitHub-hosted nested
virtualization is not officially supported, so the workflow requires a successful
hosted integration run to validate runner compatibility.

These workflows report PR checks; requiring them before merging is configured
separately in repository branch rules.

## Runner integration tests

`just test` runs the fast unit suite. LXC/Docker-in-LXC and MicroVM tests are available
through `just test-runners-host` or the Docker-backed `just test-runners`.

`just test-runners` builds the Rust test executable before starting a disposable
Ubuntu 24.04 container running systemd. The shipped Ubuntu service unit runs the
real LXC and MicroVM workflows as `statix-agent`, with its filesystem and privilege
restrictions intact. Only the test command, test environment, restart behavior,
and timeout are overridden. The production LXC helper and narrow sudoers rule
are installed; the agent automatically prepares the configured LXC bridge,
forwarding, DNS listener, and NAT rules before running LXC jobs.

Test state and workspaces live beneath the service's `StateDirectory`. The VM
fixture is mounted read-only, and no host repository directory is mounted.
Permission-denied and read-only-filesystem errors during normal runner operations
fail the tests, with runner diagnostics and the service journal printed. Both
tests run even if one fails. Existing service restrictions and runtime bugs are
not relaxed or treated as expected successes. This checks the exercised workflows,
not every possible filesystem access or the Arch service policy.

`just test-runners-host` runs the same Rust tests directly as the invoking user
for diagnostics; it does not exercise the systemd sandbox.

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

Job stdout and stderr are retained in the agent-owned spool described below;
they are not forwarded to or archived by the server. Agent diagnostics are
bounded and common secret arguments are redacted.

Execution output is retained by the agent in `$STATIX_AGENT_STATE_DIR/logs` as
structured JSONL, including while the agent reconnects. Statix reads bounded
pages from that spool through the authenticated agent connection; it does not
archive new log lines in the control-plane database. Files rotate at 100 MiB
and are kept for seven days. Runtime and deployment commands include their
resource ID so the infrastructure view can request the relevant retained
output.
