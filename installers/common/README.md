# Installer support assets

`statix-agent-dependencies.sh` is published with each release and installed by
the distro-specific installer. The updater downloads the same asset before
replacing the agent binary, so dependency installation and validation have one
source of truth.
