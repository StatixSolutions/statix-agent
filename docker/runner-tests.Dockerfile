FROM rust:1.88-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    cloud-image-utils dnsmasq iptables lxc lxc-templates openssh-client qemu-system-x86 qemu-utils sudo wget xz-utils \
    ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /repo
COPY installers/ubuntu/24.04/statix-agent-lxc-helper /usr/local/libexec/statix-agent-lxc
RUN chmod 0755 /usr/local/libexec/statix-agent-lxc
CMD ["cargo", "test", "--all-targets", "--", "--ignored"]
