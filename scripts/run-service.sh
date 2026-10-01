#!/usr/bin/env bash
set -Eeuo pipefail

if (( $# < 4 )); then
  echo "usage: run-service.sh ENV_FILE ROOT MODE SERVICE [WORKER_BUNDLE]" >&2
  exit 2
fi

env_file=$1
root=$2
mode=$3
service=$4

if [[ $service == worker ]]; then
  if (( $# != 5 )); then
    echo "worker launch requires an explicit per-worker credential bundle" >&2
    exit 2
  fi
  worker_bundle=$5
  case "$mode" in
    native-debug|native-release)
      profile=${mode#native-}
      binary_dir="$root/target/$profile"
      ;;
    docker)
      binary_dir="$root/target/release"
      ;;
    *)
      echo "unknown deployment mode: $mode" >&2
      exit 2
      ;;
  esac
  [[ -f $worker_bundle ]] || {
    echo "worker credential bundle missing: $worker_bundle; run ./start.sh worker-bundle" >&2
    exit 2
  }
  cd "$root"
  source "$worker_bundle"
  worker_env=(env -i "PATH=${PATH:-/usr/local/bin:/usr/bin:/bin}" "HOME=${HOME:-/}")
  for variable_name in \
    LANG LC_ALL TMPDIR SSL_CERT_FILE SSL_CERT_DIR RUST_LOG DOCKER_HOST DOCKER_CONFIG \
    DOCKER_TLS_VERIFY DOCKER_CERT_PATH WORKER_ID WORKER_NAME WORKER_ADDRESS \
    WORKER_SIGNAL_ADDRESS WORKER_DATABASE_URL WORKER_API_TOKEN \
    WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP WORKER_ROOT WORKER_STORAGE_MODE \
    WORKER_RUNTIME WORKER_MEMORY_GIB WORKER_DISK_GIB WORKER_CPU_MILLIS \
    WORKER_PORT_MIN WORKER_PORT_MAX PUMPKIN_HOST_CONTROL_TOKEN CONTROL_PLANE_URL \
    PUBLIC_BASE_URL PUMPKIN_BINARY PUMPKIN_IMAGE KURBIS_CONTROL_PLUGIN_WASM \
    SNAPSHOT_INTERVAL_SECONDS IDLE_TIMEOUT_SECONDS; do
    if declare -p "$variable_name" >/dev/null 2>&1; then
      worker_env+=("$variable_name=${!variable_name}")
    fi
  done
  exec "${worker_env[@]}" "$binary_dir/worker"
fi

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
    export CONTROL_PLANE_URL=http://127.0.0.1:8080
    export KURBIS_CONTROL_PLUGIN_WASM="$root/plugins/kurbis-control/target/wasm32-wasip2/$profile/kurbis_control.wasm"
    ;;
  docker)
    binary_dir="$root/target/release"
    export DATABASE_URL="postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@127.0.0.1:${DATABASE_PORT}/${POSTGRES_DB}"
    export PUMPKIN_IMAGE=melon-pumpkin:host-control
    export WORKER_RUNTIME=docker
    export CONTROL_PLANE_URL="http://127.0.0.1:${CONTROL_PLANE_PORT:-18080}"
    unset KURBIS_CONTROL_PLUGIN_WASM
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
      exec pnpm run dev -- --hostname 0.0.0.0
    fi
    exec pnpm run start -- --hostname 0.0.0.0
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
