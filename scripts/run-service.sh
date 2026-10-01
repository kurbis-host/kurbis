#!/usr/bin/env bash
set -Eeuo pipefail

if (( $# < 4 )); then
  echo "usage: run-service.sh ENV_FILE ROOT MODE SERVICE" >&2
  exit 2
fi

env_file=$1
root=$2
mode=$3
service=$4

set -a
# This is the private, operator-maintained shell-compatible .env created from
# the checked-in template by initialize_deploy_env.py.
source "$env_file"
set +a
cd "$root"

case "$mode" in
  native-debug|native-release)
    profile=${mode#native-}
    binary_dir="$root/target/$profile"
    export DATABASE_URL=$DATABASE_URL_NATIVE
    export PUMPKIN_BINARY="$root/output/native-pumpkin/target/$profile/pumpkin"
    export WORKER_RUNTIME=native
    export WORKER_STORAGE_MODE=directory
    export WAITING_CONTROL_SOCKET="/tmp/kurbis-waiting-${WORKER_ID%%-*}.sock"
    ;;
  docker)
    binary_dir="$root/target/release"
    export DATABASE_URL="postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@127.0.0.1:${DATABASE_PORT}/${POSTGRES_DB}"
    export PUMPKIN_IMAGE=melon-pumpkin:host-control
    export WORKER_RUNTIME=docker
    ;;
  *)
    echo "unknown deployment mode: $mode" >&2
    exit 2
    ;;
esac

case "$service" in
  control-plane)
    if [[ $mode == native-debug || $mode == native-release ]]; then
      export CONTROL_PLANE_LISTEN=127.0.0.1:8080
    fi
    exec "$binary_dir/control-plane"
    ;;
  gateway)
    if [[ $mode == docker ]]; then
      echo "The Docker gateway is managed by Compose; use its dashboard log pane." >&2
      exit 2
    fi
    export CONTROL_PLANE_URL=http://127.0.0.1:8080
    export JAVA_LISTEN=0.0.0.0:25565
    export BEDROCK_LISTEN=0.0.0.0:19132
    exec "$binary_dir/gateway"
    ;;
  worker)
    exec "$binary_dir/worker"
    ;;
  waiting-pumpkin)
    if [[ $mode == docker ]]; then
      echo "The Docker waiting runtime is managed by Compose; use its dashboard log pane." >&2
      exit 2
    fi
    exec "$root/tools/run_native_waiting.sh"
    ;;
  panel)
    export CONTROL_PLANE_INTERNAL_URL=http://127.0.0.1:8080
    cd "$root/web"
    if [[ $mode == native-debug ]]; then
      exec npm run dev -- --hostname 0.0.0.0
    fi
    exec npm run start -- --hostname 0.0.0.0
    ;;
  postgres|control-plane-log|waiting-pumpkin-log|gateway-log|panel-log)
    compose_service=${service%-log}
    exec docker compose --env-file "$env_file" -f "$root/compose.yaml" logs --follow --tail=100 "$compose_service"
    ;;
  *)
    echo "unknown service: $service" >&2
    exit 2
    ;;
esac
