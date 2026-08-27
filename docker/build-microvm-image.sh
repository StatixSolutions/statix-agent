#!/bin/sh
set -eu

output=/out/microvm-test.qcow2
url=${STATIX_MICROVM_IMAGE_URL:?STATIX_MICROVM_IMAGE_URL is required}

mkdir -p /out
if [ ! -s "$output" ]; then
    tmp="$output.tmp"
    rm -f "$tmp"
    curl --fail --location --retry 3 --silent --show-error "$url" -o "$tmp"
    qemu-img check "$tmp"
    mv "$tmp" "$output"
fi

qemu-img info "$output" >/dev/null
echo "$output"
