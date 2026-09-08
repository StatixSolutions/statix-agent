"""Exercise shipped service policies in a disposable systemd container.

The desired policy denies access to unrelated data, including reads. A read-only
mount is insufficient. Never run the fixture setup on a real installation.
"""

import errno
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import sys


CONFIG = Path("/opt/statix/service-sandbox-cases.json")
DENIED = {errno.EACCES, errno.EPERM, errno.EROFS}
SENTINEL = "sandbox sentinel\n"


def fixture(path):
    path = Path(path)
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(0o777)
    (path / "sentinel").write_text(SENTINEL)
    (path / "sentinel").chmod(0o666)
    return str(path)


def probe():
    failures = []
    if os.getuid() != pwd.getpwnam("statix-agent").pw_uid:
        raise RuntimeError("probe must run as the service user")

    def check(label, operation, allowed):
        try:
            operation()
        except OSError as error:
            if allowed or error.errno not in DENIED:
                failures.append(f"{label}: unexpected {error}")
            else:
                print(f"PASS denied {label}", flush=True)
        else:
            if not allowed:
                failures.append(f"{label}: access outside permitted folders succeeded")
            else:
                print(f"PASS allowed {label}", flush=True)

    cases = json.loads(CONFIG.read_text())
    for directory, readable, writable in cases:
        path = Path(directory)
        check(f"list {path}", lambda: list(path.iterdir()), readable)
        check(f"read {path}", lambda: (path / "sentinel").read_text(), readable)
        check(f"create {path}", lambda: (path / "created").write_text("probe"), writable)
        check(f"overwrite {path}", lambda: (path / "sentinel").write_text(SENTINEL), writable)
        check(f"mkdir {path}", lambda: (path / "created-dir").mkdir(), writable)

    for directory in ("/tmp", "/var/tmp"):
        path = Path(directory)
        if (path / "host-sandbox-sentinel").exists():
            failures.append(f"{path}: host temporary file visible despite PrivateTmp")
        check(f"private temporary write {path}",
              lambda: (path / "service-sandbox-probe").write_text("probe"), True)

    for failure in failures:
        print(f"FAIL {failure}", flush=True)
    print(f"SERVICE_SANDBOX_PROBE_COMPLETE failures={len(failures)}", flush=True)
    return bool(failures)


def run():
    # Fixture setup is intentionally restricted to our disposable test image.
    if Path("/proc/1/comm").read_text().strip() != "systemd" or not Path("/fixtures/ubuntu.service").exists():
        raise RuntimeError("run through scripts/test-service-sandbox.sh")
    failed = False
    for distro in ("ubuntu", "archlinux"):
        cases = []
        for parent in ("/etc", "/opt", "/usr/local/share", "/var/lib", "/home", "/srv", "/run", "/dev/shm"):
            cases.append((fixture(f"{parent}/sandbox-outside-{distro}"), False, False))
        for parent in ("/etc/statix", "/opt/statix"):
            cases.append((fixture(f"{parent}/sandbox-{distro}"), True, False))
        for parent in ("/var/lib/statix-agent", "/run/statix-agent"):
            Path(parent).mkdir(exist_ok=True)
            shutil.chown(parent, user="statix-agent", group="statix-agent")
            cases.append((fixture(f"{parent}/sandbox-{distro}"), True, True))
        # Create both optional multiarch paths so neither allowance is skipped.
        for parent in ("/run/lxc", "/usr/lib/x86_64-linux-gnu/lxc/rootfs", "/usr/lib/aarch64-linux-gnu/lxc/rootfs"):
            cases.append((fixture(f"{parent}/sandbox-{distro}"), distro == "ubuntu", distro == "ubuntu"))
        escape = Path(f"/var/lib/statix-agent/sandbox-escape-{distro}")
        escape.symlink_to(f"/srv/sandbox-outside-{distro}", target_is_directory=True)
        cases.append((str(escape), False, False))
        for directory in ("/tmp", "/var/tmp"):
            Path(directory, "host-sandbox-sentinel").write_text(SENTINEL)
        CONFIG.write_text(json.dumps(cases))

        unit = f"sandbox-{distro}.service"
        shutil.copyfile(f"/fixtures/{distro}.service", f"/etc/systemd/system/{unit}")
        dropin = Path(f"/etc/systemd/system/{unit}.d")
        dropin.mkdir()
        # Keep every sandbox/identity directive from the shipped unit intact.
        (dropin / "probe.conf").write_text(
            "[Service]\nType=oneshot\nRestart=no\nExecStart=\n"
            "ExecStart=/usr/bin/python3 /opt/statix/test-service-sandbox.py --probe\n"
            "TimeoutStartSec=30s\n"
        )
        subprocess.run(["systemctl", "daemon-reload"], check=True)
        result = subprocess.run(["systemctl", "start", unit], check=False, timeout=45)
        subprocess.run(["journalctl", "--sync"], check=True)
        journal = subprocess.run(
            ["journalctl", "--unit", unit, "--no-pager", "--output=cat"],
            check=True, capture_output=True, text=True,
        ).stdout
        print(f"=== {distro} service policy ===\n{journal}", flush=True)
        if "SERVICE_SANDBOX_PROBE_COMPLETE" not in journal:
            print(f"FAIL {distro}: probe did not complete (test infrastructure failure)", flush=True)
            failed = True
        if result.returncode != 0:
            failed = True
    return failed


if __name__ == "__main__":
    sys.exit(probe() if sys.argv[1:] == ["--probe"] else run())
