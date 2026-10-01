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
  local name value
  for name in PUBLIC_BASE_URL ROOT_DOMAIN WORKER_ADDRESS \
    GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET \
    DISCORD_OAUTH_CLIENT_ID DISCORD_OAUTH_CLIENT_SECRET; do
    value=${!name:-}
    [[ -n $value ]] || die "$name is empty in .env"
    [[ $value != *replace-me* && $value != *REPLACE-ME* ]] || die "set $name in .env before starting"
  done
  [[ $PUBLIC_BASE_URL == https://* ]] || die "PUBLIC_BASE_URL must use https://"
  [[ $PUBLIC_BASE_URL != *example.com* ]] || die "replace the example PUBLIC_BASE_URL"
  [[ $ROOT_DOMAIN != *example.com* ]] || die "replace the example ROOT_DOMAIN"
  [[ $WORKER_ADDRESS != 203.0.113.10 ]] || die "set WORKER_ADDRESS to this worker's reachable address"
  for name in INTERNAL_TOKEN PUMPKIN_GATEWAY_CONTROL_TOKEN PUMPKIN_HOST_CONTROL_TOKEN WORKER_ID; do
    value=${!name:-}
    [[ -n $value && $value != GENERATE_ON_FIRST_START ]] || die "$name was not initialized; run ./start.sh setup"
  done
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
  require_command npm
  require_command python3
  require_command psql
  require_command curl
  [[ -f $ROOT/web/package-lock.json ]] || die "web/package-lock.json is missing"
}

check_native_database() {
  psql "$DATABASE_URL_NATIVE" -X -qAt -c 'SELECT 1' >/dev/null \
    || die "cannot connect to DATABASE_URL_NATIVE; create the PostgreSQL role/database and start PostgreSQL first"
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
  for service in "${services[@]:1}"; do
    last=${panes[${#panes[@]}-1]}
    panes+=("$(tmux split-window -d -P -F '#{pane_id}' -t "$last" -c "$ROOT")")
  done
  tmux set-option -t "$SESSION" mouse on
  tmux set-option -t "$SESSION" remain-on-exit on
  tmux set-window-option -t "$SESSION:services" pane-border-status top
  tmux set-window-option -t "$SESSION:services" pane-border-format '#{pane_title}'
  tmux select-layout -t "$SESSION:services" tiled >/dev/null

  for (( index=0; index<${#services[@]}; index++ )); do
    service=${services[index]}
    pane=${panes[index]}
    tmux select-pane -t "$pane" -T "$service"
  done
  tmux select-pane -t "${panes[0]}"
}

start_dashboard_service() {
  local mode=$1 service=$2 pane pane_title pane_command
  while read -r pane pane_title; do
    if [[ $pane_title == "$service" ]]; then
      printf -v pane_command '%q ' "$ROOT/scripts/run-service.sh" "$ENV_FILE" "$ROOT" "$mode" "$service"
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
    npm --prefix "$ROOT/web" ci
  fi
  if [[ $profile == release ]]; then
    cargo_args+=(--release)
    printf 'Building the production panel...\n'
    (cd "$ROOT/web" && CONTROL_PLANE_INTERNAL_URL=http://127.0.0.1:8080 npm run build)
  fi
  (cd "$ROOT" && cargo "${cargo_args[@]}")

  mkdir -p "$ROOT/data/native-waiting"
  printf 'native\n' > "$MODE_FILE"
  create_dashboard control-plane waiting-pumpkin gateway worker panel
  start_dashboard_service "$mode" control-plane
  wait_for_http http://127.0.0.1:8080/healthz 'the control plane'
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
  check_docker_storage

  printf 'Building the Docker Pumpkin runtime...\n'
  docker build -f "$ROOT/runtime/pumpkin/Dockerfile" -t melon-pumpkin:host-control "$ROOT"
  printf 'Building the Docker waiting runtime...\n'
  docker build -f "$ROOT/deployment/waiting.Dockerfile" -t melon-pumpkin:waiting "$ROOT"
  printf 'Building Compose services...\n'
  docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" build --parallel 1
  printf 'Building the host worker for Docker mode...\n'
  (cd "$ROOT" && cargo build --release --bin worker)
  mkdir -p "$ROOT/data"
  printf 'docker\n' > "$MODE_FILE"
  printf 'Starting Compose services...\n'
  if ! docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" up --detach --wait; then
    docker compose --env-file "$ENV_FILE" -f "$ROOT/compose.yaml" logs --tail=100 || true
    die "Compose services did not become healthy"
  fi

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
    check_deployment_values
    check_mode_switch native
    check_tmux
    start_native "$profile"
    ;;
  docker)
    [[ $# == 1 ]] || die "docker takes no extra arguments"
    setup_env
    load_env
    check_deployment_values
    check_mode_switch docker
    check_tmux
    start_docker
    ;;
  *)
    usage >&2
    die "unknown command: $command_name"
    ;;
esac
