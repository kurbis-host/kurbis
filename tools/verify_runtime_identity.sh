#!/bin/sh
# Verify the runtime image's fixed non-root identity and SSH-based builder probe.
# The probe intentionally uses ssh -G: it exercises OpenSSH's local identity
# lookup without needing production credentials or a network endpoint.
set -eu

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    echo "usage: $0 IMAGE" >&2
    exit 2
fi

image=$1
# Set KURBIS_RUNTIME_SSH_TARGET to exercise the real remote meminfo probe
# against a disposable rootless builder. Without it, ssh -G still proves
# OpenSSH can initialize as UID 65532 while keeping this check credential-free.
target=${KURBIS_RUNTIME_SSH_TARGET:-}
network_mode=none
set -- docker run --rm
if [ -n "$target" ]; then
    network_mode=${KURBIS_RUNTIME_SSH_NETWORK:-bridge}
    set -- "$@" --env "KURBIS_RUNTIME_SSH_TARGET=$target"
fi
set -- "$@" \
    --network "$network_mode" \
    --user 65532:65532 \
    --read-only \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=4m \
    --entrypoint /bin/sh \
    "$image" \
    -ec '
        set -eu
        test "$(id -u)" = 65532
        test "$(id -g)" = 65532
        test "$(id -un)" = kurbis
        test "$(id -gn)" = kurbis
        test "$(getent passwd 65532 | cut -d: -f1)" = kurbis
        test "$(getent passwd 65532 | cut -d: -f6)" = /home/kurbis
        test "$(getent group 65532 | cut -d: -f1)" = kurbis
        # OpenSSH resolves the invoking UID before constructing the request.
        # This fails with "No user exists for uid 65532" when /etc/passwd is
        # missing the runtime identity.
        if [ -n "${KURBIS_RUNTIME_SSH_TARGET:-}" ]; then
            ssh -o BatchMode=yes -o IdentitiesOnly=yes \
                -o StrictHostKeyChecking=yes -o ConnectTimeout=15 \
                -o ServerAliveInterval=30 -o ServerAliveCountMax=2 \
                -T -- "$KURBIS_RUNTIME_SSH_TARGET" cat /proc/meminfo
        else
            ssh -G -o BatchMode=yes -o ConnectTimeout=1 \
                -o UserKnownHostsFile=/dev/null \
                -o IdentityFile=/dev/null \
                ci-builder@builder.invalid >/dev/null
            cat /proc/meminfo
        fi | awk "/^(MemTotal|MemAvailable):/ { seen++ } END { exit seen == 2 ? 0 : 1 }"
    '
"$@"
