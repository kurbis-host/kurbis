#!/usr/bin/env bash
set -euo pipefail

# Run the same real Docker/kernel checks used during worker admission. This is
# executable evidence, not an environment-variable assertion or a fake runtime.
if [[ "$(uname -s)" != Linux ]]; then
  echo "rootless sandbox verification requires Linux" >&2
  exit 2
fi

image=${1:?usage: $0 IMAGE@sha256:DIGEST}
case "$image" in
  *@sha256:*) image_name=${image%@sha256:*}; digest=${image##*@sha256:} ;;
  *) image_name=; digest= ;;
esac
if [[ -z "$image_name" || ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
  echo "runtime image must be pinned by a sha256 digest" >&2
  exit 2
fi

# Validate and pin one context for every Docker CLI request. DOCKER_HOST can
# override the endpoint independently of `docker context inspect`, so reject it
# instead of risking probes against a different (possibly rootful/remote) daemon.
if [[ -n "${DOCKER_HOST:-}" ]]; then
  echo "DOCKER_HOST override rejected; select a local rootless Docker context" >&2
  exit 1
fi
context=${DOCKER_CONTEXT:-$(docker context show)}
unset DOCKER_HOST DOCKER_CONTEXT
docker_cli=(docker --context "$context")

security_options=$("${docker_cli[@]}" info --format '{{json .SecurityOptions}}')
case "$security_options" in
  *'name=rootless'*) ;;
  *) echo "Docker daemon is not rootless" >&2; exit 1;;
esac
[[ "$("${docker_cli[@]}" info --format '{{.CgroupVersion}}')" == 2 ]] || {
  echo "Docker daemon is not using cgroup v2" >&2; exit 1;
}
for controller in cpu memory pids; do
  grep -qw "$controller" /sys/fs/cgroup/cgroup.controllers || {
    echo "missing cgroup controller: $controller" >&2
    exit 1
  }
done

socket=$("${docker_cli[@]}" context inspect "$context" --format '{{(index .Endpoints "docker").Host}}')
[[ "$socket" == unix://* ]] || { echo "remote Docker endpoint rejected" >&2; exit 1; }
socket=${socket#unix://}
[[ -S "$socket" ]] || { echo "Docker endpoint is not a Unix socket" >&2; exit 1; }
[[ "$(stat -c '%u' "$socket")" != 0 ]] || { echo "root-owned Docker socket rejected" >&2; exit 1; }
uid=$(stat -c '%u' "$socket")
gid=$(id -g)

memory=67108864
cpu_millis=250
pids=16
disk=67108864

"${docker_cli[@]}" run --rm --pull=never --network=none \
  --memory "$memory" --memory-swap "$memory" \
  --cpu-period 100000 --cpu-quota 25000 --pids-limit "$pids" \
  --user 0:0 --read-only \
  --security-opt no-new-privileges:true --cap-drop ALL \
  --tmpfs "/work:rw,nosuid,nodev,size=${disk},nr_inodes=65536" \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=4m \
  --env "EXPECTED_HOST_UID=$uid" --env "EXPECTED_HOST_GID=$gid" \
  --env "EXPECTED_MEMORY_BYTES=$memory" --env "EXPECTED_CPU_MILLIS=$cpu_millis" \
  --env "EXPECTED_PID_LIMIT=$pids" --env "EXPECTED_DISK_BYTES=$disk" \
  --entrypoint /bin/sh "$image" -ec '
set -eu
[ "$(id -u)" -eq 0 ]
awk -v expected="$EXPECTED_HOST_UID" '\''$1 == 0 && $2 == expected && $3 == 1 { root = 1 } $1 == 1 && $2 > 0 && $3 >= 65536 { range = 1 } END { exit !(root && range) }'\'' /proc/self/uid_map
awk -v expected="$EXPECTED_HOST_GID" '\''$1 == 0 && $2 == expected && $3 == 1 { root = 1 } $1 == 1 && $2 > 0 && $3 >= 65536 { range = 1 } END { exit !(root && range) }'\'' /proc/self/gid_map
[ "$(cat /sys/fs/cgroup/memory.max)" -le "$EXPECTED_MEMORY_BYTES" ]
[ "$(cat /sys/fs/cgroup/memory.swap.max)" -eq 0 ]
read -r cpu_quota cpu_period < /sys/fs/cgroup/cpu.max
[ "$cpu_quota" -gt 0 ] && [ $((cpu_quota * 1000)) -le $((cpu_period * EXPECTED_CPU_MILLIS)) ]
[ "$(cat /sys/fs/cgroup/pids.max)" -le "$EXPECTED_PID_LIMIT" ]
grep -Eq "^NoNewPrivs:[[:space:]]+1$" /proc/self/status
grep -Eq "^CapEff:[[:space:]]+0+$" /proc/self/status
awk '\''$5 == "/" { if ("," $6 "," ~ /,ro,/) found = 1 } END { exit !found }'\'' /proc/self/mountinfo
awk '\''NR > 1 && $1 != "lo" { route = 1 } END { exit route }'\'' /proc/net/route
awk '\''$10 != "lo" { route = 1 } END { exit route }'\'' /proc/net/ipv6_route
'

"${docker_cli[@]}" run --rm --pull=never --network=none \
  --memory 268435456 --memory-swap 268435456 \
  --cpu-period 100000 --cpu-quota 25000 --pids-limit 16 --user 0:0 \
  --read-only --tmpfs "/work:rw,nosuid,nodev,size=${disk},nr_inodes=65536" \
  --security-opt no-new-privileges:true --cap-drop ALL \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=4m \
  --env "EXPECTED_DISK_BYTES=$disk" --entrypoint /bin/sh "$image" -ec '
set -eu
if dd if=/dev/zero of=/work/kurbis-quota-probe bs=1M count=96 conv=fsync status=none 2>/dev/null; then
  exit 90
fi
written=$(stat -c %s /work/kurbis-quota-probe)
[ "$written" -le "$EXPECTED_DISK_BYTES" ]
[ "$written" -ge 62914560 ]
'

echo "rootless Docker cgroup, namespace, read-only-root, bounded-workspace and no-egress probes passed"
