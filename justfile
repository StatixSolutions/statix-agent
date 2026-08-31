test:
    cargo test --all-targets

prepare-microvm-test-image:
    if test -n "${STATIX_MICROVM_TEST_IMAGE:-}"; then test -f "$STATIX_MICROVM_TEST_IMAGE"; else mkdir -p "$PWD/.cache/statix-agent" && docker build -f docker/microvm-image-builder.Dockerfile -t statix-agent-microvm-image-builder docker && docker run --rm -e STATIX_MICROVM_IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-$(case "$(uname -m)" in x86_64) echo amd64;; aarch64) echo arm64;; *) echo amd64;; esac).img" -v "$PWD/.cache/statix-agent:/out" statix-agent-microvm-image-builder; fi

test-runners: prepare-microvm-test-image
    docker build -f docker/runner-tests.Dockerfile -t statix-agent-runner-tests .
    docker run --rm --privileged --device /dev/kvm -e STATIX_MICROVM_TEST_IMAGE=/fixtures/test.qcow2 -v "$PWD:/repo" -v "${STATIX_MICROVM_TEST_IMAGE:-$PWD/.cache/statix-agent/microvm-test.qcow2}:/fixtures/test.qcow2:ro" statix-agent-runner-tests sh -c 'set -e; ip link add lxcbr0 type bridge || true; ip addr add 10.0.3.1/24 dev lxcbr0 || true; ip link set lxcbr0 up; sysctl -w net.ipv4.ip_forward=1 || true; dnsmasq --interface=lxcbr0 --bind-interfaces --listen-address=10.0.3.1 --dhcp-range=10.0.3.2,10.0.3.254,12h --no-daemon >/tmp/dnsmasq.log 2>&1 & dnsmasq_pid=$!; iptables -t nat -A POSTROUTING -s 10.0.3.0/24 -j MASQUERADE; trap "kill $dnsmasq_pid 2>/dev/null || true" EXIT; cargo test --all-targets -- --ignored --test-threads=1'

test-runners-host: prepare-microvm-test-image
    STATIX_MICROVM_TEST_IMAGE="${STATIX_MICROVM_TEST_IMAGE:-$PWD/.cache/statix-agent/microvm-test.qcow2}" cargo test --all-targets -- --ignored --test-threads=1
