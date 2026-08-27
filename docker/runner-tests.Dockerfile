FROM rust:1.88-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    cloud-image-utils lxc lxc-templates openssh-client qemu-system-x86 qemu-utils \
    ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /repo
CMD ["cargo", "test", "--all-targets", "--", "--ignored"]
