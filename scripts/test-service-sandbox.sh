#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
docker build -f docker/service-sandbox-tests.Dockerfile -t statix-agent-service-sandbox-tests .
container=$(docker run --detach --rm --privileged --cgroupns=private \
    --network=none --tmpfs /run --tmpfs /tmp --tmpfs /var/tmp \
    statix-agent-service-sandbox-tests)
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
docker exec "$container" python3 /opt/statix/test-service-sandbox.py
