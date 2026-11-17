# Installer support assets

The standalone bootstrap installs Nix and the on-demand update service. Agent
dependencies are delivered through the pinned flake and trusted Nix binary
cache. The orchestration plane sends an immutable update operation; the Rust
update worker realizes and health-checks that generation before activation and
rolls back on failure.

The older distro-specific shell assets are retained only for migration and are
not used by the new update path.
