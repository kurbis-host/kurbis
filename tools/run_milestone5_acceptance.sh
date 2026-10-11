#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
: "${TEST_DATABASE_URL:?TEST_DATABASE_URL is required (use a disposable PostgreSQL database)}"

if [[ "${M5_LIVE_LINUX:-0}" == "1" ]]; then
  [[ "${CI_BUILDER_BACKEND:-}" == "bubblewrap_ssh" ]] || {
    echo "M5 live mode requires CI_BUILDER_BACKEND=bubblewrap_ssh" >&2; exit 2;
  }
  [[ -n "${CI_BUILDER_SSH_TARGET:-}" && "${CI_BUILDER_SSH_TARGET}" != root@* ]] || {
    echo "M5 live mode requires a non-root CI_BUILDER_SSH_TARGET" >&2; exit 2;
  }
  [[ -n "${CI_BUILDER_SSH_HOST_DIRECTORY:-}" && -d "${CI_BUILDER_SSH_HOST_DIRECTORY}" ]] || {
    echo "M5 live mode requires CI_BUILDER_SSH_HOST_DIRECTORY" >&2; exit 2;
  }
  [[ -n "${DOKPLOY_URL:-}" && -n "${DOKPLOY_API_KEY:-}" ]] || {
    echo "M5 live mode requires DOKPLOY_URL and DOKPLOY_API_KEY" >&2; exit 2;
  }
  python3 -m unittest discover -s "$root/tools" -p 'test_dokploy_worker_rollout.py'
  python3 -m unittest discover -s "$root/tools" -p 'test_dokploy_legacy_baseline.py'
fi

cd "$root"
TEST_DATABASE_URL="$TEST_DATABASE_URL" cargo test --locked --test milestone5_acceptance -- --nocapture
