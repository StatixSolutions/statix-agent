FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends systemd systemd-sysv python3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --user-group statix-agent

COPY installers/ubuntu/24.04/statix-agent.service /fixtures/ubuntu.service
COPY installers/archlinux/statix-agent.service /fixtures/archlinux.service
COPY scripts/test-service-sandbox.py /opt/statix/test-service-sandbox.py

ENV container=docker
STOPSIGNAL SIGRTMIN+3
CMD ["/bin/sh", "-c", "mount -o remount,rw /sys/fs/cgroup && exec /sbin/init"]
