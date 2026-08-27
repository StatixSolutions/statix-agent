FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl qemu-utils \
    && rm -rf /var/lib/apt/lists/*

COPY build-microvm-image.sh /usr/local/bin/build-microvm-image
RUN chmod 0755 /usr/local/bin/build-microvm-image

ENTRYPOINT ["/usr/local/bin/build-microvm-image"]
