FROM rust:bookworm AS build

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo test --locked --bin statix-agent --no-run \
    && find target/debug/deps -maxdepth 1 -type f -name 'statix_agent-*' -perm -111 -exec cp {} /runner-tests \; \
    && test -x /runner-tests

FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
    cloud-image-utils dnsmasq iptables lxc lxc-templates openssh-client qemu-system-x86 qemu-utils sudo wget xz-utils \
    ca-certificates git systemd systemd-sysv \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --user-group --home-dir /nonexistent --no-create-home --shell /usr/sbin/nologin statix-agent \
    && mkdir -p /etc/statix /opt/statix \
    && systemctl disable lxc-net.service dnsmasq.service

COPY --from=build /runner-tests /opt/statix/runner-tests
COPY installers/ubuntu/24.04/statix-agent-lxc-helper /usr/local/libexec/statix-agent-lxc
COPY installers/ubuntu/24.04/statix-agent.service /etc/systemd/system/statix-agent.service
COPY scripts/run-runner-tests-service.sh /opt/statix/run-runner-tests-service.sh
RUN chmod 0755 /usr/local/libexec/statix-agent-lxc \
    && printf '%s\n' \
    'Defaults!/usr/local/libexec/statix-agent-lxc env_keep += "STATIX_AGENT_STATE_DIR STATE_DIRECTORY"' \
    'statix-agent ALL=(root) NOPASSWD: /usr/bin/systemctl start statix-agent-update.service' \
    'statix-agent ALL=(root) NOPASSWD: /usr/local/libexec/statix-agent-lxc *' \
    > /etc/sudoers.d/statix-agent \
    && chmod 0440 /etc/sudoers.d/statix-agent \
    && visudo -cf /etc/sudoers.d/statix-agent

ENV container=docker
STOPSIGNAL SIGRTMIN+3
CMD ["/bin/sh", "-c", "mount -o remount,rw /sys/fs/cgroup && exec /sbin/init"]
