#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
fixture=$(realpath "${STATIX_MICROVM_TEST_IMAGE:-$PWD/.cache/statix-agent/microvm-test.qcow2}")
test -s "$fixture"
docker build -f docker/runner-tests.Dockerfile -t statix-agent-runner-tests .
container=$(docker run --detach --rm --privileged --device /dev/kvm \
    --cgroupns=private --tmpfs /run --tmpfs /tmp --tmpfs /var/tmp \
    --mount "type=bind,src=$fixture,dst=/fixtures/test.qcow2,readonly" \
    statix-agent-runner-tests)
trap 'docker rm --force "$container" >/dev/null' EXIT

ready=false
for attempt in $(seq 1 60); do
    if docker exec "$container" systemctl list-units --no-pager >/dev/null 2>&1; then
        ready=true
        break
    fi
    sleep 1
done
if [ "$ready" != true ]; then
    docker logs "$container"
    echo 'Test infrastructure failure: systemd did not start' >&2
    exit 1
fi
docker exec "$container" bash /opt/statix/run-runner-tests-service.sh
