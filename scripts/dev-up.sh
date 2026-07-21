#!/usr/bin/env bash
# Phased local stack: Docker services + host Worker.
#
# Order:
#   1) redis / minio / registry / dashboard
#   2) mint gateway join token
#   3) gateway (with token file mounted)
#   4) mint worker join token for host process
#
# Usage (from repo root):
#   ./scripts/dev-up.sh up [--build]
#   ./scripts/dev-up.sh down
#   ./scripts/dev-up.sh status
#   ./scripts/dev-up.sh worker-token   # refresh worker join token only
#   ./scripts/dev-up.sh logs [service]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

COMPOSE=(docker compose -f docker/docker-compose.dev.yml)
ADMIN_TOKEN="${BEENET_ADMIN_TOKEN:-beenet-dev-admin-token}"
SECRET_DIR="${BEENET_DEV_DIR:-$ROOT/.beenet-dev}"
GW_TOKEN_FILE="$SECRET_DIR/gateway-join-token"
WORKER_TOKEN_FILE="$SECRET_DIR/worker-join-token"
REGISTRY_URL="${BEENET_REGISTRY_URL:-http://127.0.0.1:3030}"
GATEWAY_HEALTH_URL="${BEENET_GATEWAY_HEALTH_URL:-http://127.0.0.1:18080/health}"

mkdir -p "$SECRET_DIR"
chmod 700 "$SECRET_DIR" 2>/dev/null || true

usage() {
  cat <<EOF
Usage: $0 <command> [options]

Commands:
  up [--build]   Phased start of Docker stack; print host worker command
  down           Stop compose stack (keeps volumes)
  status         Show registry dashboard snapshot + compose ps
  worker-token   Mint a fresh worker join token into $WORKER_TOKEN_FILE
  logs [svc]     Tail compose logs (default: beenet-registry beenet-gateway)

Env:
  BEENET_ADMIN_TOKEN   default: beenet-dev-admin-token
  BEENET_DEV_DIR       default: <repo>/.beenet-dev
EOF
}

wait_http() {
  local url=$1
  local name=$2
  local n=${3:-60}
  for _ in $(seq 1 "$n"); do
    if curl -sf "$url" >/dev/null; then
      echo "ok: $name ($url)"
      return 0
    fi
    sleep 0.5
  done
  echo "timeout waiting for $name at $url" >&2
  return 1
}

mint_token() {
  local path=$1
  local description=$2
  local out=$3
  local value
  value=$(curl -sS -X POST "${REGISTRY_URL}${path}" \
    -H "Authorization: Bearer ${ADMIN_TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{\"description\":\"${description}\",\"ttl_secs\":3600}" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["token_value"])')
  printf '%s\n' "$value" >"$out"
  chmod 600 "$out"
  echo "wrote $out"
}

cmd_up() {
  local build=0
  if [[ "${1:-}" == "--build" ]]; then
    build=1
    shift || true
  fi

  echo "==> phase 1: redis / minio / registry / dashboard"
  if [[ "$build" == "1" ]]; then
    # compose 默认可能走 buildx container builder，且会强制访问 Docker Hub
    # 鉴权；本机已有基础镜像时用 default builder + --pull=false 更稳。
    echo "building images with docker build --pull=false (builder=default)..."
    docker buildx use default >/dev/null 2>&1 || true
    (
      # Avoid broken compose-proxy defaults like host.docker.internal inside builders.
      unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy ALL_PROXY all_proxy || true
      docker build --pull=false -f docker/Dockerfile.registry -t beenet/beenet-registry:dev .
      docker build --pull=false -f docker/Dockerfile.gateway -t beenet/beenet-gateway:dev .
      docker build --pull=false -f docker/Dockerfile.dashboard -t beenet/beenet-dashboard:dev .
    )
  fi
  "${COMPOSE[@]}" up -d redis minio minio-init beenet-registry beenet-dashboard
  wait_http "${REGISTRY_URL}/health" "registry"

  echo "==> phase 2: mint gateway join token"
  mint_token "/v1/admin/gateway-tokens" "compose-gateway" "$GW_TOKEN_FILE"

  echo "==> phase 3: gateway"
  # Compose mounts ../.beenet-dev/gateway-join-token (see docker-compose.dev.yml).
  "${COMPOSE[@]}" up -d --no-build beenet-gateway
  wait_http "$GATEWAY_HEALTH_URL" "gateway"

  echo "==> phase 4: mint worker join token (host process)"
  mint_token "/v1/admin/tokens" "compose-worker" "$WORKER_TOKEN_FILE"

  cat <<EOF

Stack is up.

  Registry:   ${REGISTRY_URL}
  Gateway:    http://127.0.0.1:18080
  Dashboard:  http://127.0.0.1:8081  (admin: ${ADMIN_TOKEN})
  MinIO:      http://127.0.0.1:9000  (console :9001)

Gateway join token:  ${GW_TOKEN_FILE}
Worker join token:   ${WORKER_TOKEN_FILE}

Start Worker on the host (after cargo build --release -p beenet-worker):

  ./target/release/beenet-worker \\
    --config examples/local-dev-config.toml \\
    --join-token-file ${WORKER_TOKEN_FILE}

Invoke via Docker Gateway:

  curl -i -X POST "http://127.0.0.1:18080/run/ipfs/\$CID" --data 'hello'

Useful:
  $0 status
  $0 logs
  $0 down
EOF
}

cmd_down() {
  "${COMPOSE[@]}" down
  echo "compose stack stopped (volumes retained; secrets kept in $SECRET_DIR)"
}

cmd_status() {
  "${COMPOSE[@]}" ps
  echo
  curl -sS -H "Authorization: Bearer ${ADMIN_TOKEN}" \
    "${REGISTRY_URL}/v1/dashboard/status" | python3 -m json.tool || true
}

cmd_worker_token() {
  wait_http "${REGISTRY_URL}/health" "registry" 20
  mint_token "/v1/admin/tokens" "compose-worker" "$WORKER_TOKEN_FILE"
  echo "use: --join-token-file $WORKER_TOKEN_FILE"
}

cmd_logs() {
  if [[ $# -gt 0 ]]; then
    "${COMPOSE[@]}" logs -f "$@"
  else
    "${COMPOSE[@]}" logs -f beenet-registry beenet-gateway
  fi
}

main() {
  local cmd=${1:-}
  shift || true
  case "$cmd" in
    up) cmd_up "$@" ;;
    down) cmd_down ;;
    status) cmd_status ;;
    worker-token) cmd_worker_token ;;
    logs) cmd_logs "$@" ;;
    -h|--help|help|"") usage ;;
    *)
      echo "unknown command: $cmd" >&2
      usage >&2
      exit 1
      ;;
  esac
}

main "$@"
