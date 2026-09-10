#!/usr/bin/env bash
set -euo pipefail

# Stop distro-managed LXC networking so the agent exercises its own setup.
systemctl stop lxc-net.service dnsmasq.service
install -d -m 0755 /run/lxc

# Use the shipped unit unchanged, with only test command/lifecycle overrides.
mkdir -p /etc/systemd/system/statix-agent.service.d
cat > /etc/systemd/system/statix-agent.service.d/tests.conf <<'EOF'
[Service]
Type=oneshot
Restart=no
ExecStart=
ExecStart=/opt/statix/runner-tests jobs::runners::integration_tests:: --ignored --test-threads=1 --nocapture
Environment=STATIX_MICROVM_TEST_IMAGE=/fixtures/test.qcow2
Environment=STATIX_RUNNER_TEST_SYSTEMD=1
TimeoutStartSec=15min
EOF
systemctl daemon-reload
systemctl cat statix-agent.service
systemctl show statix-agent.service --property=User,Group,ProtectSystem,ProtectHome,ReadWritePaths,ReadOnlyPaths,PrivateTmp,StateDirectory

status=0
systemctl start statix-agent.service || status=$?
journalctl --sync
journalctl --unit=statix-agent.service --no-pager --output=cat > /tmp/runner-tests-journal.log
cat /tmp/runner-tests-journal.log
if ! grep -Fx 'running 2 tests' /tmp/runner-tests-journal.log >/dev/null \
    || ! grep '^test result:' /tmp/runner-tests-journal.log >/dev/null; then
    echo 'Test infrastructure failure: Rust test harness did not finish' >&2
    exit 1
fi
exit "$status"
