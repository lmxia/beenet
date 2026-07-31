# Beenet

基于 CID 的分布式 Wasm 任务网络，采用 Spin / Wasmtime 风格的嵌入式宿主架构。

- **CID 即函数地址**：Wasm 二进制哈希就是调用地址。
- **P2P 即调用总线**：Gateway / Agent 通过 libp2p 调 Worker。
- **Wasm 即计算体**：毫秒级实例化、强隔离。

> 完整架构、决策记录与里程碑见 [`target.md`](./target.md)。

## 现状（M1 / M1.5）

最短闭环已端到端跑通：

```text
curl → Gateway(HTTP) → libp2p → Worker → wasi:http/incoming-handler@0.2 → body 回传
```

**必须** 运行 **`beenet-registry`**：Worker / Gateway 首次入网各用 admin 签发的 **join token**；之后用本地持久化 Ed25519 identity 签名 heartbeat（及 Gateway 的 lookup）。详见 [`target.md` §4.1 / §4.3](./target.md)。

**Wasm 构建与发布**由相邻的 **Beenet Cloud** 项目负责；本仓库保留 CID、manifest 和 Artifact 校验协议。Worker 配置 **`wasm_fetch_base`**，缓存未命中时按 CID 拉取产物。

档 0 接口走 W3C 标准 `wasi:http/incoming-handler@0.2`（`spin-sdk` + `#[http_component]`）。

## 先决条件

- Rust stable（`rust-toolchain.toml` 已固定）
- `wasm32-wasip2`：`rustup target add wasm32-wasip2`
- Docker（本地推荐用 compose 跑 Registry / Gateway / Dashboard；Artifact Store 由 Beenet Cloud 提供）

## 本地一键栈（推荐）

控制面与 Gateway **容器化**；Worker **宿主机进程**（依赖 Spin 宿主，暂不容器化）。
MinIO/未来 OSS 属于发布平台，因此统一由相邻的 **Beenet Cloud** 管理，Beenet 不再启动第二套
对象存储。

Gateway 需要 Registry 先签发 **gateway join token**，因此不要直接 `docker compose up` 全部服务，请用分阶段脚本：

```bash
# 首次启动共享 Artifact Store
make -C ../beenet-cloud storage-up

# 首次或镜像有变更时加 --build（使用本机已有基础镜像，避免 Docker Hub 超时）
./scripts/dev-up.sh up --build
# 仅启动 / 刷新 token（不重建镜像）
./scripts/dev-up.sh up
```

脚本顺序：

1. 启动 Redis / Registry / Dashboard，并检查 Beenet Cloud Artifact Store
2. 签发 gateway join token → `.beenet-dev/gateway-join-token`  
3. 启动 Gateway（挂载该 token）  
4. 签发 worker join token → `.beenet-dev/worker-join-token`  

| 地址 | 说明 |
| --- | --- |
| http://127.0.0.1:3030 | Registry |
| http://127.0.0.1:18080 | Gateway HTTP（libp2p `14001`） |
| http://127.0.0.1:8081 | Dashboard（admin：`beenet-dev-admin-token`） |
| http://127.0.0.1:9000 | Beenet Cloud MinIO S3（控制台 `:9001`，`minioadmin` / `minioadmin`） |

常用命令：

```bash
./scripts/dev-up.sh status
./scripts/dev-up.sh logs
./scripts/dev-up.sh worker-token   # 刷新 worker join token
./scripts/dev-up.sh down
make -C ../beenet-cloud storage-up
# 或: make docker-up / make docker-down
```

宿主机启动 Worker：

```bash
cargo build --release -p beenet-worker

./target/release/beenet-worker \
  --config examples/local-dev-config.toml \
  --join-token-file .beenet-dev/worker-join-token
```

首次 join 成功后可删除临时 token 文件；重启复用 `wasm_cache_dir/identity.key`。`--join-token-stdin` 亦可；避免把明文写进 shell history。

## 配置文件

**Gateway / Worker** 读 TOML；Beenet Cloud 的 **`beenet-pack upload`** 也可复用其中的
`[oss]` 发布配置；**`beenet-registry`** 只用 CLI。

- 默认：`dirs::config_dir()/beenet/config.toml`
- 覆盖：`--config /path/to/config.toml`
- 本地联调可直接用 **`examples/local-dev-config.toml`**。

```toml
[worker]
registry_url = "http://127.0.0.1:3030"
wasm_fetch_base = "http://127.0.0.1:9000/beenet"

[oss]
endpoint = "http://127.0.0.1:9000"
bucket = "beenet"
access_key_id = "minioadmin"
access_key_secret = "minioadmin"
region = "us-east-1"
force_path_style = true
```

## 端到端示例

假定仓库根目录；Beenet Cloud Artifact Store 与本地运行时栈已经拉起：

```bash
make -C ../beenet-cloud storage-up
./scripts/dev-up.sh up
```

### 1. 通过 Beenet Cloud Builder 编译与打包

```bash
mkdir -p dist
make -C ../beenet-cloud builder-image
make -C ../beenet-cloud wasm \
  DIR=../beenet/examples/fair-red-packet-http \
  OUT=../beenet/dist
```

### 2. 通过 `beenet-pack upload` 发布到 MinIO

```bash
make -C ../beenet-cloud upload \
  WASM=../beenet/dist/task.wasm \
  CONFIG=../beenet/examples/local-dev-config.toml

export CID="$(awk '/^CID:/{print $2}' dist/build-result.txt)"
curl -I "http://127.0.0.1:9000/beenet/$CID"
```

`make upload` 会启动一个独立、可联网的容器，并在其中执行 `beenet-pack upload`。
Artifact 的对象 key 是 CID 本身，不带 `.wasm` 后缀；Builder 容器仍使用
`--network none`，不会接触 MinIO/OSS 的 AK/SK。

生产环境不需要 Agent 手动上传：Beenet Cloud API 会在编译完成并重新校验 CID 后自动
上传 OSS，验证对象存在后才将 Build Job 标记为 `succeeded/published`。

### 3. 启动 Worker 并调用

```bash
./target/release/beenet-worker \
  --config examples/local-dev-config.toml \
  --join-token-file .beenet-dev/worker-join-token

curl -i -X POST "http://127.0.0.1:18080/run/ipfs/$CID" \
  -H 'content-type: application/json' \
  --data '{
    "total_yuan": "100.00",
    "participants": ["小明", "小红", "老王", "翠花"],
    "public_seed": "2026春晚-第一个节目"
  }'
```

预期：`HTTP/1.1 200`、`x-beenet-status: ok`，并返回可复算的拼手气结果：

```json
{
  "total_yuan": "100.00",
  "allocations": [
    { "name": "小明", "amount_yuan": "24.09" },
    { "name": "小红", "amount_yuan": "27.32" },
    { "name": "老王", "amount_yuan": "18.96" },
    { "name": "翠花", "amount_yuan": "29.63" }
  ],
  "lucky_winner": "翠花",
  "public_seed": "2026春晚-第一个节目",
  "algorithm": "fair-red-packet/sha256-weighted-v1",
  "draw_id": "c6d60e1c03b375b7237d46704474c0a647614b40be4e66bf60aebf7103888e04"
}
```

这个示例是一台“公平红包公证器”：金额按整数分计算，确保每人至少一分钱且总额精确守恒；
公开种子、参与者顺序和规则 CID 相同，任何 Worker 都会算出完全相同的结果。修改种子会改变
`draw_id` 和分配结果，修改算法则会改变 CID。因此任何人都能复算，而平台无法悄悄更换规则。
实际使用时应在报名结束前约定一个事后才能确定的公开种子（例如某个公开事件结果），避免发起人
反复试种子挑选对自己有利的结果。

确认入网：

```bash
curl -s -H "Authorization: Bearer beenet-dev-admin-token" \
  http://127.0.0.1:3030/v1/dashboard/status | jq .
```

### 故障排查（简）

- **connection refused**：`./scripts/dev-up.sh status`，确认 Gateway `18080` 与 Worker 在跑。
- **load-error**：Wasm 未上传到 `{wasm_fetch_base}/{CID}`、对象名错误地带了 `.wasm`，或本地缓存目录不可写。
- **Gateway 起不来**：是否跳过了 `dev-up.sh`、直接 compose up？需先有 `.beenet-dev/gateway-join-token`。

## 写你自己的任务

1. 新建 `wasm32-wasip2` `cdylib`，`spin-sdk` + `#[http_component]`。  
2. 写 `beenet.toml`（`interface` = `wasi:http/incoming-handler@0.2`）。  
3. 交给 Beenet Cloud 构建、发布，再经鉴权后的 Invoke Proxy 调用 CID。

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `crates/beenet-common` | `BeenetCid`、协议常量、Invoke 请求/响应类型 |
| `crates/beenet-artifact` | `beenet:manifest/v1`、package / inspect / CID 校验 |
| `crates/beenet-registry` | HTTP 控制面（join / heartbeat / lookup） |
| `crates/beenet-worker` | libp2p Worker（宿主机） |
| `crates/beenet-gateway` | HTTP → libp2p Gateway |
| `scripts/dev-up.sh` | 本地分阶段 Docker 启动 |
| `docker/` | Dockerfile + `docker-compose.dev.yml` |
| `examples/fair-red-packet-http` | 可复算的公平拼手气红包（推荐端到端示例） |
| `examples/checkout-risk-http` | 结构化交易风控任务 |
| `examples/hello-filter-http` | 最小 HTTP 组件样板 |
| `examples/local-dev-config.toml` | 本地 Worker 与对象存储配置 |
| `Makefile` | `make docker-up` 等 |

## 许可

见 [`LICENSE`](./LICENSE)。
