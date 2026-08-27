#!/usr/bin/env bash
# Install bworker onto PATH, write ~/.config/beenet/config.toml, then run it.
#
#   curl -fsSL -o get-bworker.sh \
#     https://github.com/lmxia/beenet/releases/latest/download/get-bworker.sh
#   chmod +x get-bworker.sh
#   ./get-bworker.sh --join-token-file ./join-token
#
# Later:
#   bworker
set -euo pipefail

REPO="${BEENET_REPO:-lmxia/beenet}"
VERSION="${BEENET_VERSION:-latest}"
PREFIX="${BEENET_PREFIX:-}"
INSTALL_ONLY=0
WORKER_ARGS=()

# Keep in sync with macOS WorkerConfigSnapshot and beenet-common Linux defaults.
DEFAULT_REGISTRY_URL="http://registry.hyperos.online"
DEFAULT_WASM_FETCH_BASE="http://cloud.hyperos.online/api/v1/artifacts"
DEFAULT_LISTEN_ADDR="/ip4/0.0.0.0/tcp/0"
DEFAULT_HEARTBEAT_SECS="30"
DEFAULT_WASM_FETCH_TIMEOUT_SECS="60"
DEFAULT_CPU_PERCENT="25"
DEFAULT_MEMORY_MB="512"
DEFAULT_PIDS_MAX="128"

CONFIG="${BEENET_CONFIG:-}"
CACHE=""
REGISTRY_URL=""
WASM_FETCH_BASE=""
LISTEN_ADDR=""
REGION="${BEENET_REGION:-}"
NAME="${BEENET_WORKER_NAME:-}"
HEARTBEAT_SECS=""
WASM_FETCH_TIMEOUT_SECS=""
CPU_PERCENT=""
MEMORY_MB=""
PIDS_MAX=""
NICE=""

usage() {
  cat <<EOF
Usage: $(basename "$0") [--join-token-file PATH] [bworker args...]

Downloads the Linux beenet-worker release, installs beenet-worker and the
bworker alias onto PATH, writes a default config, then runs bworker.

Config path (fixed unless --config is passed):
  \${XDG_CONFIG_HOME:-\$HOME/.config}/beenet/config.toml

Defaults (CLI flags override; existing config values are kept unless overridden):
  registry_url              ${DEFAULT_REGISTRY_URL}
  wasm_fetch_base           ${DEFAULT_WASM_FETCH_BASE}
  listen_addr               ${DEFAULT_LISTEN_ADDR}
  wasm_cache_dir            \${XDG_DATA_HOME:-\$HOME/.local/share}/beenet/wasm_cache
  --quota-cpu-percent       ${DEFAULT_CPU_PERCENT}
  --quota-memory-mb         ${DEFAULT_MEMORY_MB}
  --quota-pids-max          ${DEFAULT_PIDS_MAX}
  --name NAME               display name (default: Docker-style auto name)
  --region REGION           registry affinity, e.g. cn-hongkong (default: empty)

  --join-token-file PATH    first-run enroll token (also accepts -join-token-file)
  --prefix DIR              install dir (default: /usr/local/bin, else ~/.local/bin)
  --version TAG             release tag (default: latest)
  --install-only            install and write config, do not start
  -h, --help

Environment:
  BEENET_REPO, BEENET_VERSION, BEENET_PREFIX, BEENET_CONFIG,
  BEENET_WORKER_NAME, BEENET_REGION
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

abspath() {
  local p="$1"
  if [[ "$p" == /* ]]; then
    printf '%s\n' "$p"
  else
    printf '%s\n' "$(pwd)/$p"
  fi
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

download() {
  local url="$1" dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-delay 1 -o "$dest" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$dest" "$url"
  else
    die "need curl or wget to download $url"
  fi
}

asset_url() {
  local name="$1"
  if [[ "${VERSION}" == "latest" ]]; then
    printf 'https://github.com/%s/releases/latest/download/%s\n' "${REPO}" "${name}"
  else
    printf 'https://github.com/%s/releases/download/%s/%s\n' "${REPO}" "${VERSION}" "${name}"
  fi
}

run_privileged() {
  local dest_dir="$1"
  shift
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
    return
  fi
  if [[ -d "${dest_dir}" && -w "${dest_dir}" ]]; then
    "$@"
    return
  fi
  local parent
  parent="$(dirname "${dest_dir}")"
  if [[ ! -e "${dest_dir}" && -d "${parent}" && -w "${parent}" ]]; then
    "$@"
    return
  fi
  need_cmd sudo
  sudo "$@"
}

persist_path() {
  local prefix="$1"
  case ":${PATH}:" in
    *":${prefix}:"*) return 0 ;;
  esac
  export PATH="${prefix}:${PATH}"
  local marker="# beenet bworker"
  local line="export PATH=\"${prefix}:\$PATH\""
  local rc
  for rc in "${HOME}/.bashrc" "${HOME}/.profile" "${HOME}/.zshrc"; do
    if [[ -f "$rc" ]] && grep -Fq "${marker}" "$rc"; then
      return 0
    fi
  done
  rc="${HOME}/.profile"
  [[ -f "${HOME}/.bashrc" ]] && rc="${HOME}/.bashrc"
  {
    echo ""
    echo "${marker}"
    echo "${line}"
  } >>"$rc"
  echo "added ${prefix} to PATH in ${rc} (open a new shell if bworker is not found)"
}

toml_get() {
  local file="$1" key="$2"
  [[ -f "$file" ]] || return 0
  awk -F '"' -v k="$key" '
    $0 ~ "^[[:space:]]*" k "[[:space:]]*=" {
      if (NF >= 2) { print $2; exit }
    }' "$file"
}

toml_get_num() {
  local file="$1" key="$2"
  [[ -f "$file" ]] || return 0
  awk -v k="$key" '
    $0 ~ "^[[:space:]]*" k "[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/, "")
      gsub(/[^0-9-].*/, "")
      print
      exit
    }' "$file"
}

toml_escape() {
  local s="$1"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

pick() {
  local cli="$1" existing="$2" default="$3"
  if [[ -n "$cli" ]]; then
    printf '%s' "$cli"
  elif [[ -n "$existing" ]]; then
    printf '%s' "$existing"
  else
    printf '%s' "$default"
  fi
}

write_file() {
  local dest="$1"
  local dir
  dir="$(dirname "$dest")"
  run_privileged "$dir" mkdir -p "$dir"
  if [[ -w "$dir" ]]; then
    cat >"$dest"
    chmod 0644 "$dest" 2>/dev/null || true
  else
    need_cmd sudo
    sudo tee "$dest" >/dev/null
    sudo chmod 0644 "$dest"
  fi
}

write_worker_config() {
  local existing="${CONFIG}"
  local registry cache listen fetch heartbeat timeout cpu mem pids nice name region

  registry="$(pick "${REGISTRY_URL}" "$(toml_get "$existing" registry_url)" "${DEFAULT_REGISTRY_URL}")"
  cache="$(pick "${CACHE}" "$(toml_get "$existing" wasm_cache_dir)" "${XDG_DATA_HOME:-$HOME/.local/share}/beenet/wasm_cache")"
  listen="$(pick "${LISTEN_ADDR}" "$(toml_get "$existing" listen_addr)" "${DEFAULT_LISTEN_ADDR}")"
  fetch="$(pick "${WASM_FETCH_BASE}" "$(toml_get "$existing" wasm_fetch_base)" "${DEFAULT_WASM_FETCH_BASE}")"
  heartbeat="$(pick "${HEARTBEAT_SECS}" "$(toml_get_num "$existing" registry_heartbeat_secs)" "${DEFAULT_HEARTBEAT_SECS}")"
  timeout="$(pick "${WASM_FETCH_TIMEOUT_SECS}" "$(toml_get_num "$existing" wasm_fetch_timeout_secs)" "${DEFAULT_WASM_FETCH_TIMEOUT_SECS}")"
  cpu="$(pick "${CPU_PERCENT}" "$(toml_get_num "$existing" cpu_percent)" "${DEFAULT_CPU_PERCENT}")"
  mem="$(pick "${MEMORY_MB}" "$(toml_get_num "$existing" memory_mb)" "${DEFAULT_MEMORY_MB}")"
  pids="$(pick "${PIDS_MAX}" "$(toml_get_num "$existing" pids_max)" "${DEFAULT_PIDS_MAX}")"
  nice="$(pick "${NICE}" "$(toml_get_num "$existing" nice)" "")"
  name="$(pick "${NAME}" "$(toml_get "$existing" name)" "")"
  region="$(pick "${REGION}" "$(toml_get "$existing" region)" "")"
  CACHE="$cache"

  {
    echo "[worker]"
    echo "backend = \"native\""
    echo "listen_addr = \"$(toml_escape "$listen")\""
    echo "registry_url = \"$(toml_escape "$registry")\""
    echo "wasm_fetch_base = \"$(toml_escape "$fetch")\""
    echo "wasm_fetch_timeout_secs = ${timeout}"
    echo "registry_heartbeat_secs = ${heartbeat}"
    echo "wasm_cache_dir = \"$(toml_escape "$cache")\""
    if [[ -n "$name" ]]; then
      echo "name = \"$(toml_escape "$name")\""
    fi
    if [[ -n "$region" ]]; then
      echo "region = \"$(toml_escape "$region")\""
    fi
    echo
    echo "[worker.quota]"
    echo "cpu_percent = ${cpu}"
    echo "memory_mb = ${mem}"
    echo "pids_max = ${pids}"
    if [[ -n "$nice" ]]; then
      echo "nice = ${nice}"
    fi
  } | write_file "$CONFIG"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --install-only)
      INSTALL_ONLY=1
      shift
      ;;
    --prefix)
      PREFIX="${2:?--prefix requires a directory}"
      shift 2
      ;;
    --prefix=*)
      PREFIX="${1#*=}"
      shift
      ;;
    --version)
      VERSION="${2:?--version requires a tag}"
      shift 2
      ;;
    --version=*)
      VERSION="${1#*=}"
      shift
      ;;
    --config)
      CONFIG="$(abspath "${2:?--config requires a path}")"
      shift 2
      ;;
    --config=*)
      CONFIG="$(abspath "${1#*=}")"
      shift
      ;;
    -join-token-file|--join-token-file)
      token_file="$(abspath "${2:?--join-token-file requires a path}")"
      [[ -f "${token_file}" ]] || die "join token file not found: ${token_file}"
      WORKER_ARGS+=(--join-token-file "${token_file}")
      shift 2
      ;;
    -join-token-file=*|--join-token-file=*)
      token_file="$(abspath "${1#*=}")"
      [[ -f "${token_file}" ]] || die "join token file not found: ${token_file}"
      WORKER_ARGS+=(--join-token-file "${token_file}")
      shift
      ;;
    --registry-url)
      REGISTRY_URL="${2:?--registry-url requires a URL}"
      shift 2
      ;;
    --registry-url=*)
      REGISTRY_URL="${1#*=}"
      shift
      ;;
    --wasm-fetch-base)
      WASM_FETCH_BASE="${2:?--wasm-fetch-base requires a URL}"
      shift 2
      ;;
    --wasm-fetch-base=*)
      WASM_FETCH_BASE="${1#*=}"
      shift
      ;;
    --wasm-cache-dir)
      CACHE="$(abspath "${2:?--wasm-cache-dir requires a path}")"
      shift 2
      ;;
    --wasm-cache-dir=*)
      CACHE="$(abspath "${1#*=}")"
      shift
      ;;
    --listen-addr)
      LISTEN_ADDR="${2:?--listen-addr requires a multiaddr}"
      shift 2
      ;;
    --listen-addr=*)
      LISTEN_ADDR="${1#*=}"
      shift
      ;;
    --region)
      REGION="${2:?--region requires a value}"
      shift 2
      ;;
    --region=*)
      REGION="${1#*=}"
      shift
      ;;
    --name)
      NAME="${2:?--name requires a value}"
      shift 2
      ;;
    --name=*)
      NAME="${1#*=}"
      shift
      ;;
    --registry-heartbeat-secs)
      HEARTBEAT_SECS="${2:?--registry-heartbeat-secs requires a value}"
      shift 2
      ;;
    --registry-heartbeat-secs=*)
      HEARTBEAT_SECS="${1#*=}"
      shift
      ;;
    --wasm-fetch-timeout-secs)
      WASM_FETCH_TIMEOUT_SECS="${2:?--wasm-fetch-timeout-secs requires a value}"
      shift 2
      ;;
    --wasm-fetch-timeout-secs=*)
      WASM_FETCH_TIMEOUT_SECS="${1#*=}"
      shift
      ;;
    --quota-cpu-percent)
      CPU_PERCENT="${2:?--quota-cpu-percent requires a value}"
      shift 2
      ;;
    --quota-cpu-percent=*)
      CPU_PERCENT="${1#*=}"
      shift
      ;;
    --quota-memory-mb)
      MEMORY_MB="${2:?--quota-memory-mb requires a value}"
      shift 2
      ;;
    --quota-memory-mb=*)
      MEMORY_MB="${1#*=}"
      shift
      ;;
    --quota-pids-max)
      PIDS_MAX="${2:?--quota-pids-max requires a value}"
      shift 2
      ;;
    --quota-pids-max=*)
      PIDS_MAX="${1#*=}"
      shift
      ;;
    --quota-nice)
      NICE="${2:?--quota-nice requires a value}"
      shift 2
      ;;
    --quota-nice=*)
      NICE="${1#*=}"
      shift
      ;;
    *)
      WORKER_ARGS+=("$1")
      shift
      ;;
  esac
done

[[ "$(uname -s)" == "Linux" ]] || die "this installer is for Linux; macOS contributors should use the DMG"

arch="$(uname -m)"
case "${arch}" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) die "unsupported architecture: ${arch}" ;;
esac

if [[ -z "${PREFIX}" ]]; then
  if [[ "$(id -u)" -eq 0 ]] || [[ -w /usr/local/bin ]] || command -v sudo >/dev/null 2>&1; then
    PREFIX="/usr/local/bin"
  else
    PREFIX="${HOME}/.local/bin"
  fi
fi
if [[ "${PREFIX}" == "${HOME}/"* || "${PREFIX}" == "${HOME}" ]]; then
  mkdir -p "${PREFIX}"
fi

if [[ -z "${CONFIG}" ]]; then
  CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/beenet/config.toml"
fi

tarball_name="beenet-worker-linux-${arch}.tar.gz"
url="$(asset_url "${tarball_name}")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

need_cmd tar
echo "downloading ${url}"
download "${url}" "${tmp}/${tarball_name}"
tar -xzf "${tmp}/${tarball_name}" -C "${tmp}"
src="$(find "${tmp}" -type f -name beenet-worker -print -quit)"
[[ -n "${src}" ]] || die "archive did not contain beenet-worker"
chmod +x "${src}"

run_privileged "${PREFIX}" install -d "${PREFIX}"
run_privileged "${PREFIX}" install -m 0755 "${src}" "${PREFIX}/beenet-worker"
run_privileged "${PREFIX}" ln -sfn beenet-worker "${PREFIX}/bworker"
persist_path "${PREFIX}"

echo "installed ${PREFIX}/beenet-worker"
echo "alias     ${PREFIX}/bworker"

write_worker_config
echo "config    ${CONFIG}"

if [[ "${INSTALL_ONLY}" -eq 1 ]]; then
  echo "later runs: bworker"
  exit 0
fi

hash -r 2>/dev/null || true
"${PREFIX}/bworker" --config "${CONFIG}" --wasm-cache-dir "${CACHE}" "${WORKER_ARGS[@]}"
