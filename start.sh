#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "$0")" && pwd)
ENV_FILE="$ROOT/.env"
SESSION=kurbis-hoster
MODE_FILE="$ROOT/data/deployment-mode"

usage() {
  cat <<'EOF'
Usage:
  ./start.sh native [debug|release]  Build and run the host processes in tmux.
  ./start.sh docker                  Build and run the Docker deployment.
  ./start.sh attach                  Reattach to the process dashboard.
  ./start.sh stop                    Stop the dashboard and Compose services.
  ./start.sh setup                   Create .env with fresh secrets.
  ./start.sh worker-bundle [id] [--rotate]
                                      Provision a scoped control-plane credential.

Native mode defaults to release. On first use, setup creates .env; edit its
URLs, worker address, and OAuth credentials before starting either mode.
EOF
}

die() {
  printf 'start.sh: %s\n' "$*" >&2
  exit 1
}

setup_env() {
  if [[ ! -f $ENV_FILE ]]; then
    python3 "$ROOT/scripts/initialize_deploy_env.py" "$ROOT/.env.example" "$ENV_FILE"
  fi
}

load_env() {
  set -a
  source "$ENV_FILE"
  set +a
}

check_deployment_values() {
  local name value deployment=${1:-} profile=${2:-} local_origin
  for name in PUBLIC_BASE_URL ROOT_DOMAIN WORKER_ADDRESS \
    GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET \
    DISCORD_OAUTH_CLIENT_ID DISCORD_OAUTH_CLIENT_SECRET; do
    value=${!name:-}
    [[ -n $value ]] || die "$name is empty in .env"
    [[ $value != *replace-me* && $value != *REPLACE-ME* ]] || die "set $name in .env before starting"
  done
  case "$PUBLIC_BASE_URL" in
    https://*) ;;
    http://*)
      [[ $deployment == native && $profile == debug ]] \
        || die "PUBLIC_BASE_URL must use https:// outside native debug mode"
      local_origin=${PUBLIC_BASE_URL#http://}
      local_origin=${local_origin%%/*}
      local_origin=${local_origin%%\?*}
      local_origin=${local_origin%%\#*}
      case "$local_origin" in
        localhost|localhost:*|127.0.0.1|127.0.0.1:*|'[::1]'|'[::1]':*) ;;
        *) die "native debug allows http:// only for a loopback PUBLIC_BASE_URL" ;;
      esac
      ;;
    *) die "PUBLIC_BASE_URL must use https://" ;;
  esac
  [[ $PUBLIC_BASE_URL != *example.com* ]] || die "replace the example PUBLIC_BASE_URL"
  [[ $ROOT_DOMAIN != *example.com* ]] || die "replace the example ROOT_DOMAIN"
  [[ $WORKER_ADDRESS != 203.0.113.10 ]] || die "set WORKER_ADDRESS to this worker's reachable address"
  for name in INTERNAL_TOKEN PUMPKIN_GATEWAY_CONTROL_TOKEN WORKER_ID WORKER_SIGNAL_ADDRESS; do
    value=${!name:-}
    [[ -n $value && $value != GENERATE_ON_FIRST_START ]] || die "$name was not initialized; run ./start.sh setup"
  done
}

worker_bundle_path() {
  printf '%s/data/worker-bundles/%s.env' "$ROOT" "$1"
}

provision_worker_bundle() {
  local mode=$1 worker_id=${2:-$WORKER_ID} rotate=${3:-false} profile=${4:-release}
  local database_url bundle_path control_plane_url runtime storage_mode pumpkin_binary
  local pumpkin_image plugin_wasm proxy_upstream proxy_egress_network proxy_image
  local variable_name
  case "$mode" in
    native) database_url=$DATABASE_URL_NATIVE ;;
    docker)
      database_url="postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@127.0.0.1:${DATABASE_PORT}/${POSTGRES_DB}"
      ;;
    *) die "unsupported worker provisioning mode: $mode" ;;
  esac
  case "$mode" in
    native)
      control_plane_url=${WORKER_CONTROL_PLANE_URL:-http://127.0.0.1:8080}
      runtime=native
      storage_mode=${WORKER_STORAGE_MODE:-directory}
      pumpkin_binary=${PUMPKIN_BINARY:-"$ROOT/output/native-pumpkin/target/$profile/pumpkin"}
      pumpkin_image=
      plugin_wasm=${KURBIS_CONTROL_PLUGIN_WASM:-"$ROOT/plugins/kurbis-control/target/wasm32-wasip2/$profile/kurbis_control.wasm"}
      ;;
    docker)
      control_plane_url=${WORKER_CONTROL_PLANE_URL:-"http://127.0.0.1:${CONTROL_PLANE_PORT:-18080}"}
      runtime=docker
      storage_mode=${WORKER_STORAGE_MODE:-directory}
      pumpkin_binary=
      pumpkin_image=${PUMPKIN_IMAGE:-melon-pumpkin:host-control}
      plugin_wasm=
      ;;
    *) die "unsupported worker bundle mode: $mode" ;;
  esac
  proxy_upstream=${WORKER_RUNTIME_API_PROXY_UPSTREAM:-http://control-plane:8080}
  proxy_egress_network=${WORKER_RUNTIME_API_EGRESS_NETWORK:-kurbis-hoster_runtime-api-egress}
  proxy_image=${WORKER_RUNTIME_API_PROXY_IMAGE:-melon-hoster:local}
  bundle_path=$(worker_bundle_path "$worker_id")
  mkdir -p "$(dirname "$bundle_path")"
  local -a args=(--worker-id "$worker_id" --database-url "$database_url")
  local -a provision_env=(
    "WORKER_NAME=$WORKER_NAME"
    "WORKER_ADDRESS=$WORKER_ADDRESS"
    "WORKER_SIGNAL_ADDRESS=$WORKER_SIGNAL_ADDRESS"
    "WORKER_CONTROL_PLANE_URL=$control_plane_url"
    "WORKER_RUNTIME_API_PROXY_UPSTREAM=$proxy_upstream"
    "WORKER_RUNTIME_API_EGRESS_NETWORK=$proxy_egress_network"
    "WORKER_RUNTIME_API_PROXY_IMAGE=$proxy_image"
    "PUBLIC_BASE_URL=$PUBLIC_BASE_URL"
    "WORKER_RUNTIME=$runtime"
    "WORKER_STORAGE_MODE=$storage_mode"
    "WORKER_ROOT=${WORKER_ROOT:-./data/worker}"
    "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP=${WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP:-false}"
    "SNAPSHOT_INTERVAL_SECONDS=${SNAPSHOT_INTERVAL_SECONDS:-300}"
    "IDLE_TIMEOUT_SECONDS=${IDLE_TIMEOUT_SECONDS:-300}"
  )
  if [[ $mode == docker ]]; then
    local gateway_address=${WORKER_GATEWAY_ADDRESS:-${GATEWAY_IP:-}}
    [[ -n $gateway_address ]] || die "WORKER_GATEWAY_ADDRESS or GATEWAY_IP is required for Gateway-only workload ingress"
    provision_env+=("WORKER_GATEWAY_ADDRESS=$gateway_address")
    local gateway_bedrock_ip=${WORKER_GATEWAY_BEDROCK_IP:-${BEDROCK_PUBLIC_IP:-}}
    local gateway_bedrock_port=${WORKER_GATEWAY_BEDROCK_PORT:-${BEDROCK_PUBLIC_PORT:-${BEDROCK_PORT:-}}}
    [[ -n $gateway_bedrock_ip && -n $gateway_bedrock_port ]] || die "WORKER_GATEWAY_BEDROCK_IP/PORT or BEDROCK_PUBLIC_IP/PORT is required for Gateway-routed Bedrock ICE"
    provision_env+=("WORKER_GATEWAY_BEDROCK_IP=$gateway_bedrock_ip" "WORKER_GATEWAY_BEDROCK_PORT=$gateway_bedrock_port")
  fi
  for variable_name in WORKER_MEMORY_GIB WORKER_DISK_GIB WORKER_CPU_MILLIS WORKER_PORT_MIN WORKER_PORT_MAX; do
    if [[ -n ${!variable_name:-} ]]; then
      provision_env+=("$variable_name=${!variable_name}")
    fi
  done
  [[ -n $pumpkin_binary ]] && provision_env+=("PUMPKIN_BINARY=$pumpkin_binary")
  [[ -n $pumpkin_image ]] && provision_env+=("PUMPKIN_IMAGE=$pumpkin_image")
  [[ -n $plugin_wasm ]] && provision_env+=("KURBIS_CONTROL_PLUGIN_WASM=$plugin_wasm")
  if [[ $rotate == true ]]; then
    args+=(--rotate)
  fi

  # Bundle provisioning is an operator action. Give it only the database
  # credential it needs to register the worker plus build/runtime metadata;
  # control-plane object-store, OAuth, and internal credentials must not become
  # ambient inputs to worker provisioning.
  local -a clean_env=(env -i "PATH=${PATH:-/usr/local/bin:/usr/bin:/bin}" "HOME=${HOME:-/}")
  for variable_name in \
    LANG LC_ALL TMPDIR CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR RUSTC_WRAPPER \
    RUSTFLAGS RUSTUP_TOOLCHAIN SSL_CERT_FILE SSL_CERT_DIR; do
    if [[ -n ${!variable_name:-} ]]; then
      clean_env+=("$variable_name=${!variable_name}")
    fi
  done
  "${clean_env[@]}" "${provision_env[@]}" cargo run --quiet --bin worker-bundle -- \
    "${args[@]}" --output "$bundle_path"
}

ensure_worker_bundle() {
  local mode=$1 profile=${2:-release} path
  path=$(worker_bundle_path "$WORKER_ID")
  if [[ ! -f $path ]]; then
    provision_worker_bundle "$mode" "$WORKER_ID" false "$profile"
  fi
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

check_tmux() {
  require_command tmux
  if tmux has-session -t "$SESSION" 2>/dev/null; then
    tmux attach-session -t "$SESSION"
    exit 0
  fi
}

check_mode_switch() {
  local requested=$1 previous
  if [[ -f $MODE_FILE ]]; then
    previous=$(<"$MODE_FILE")
    [[ $previous == "$requested" ]] || die "stop the existing $previous deployment before switching to $requested"
  fi
}

check_build_dependencies() {
  require_command cargo
  require_command rustc
  require_command tmux
}

check_native_dependencies() {
  check_build_dependencies
  require_command git
  require_command cmake
  require_command node
  require_command pnpm
  require_command python3
  require_command psql
  require_command curl
  [[ -f $ROOT/web/pnpm-lock.yaml ]] || die "web/pnpm-lock.yaml is missing"
}

check_native_database() {
  if psql "$DATABASE_URL_NATIVE" -X -qAt -c 'SELECT 1' >/dev/null 2>&1; then
    return 0
  fi

  printf 'Native PostgreSQL is not ready; trying to provision its local role and database...\n'
  python3 "$ROOT/scripts/provision_native_database.py" \
    || die "could not provision the local PostgreSQL role/database; see the setup error above"
  psql "$DATABASE_URL_NATIVE" -X -qAt -c 'SELECT 1' >/dev/null \
    || die "local PostgreSQL setup completed, but DATABASE_URL_NATIVE still cannot connect"
}

check_docker_storage() {
  if [[ ${WORKER_STORAGE_MODE:-directory} == loopback_ext4 ]]; then
    [[ $(uname -s) == Linux ]] || die "loopback_ext4 worker storage requires a Linux host"
    [[ $(id -u) == 0 ]] || die "loopback_ext4 needs a root worker; use directory mode or start this deployment as root"
    require_command truncate
    require_command mkfs.ext4
    require_command mount
    require_command mountpoint
    require_command umount
  elif [[ ${WORKER_STORAGE_MODE:-directory} != directory ]]; then
    die "WORKER_STORAGE_MODE must be directory or loopback_ext4"
  fi
}

wait_for_http() {
  local url=$1 label=$2
  printf 'Waiting for %s' "$label"
  for _ in {1..90}; do
    if curl --silent --show-error --fail "$url" >/dev/null 2>&1; then
      printf ' ready\n'
      return 0
    fi
    printf '.'
    sleep 1
  done
  printf '\n'
  die "$label did not become ready; inspect its tmux pane"
}

wait_for_native_waiting() {
  local socket_path="/tmp/kurbis-waiting-${WORKER_ID%%-*}.sock"
  printf 'Waiting for authenticated waiting Pumpkin readiness'
  for _ in {1..60}; do
    if python3 - "$socket_path" "$PUMPKIN_GATEWAY_CONTROL_TOKEN" <<'PY' >/dev/null 2>&1
import json
import socket
import sys

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(1)
client.connect(sys.argv[1])
client.sendall((json.dumps({"token": sys.argv[2], "action": "status"}) + "\n").encode())
response = bytearray()
while b"\n" not in response:
    chunk = client.recv(4096)
    if not chunk:
        raise RuntimeError("waiting host-control closed without a response")
    response.extend(chunk)
status = json.loads(response.split(b"\n", 1)[0])
auth = status.get("online_authentication", {})
assert status.get("ok") and auth.get("java") and auth.get("bedrock")
PY
    then
      printf ' ready\n'
      return 0
    fi
    printf '.'
    sleep 1
  done
  printf '\n'
  die "waiting Pumpkin did not become ready; inspect its tmux pane"
}

create_dashboard() {
  local pane service last
  local -a services=("$@")
  local -a panes=()

  panes+=("$(tmux new-session -d -P -F '#{pane_id}' -s "$SESSION" -n services -c "$ROOT")")
  tmux set-option -t "$SESSION" mouse on
  tmux set-option -t "$SESSION" remain-on-exit on
  tmux set-window-option -t "$SESSION:services" pane-border-status top
  tmux set-window-option -t "$SESSION:services" pane-border-format '#{pane_title}'
  for service in "${services[@]:1}"; do
    last=${panes[${#panes[@]}-1]}
    panes+=("$(tmux split-window -d -P -F '#{pane_id}' -t "$last" -c "$ROOT")")
    # Rebalance after every split. Splitting the last pane repeatedly creates
    # a single shrinking column and can exhaust a normal terminal's height
    # before the final tiled layout is applied.
    tmux select-layout -t "$SESSION:services" tiled >/dev/null
  done

  for (( index=0; index<${#services[@]}; index++ )); do
    service=${services[index]}
    pane=${panes[index]}
    tmux select-pane -t "$pane" -T "$service"
  done
  tmux select-pane -t "${panes[0]}"
}

start_dashboard_service() {
  local mode=$1 service=$2 pane pane_title pane_command
  local -a service_args
  service_args=("$ROOT/scripts/run-service.sh" "$ENV_FILE" "$ROOT" "$mode" "$service")
  if [[ $service == worker ]]; then
    service_args+=("$(worker_bundle_path "$WORKER_ID")")
  fi
  while read -r pane pane_title; do
    if [[ $pane_title == "$service" ]]; then
      printf -v pane_command '%q ' "${service_args[@]}"
      tmux send-keys -t "$pane" "$pane_command" C-m
      return 0
    fi
  done < <(tmux list-panes -t "$SESSION:services" -F '#{pane_id} #{pane_title}')
  die "dashboard pane not found for $service"
}

attach_dashboard() {
  printf '\nDashboard keys: mouse or Ctrl-b then arrow keys to select a pane; Ctrl-b d detaches.\n'
  tmux attach-session -t "$SESSION"
}

start_native() {
  local profile=$1 mode="native-$1"
  local -a cargo_args=(build --bins)
  check_native_dependencies
  check_native_database
  [[ -x $ROOT/tools/build_pumpkin_native.sh ]] || die "tools/build_pumpkin_native.sh is not executable"

  printf 'Building native Pumpkin and Rust services (%s)...\n' "$profile"
  "$ROOT/tools/build_pumpkin_native.sh" "$profile"
  if [[ ! -d $ROOT/web/node_modules ]]; then
    printf 'Installing panel dependencies...\n'
    pnpm --dir "$ROOT/web" install --frozen-lockfile --ignore-scripts
  fi
  if [[ $profile == release ]]; then
    cargo_args+=(--release)
    printf 'Building the production panel...\n'
    python3 "$ROOT/tools/run_bounded_build.py" \
      --memory-bytes "${KURBIS_PANEL_BUILD_MEMORY_BYTES:-4294967296}" \
      -- bash -lc "cd '$ROOT/web' && CONTROL_PLANE_INTERNAL_URL=http://127.0.0.1:8080 pnpm run build"
  fi
  python3 "$ROOT/tools/run_bounded_build.py" \
    --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
    -- bash -lc "cd '$ROOT' && cargo ${cargo_args[*]} --jobs 1"

  mkdir -p "$ROOT/data/native-waiting"
  printf 'native\n' > "$MODE_FILE"
  create_dashboard control-plane waiting-pumpkin gateway worker panel
  start_dashboard_service "$mode" control-plane
  wait_for_http http://127.0.0.1:8080/healthz 'the control plane'
  ensure_worker_bundle native "$profile"
  start_dashboard_service "$mode" waiting-pumpkin
  wait_for_native_waiting
  start_dashboard_service "$mode" gateway
  start_dashboard_service "$mode" worker
  start_dashboard_service "$mode" panel
  attach_dashboard
}

start_docker() {
  check_build_dependencies
  [[ $(uname -s) == Linux ]] || die "Docker worker mode requires a Linux host; use native mode on macOS"
  require_command docker
  docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is required"
  docker info >/dev/null 2>&1 || die "Docker daemon is unavailable to this user"
  [[ -n ${WORKER_GATEWAY_ADDRESS:-${GATEWAY_IP:-}} ]] || die "set WORKER_GATEWAY_ADDRESS to the Gateway's reachable source IP"
  [[ -n ${WORKER_GATEWAY_BEDROCK_IP:-${BEDROCK_PUBLIC_IP:-}} ]] || die "set BEDROCK_PUBLIC_IP to the Gateway's public NetherNet/ICE endpoint"
  [[ -n ${WORKER_GATEWAY_BEDROCK_PORT:-${BEDROCK_PUBLIC_PORT:-${BEDROCK_PORT:-}}} ]] || die "set BEDROCK_PUBLIC_PORT to the Gateway's public NetherNet/ICE port"
  check_docker_storage
  case "$WORKER_SIGNAL_ADDRESS" in
    127.*|::1) die "WORKER_SIGNAL_ADDRESS must be a host-private address reachable by the gateway container" ;;
  esac

  printf 'Building the Docker Pumpkin runtime...\n'
  "$ROOT/tools/docker_build_with_limits.sh" \
    -f "$ROOT/runtime/pumpkin/Dockerfile" \
    --tag melon-pumpkin:host-control \
    "$ROOT"
  printf 'Building the Docker waiting runtime...\n'
  "$ROOT/tools/docker_build_with_limits.sh" \
    -f "$ROOT/deployment/waiting.Dockerfile" \
    --tag melon-pumpkin:waiting \
    "$ROOT"
  printf 'Building Compose services with per-step cgroup caps...\n'
  KURBIS_BUILD_MEMORY_BYTES="${KURBIS_DOCKER_BUILD_MEMORY_BYTES:-10737418240}" \
    "$ROOT/tools/docker_build_with_limits.sh" \
      --file "$ROOT/runtime/Dockerfile" \
      --tag melon-hoster:local \
      "$ROOT"
  KURBIS_BUILD_MEMORY_BYTES="${KURBIS_PANEL_BUILD_MEMORY_BYTES:-4294967296}" \
    "$ROOT/tools/docker_build_with_limits.sh" \
      --file "$ROOT/web/Dockerfile" \
      --tag kurbis-panel:local \
      "$ROOT/web"
  printf 'Building the host worker for Docker mode...\n'
  python3 "$ROOT/tools/run_bounded_build.py" \
    --memory-bytes "${KURBIS_NATIVE_BUILD_MEMORY_BYTES:-10737418240}" \
    -- bash -lc "cd '$ROOT' && cargo build --release --bin worker --jobs 1"
  mkdir -p "$ROOT/data"
  printf 'docker\n' > "$MODE_FILE"
  printf 'Starting Compose services...\n'
  if ! docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" up --no-build --detach --wait; then
    docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" logs --tail=100 || true
    die "Compose services did not become healthy"
  fi

  ensure_worker_bundle docker

  create_dashboard postgres control-plane-log waiting-pumpkin-log gateway-log panel-log worker
  for service in postgres control-plane-log waiting-pumpkin-log gateway-log panel-log worker; do
    start_dashboard_service docker "$service"
  done
  attach_dashboard
}

stop_stack() {
  local mode=
  if tmux has-session -t "$SESSION" 2>/dev/null; then
    tmux kill-session -t "$SESSION"
  fi
  if [[ -f $MODE_FILE ]]; then
    mode=$(<"$MODE_FILE")
    rm -f "$MODE_FILE"
  fi
  if [[ $mode == docker && -f $ENV_FILE ]] && command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" down --remove-orphans
    printf 'Stopped the Docker dashboard and Compose services. Persistent volumes were kept.\n'
  elif [[ $mode == native ]]; then
    printf 'Stopped the native process dashboard. PostgreSQL and deployment data were kept.\n'
  else
    printf 'Stopped the dashboard. Persistent data was kept.\n'
  fi
}

command_name=${1:-help}
case "$command_name" in
  help|-h|--help)
    usage
    ;;
  setup)
    setup_env
    ;;
  worker-bundle)
    setup_env
    load_env
    [[ $# -le 3 ]] || die "worker-bundle accepts an optional worker ID and --rotate"
    bundle_worker_id=$WORKER_ID
    bundle_rotate=false
    for argument in "${@:2}"; do
      case "$argument" in
        --rotate) bundle_rotate=true ;;
        *)
          [[ $bundle_worker_id == "$WORKER_ID" ]] || die "worker-bundle accepts only one worker ID"
          bundle_worker_id=$argument
          ;;
      esac
    done
    [[ $bundle_worker_id != GENERATE_ON_FIRST_START ]] || die "run ./start.sh setup first"
    bundle_mode=native
    if [[ -f $MODE_FILE ]]; then
      bundle_mode=$(<"$MODE_FILE")
    fi
    provision_worker_bundle "$bundle_mode" "$bundle_worker_id" "$bundle_rotate"
    ;;
  attach)
    require_command tmux
    tmux attach-session -t "$SESSION"
    ;;
  stop)
    stop_stack
    ;;
  native)
    profile=${2:-release}
    [[ $# -le 2 ]] || die "native accepts only an optional build profile"
    [[ $profile == debug || $profile == release ]] || die "native profile must be debug or release"
    setup_env
    load_env
    check_deployment_values native "$profile"
    check_mode_switch native
    check_tmux
    start_native "$profile"
    ;;
  docker)
    [[ $# == 1 ]] || die "docker takes no extra arguments"
    setup_env
    load_env
    check_deployment_values docker
    check_mode_switch docker
    check_tmux
    start_docker
    ;;
  *)
    usage >&2
    die "unknown command: $command_name"
    ;;
esac
